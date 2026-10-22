//! Integration tests run as a downstream crate, so they also check that the
//! blanket impls leave room for user implementations.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use seam_core::future::{AsyncPerform, SendPerform, block_on};
use seam_core::{Finalize, Inspect, Operation, Perform, Push, Query, Rejected, Scoped};

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

struct Tracked<'a>(&'a Cell<&'static str>);

impl Finalize for Tracked<'_> {
    fn close(self) {
        self.0.set("closed");
    }
    fn abandon(self) {
        self.0.set("abandoned");
    }
}

#[test]
fn scope_closes_on_return() {
    let state = Cell::new("open");
    let out = Tracked(&state).scope(|_| 3);
    assert_eq!(out, 3);
    assert_eq!(state.get(), "closed");
}

#[test]
fn scope_abandons_on_panic() {
    let state = Cell::new("open");
    let result = catch_unwind(AssertUnwindSafe(|| {
        Tracked(&state).scope(|_| panic!("boom"))
    }));
    assert!(result.is_err());
    assert_eq!(state.get(), "abandoned");
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
