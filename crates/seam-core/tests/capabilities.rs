//! Integration tests run as a downstream crate, so they also check that the
//! blanket impls leave room for user implementations.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use seam_core::future::{AsyncPerform, SendPerform, block_on};
use seam_core::{
    AsyncScoped, Finalize, Inspect, Operation, Perform, Push, Query, Rejected, Scoped,
};

struct Get;
impl Operation for Get {
    type Outcome = u32;
}
impl Query for Get {}

struct Set(u32);
impl Operation for Set {
    type Outcome = ();
}

#[derive(Default)]
struct Cell32(u32);

impl Inspect<Get> for Cell32 {
    fn inspect(&self, _: Get) -> u32 {
        self.0
    }
}

impl Perform<Set> for Cell32 {
    fn perform(&mut self, op: Set) {
        self.0 = op.0;
    }
}

fn read_only<C: Inspect<Get>>(c: &C) -> u32 {
    c.inspect(Get)
}

/// Asks for `Perform<Get>`; an `Inspect<Get>` value satisfies it.
fn increment<C: Perform<Get> + Perform<Set>>(c: &mut C) {
    let n = c.perform(Get);
    c.perform(Set(n + 1));
}

#[test]
fn inspect_is_accepted_where_perform_is_required() {
    let mut c = Cell32(41);
    increment(&mut c);
    assert_eq!(read_only(&c), 42);
}

#[test]
fn sync_capabilities_lift_to_async() {
    async fn increment_async<C: AsyncPerform<Get> + AsyncPerform<Set>>(c: &mut C) {
        let n = c.perform_async(Get).await;
        c.perform_async(Set(n + 1)).await;
    }

    let mut c = Cell32(1);
    block_on(increment_async(&mut c));
    assert_eq!(c.0, 2);
}

#[test]
fn send_futures_cross_threads() {
    async fn increment_send<C: SendPerform<Get> + SendPerform<Set> + Send>(c: &mut C) {
        let n = c.perform_send(Get).await;
        c.perform_send(Set(n + 1)).await;
    }

    let mut c = Cell32(7);
    let fut = increment_send(&mut c);
    std::thread::scope(|s| s.spawn(move || block_on(fut)).join().unwrap());
    assert_eq!(c.0, 8);
}

#[test]
fn block_on_repolls_cooperative_yields() {
    struct YieldOnce(bool);
    impl Future for YieldOnce {
        type Output = u8;
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<u8> {
            if std::mem::replace(&mut self.0, true) {
                std::task::Poll::Ready(5)
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }
    }
    assert_eq!(block_on(YieldOnce(false)), 5);
}

/// With std, a future woken from another thread parks `block_on` until then.
#[cfg(feature = "std")]
#[test]
fn block_on_parks_until_woken() {
    use std::sync::{Arc, Mutex};
    use std::task::{Poll, Waker};
    use std::time::Duration;

    #[derive(Default)]
    struct Slot {
        value: Option<u8>,
        waker: Option<Waker>,
    }

    struct Wait(Arc<Mutex<Slot>>);
    impl Future for Wait {
        type Output = u8;
        fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<u8> {
            let mut slot = self.0.lock().unwrap();
            match slot.value {
                Some(v) => Poll::Ready(v),
                None => {
                    slot.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }

    let slot = Arc::new(Mutex::new(Slot::default()));
    let setter = Arc::clone(&slot);
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let mut slot = setter.lock().unwrap();
        slot.value = Some(9);
        if let Some(w) = slot.waker.take() {
            w.wake();
        }
    });
    assert_eq!(block_on(Wait(slot)), 9);
    thread.join().unwrap();
}

/// Records what happened to it. `close` fails if `fail_close` is set.
struct Tracked<'a> {
    state: &'a Mutex<&'static str>,
    fail_close: bool,
}

#[derive(Debug, PartialEq)]
enum Failure {
    Close,
    Work,
}

impl<'a> Tracked<'a> {
    fn new(state: &'a Mutex<&'static str>) -> Self {
        Self {
            state,
            fail_close: false,
        }
    }
}

impl Finalize for Tracked<'_> {
    type Error = Failure;

    fn close(self) -> Result<(), Failure> {
        *self.state.lock().unwrap() = "closed";
        if self.fail_close {
            Err(Failure::Close)
        } else {
            Ok(())
        }
    }

    fn abandon(self) {
        *self.state.lock().unwrap() = "abandoned";
    }
}

fn state(m: &Mutex<&'static str>) -> &'static str {
    *m.lock().unwrap()
}

#[test]
fn scope_closes_on_ok() {
    let s = Mutex::new("open");
    assert_eq!(Tracked::new(&s).scope(|_| Ok::<_, Failure>(3)), Ok(3));
    assert_eq!(state(&s), "closed");
}

#[test]
fn scope_abandons_on_err() {
    let s = Mutex::new("open");
    assert_eq!(
        Tracked::new(&s).scope(|_| Err::<(), _>(Failure::Work)),
        Err(Failure::Work)
    );
    assert_eq!(state(&s), "abandoned");
}

#[test]
fn scope_abandons_on_panic() {
    let s = Mutex::new("open");
    let result = catch_unwind(AssertUnwindSafe(|| {
        Tracked::new(&s).scope(|_| -> Result<(), Failure> { panic!("boom") })
    }));
    assert!(result.is_err());
    assert_eq!(state(&s), "abandoned");
}

#[test]
fn scope_returns_close_errors() {
    let s = Mutex::new("open");
    let t = Tracked {
        state: &s,
        fail_close: true,
    };
    assert_eq!(t.scope(|_| Ok::<_, Failure>(())), Err(Failure::Close));
}

/// Pending once, then ready: a stand-in for any `.await` that suspends.
#[derive(Default)]
struct Suspend(bool);

impl Future for Suspend {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if std::mem::replace(&mut self.0, true) {
            std::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

#[test]
fn async_scope_closes_on_ok_and_abandons_on_err() {
    let s = Mutex::new("open");
    let out = block_on(Tracked::new(&s).scope_async(async |_| {
        Suspend::default().await;
        Ok::<_, Failure>(1)
    }));
    assert_eq!((out, state(&s)), (Ok(1), "closed"));

    let out = block_on(Tracked::new(&s).scope_async(async |_| Err::<(), _>(Failure::Work)));
    assert_eq!((out, state(&s)), (Err(Failure::Work), "abandoned"));
}

/// Dropping the scope's future while the closure is suspended abandons the
/// resource: cancellation is a rollback, never a commit.
#[test]
fn async_scope_abandons_on_cancellation() {
    let s = Mutex::new("open");
    {
        let fut = Tracked::new(&s).scope_async(async |_| {
            Suspend::default().await;
            Ok::<_, Failure>(())
        });
        let mut fut = std::pin::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        assert_eq!(state(&s), "open");
    } // cancelled here
    assert_eq!(state(&s), "abandoned");
}

/// The async scope's future is `Send` when the resource is, so it can run in
/// a task on a multithreaded runtime.
#[test]
fn async_scope_is_send() {
    fn assert_send<F: Future + Send>(f: F) -> F {
        f
    }
    let s = Mutex::new("open");
    let fut = assert_send(Tracked::new(&s).scope_async(async |_| Ok::<_, Failure>(())));
    std::thread::scope(|scope| scope.spawn(move || block_on(fut)).join().unwrap()).unwrap();
    assert_eq!(state(&s), "closed");
}

/// A sink that caps how many items the core may emit.
struct Capped {
    items: Vec<u32>,
    cap: usize,
}

impl Perform<Push<u32>> for Capped {
    fn perform(&mut self, op: Push<u32>) -> Result<(), Rejected<u32>> {
        if self.items.len() == self.cap {
            return Err(Rejected(op.0));
        }
        self.items.push(op.0);
        Ok(())
    }
}

#[test]
fn push_sink_caps_output() {
    fn squares<S: Perform<Push<u32>>>(n: u32, sink: &mut S) -> Result<(), Rejected<u32>> {
        (0..n).try_for_each(|i| sink.perform(Push(i * i)))
    }

    let mut sink = Capped {
        items: Vec::new(),
        cap: 3,
    };
    assert_eq!(squares(3, &mut sink), Ok(()));
    assert_eq!(sink.items, [0, 1, 4]);
    assert_eq!(squares(1, &mut sink), Err(Rejected(0)));
}
