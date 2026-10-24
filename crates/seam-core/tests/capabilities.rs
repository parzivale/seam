//! Integration tests run as a downstream crate, so they also check that the
//! blanket impls leave room for user implementations.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

use seam_core::future::{AsyncPerform, SendPerform, block_on};
use seam_core::{
    Acquisition, AsyncProvide, Closing, Inspect, Lend, Opening, Operation, Perform, Provide, Push,
    Query, Rejected, SendProvide, SendScope,
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
impl seam_core::Capability for Cell32 {}

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

/// A test provider that records what happened to each lease it hands out.
impl seam_core::Capability for Leases {}
#[derive(Default)]
struct Leases {
    log: Vec<&'static str>,
    fail_open: bool,
    fail_close: bool,
}

/// Acquire a lease: a `u32` the body can change.
struct Lease;

#[derive(Debug, PartialEq)]
enum Failure {
    Open,
    Close,
    Work,
}

impl Acquisition for Lease {
    type Resource = u32;
    type Error = Failure;
}

impl Perform<Opening<Lease>> for Leases {
    fn perform(&mut self, _: Opening<Lease>) -> Result<u32, Failure> {
        if self.fail_open {
            return Err(Failure::Open);
        }
        self.log.push("opened");
        Ok(0)
    }
}

impl Perform<Closing<Lease>> for Leases {
    fn perform(&mut self, n: Closing<Lease>) -> Result<(), Failure> {
        if self.fail_close {
            return Err(Failure::Close);
        }
        self.log
            .push(if n.0 == 0 { "closed" } else { "closed changed" });
        Ok(())
    }
}

/// Async-only, and its resources log being dropped, so that rolling back
/// can be observed without `&mut self`.
struct AsyncLeases<'a>(&'a Mutex<Vec<&'static str>>);

struct Tracked<'a>(&'a Mutex<Vec<&'static str>>);

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("dropped");
    }
}

struct AsyncLease;

impl Acquisition for AsyncLease {
    type Resource = Tracked<'static>;
    type Error = Failure;
}

impl AsyncPerform<Opening<AsyncLease>> for AsyncLeases<'static> {
    async fn perform_async(&mut self, _: Opening<AsyncLease>) -> Result<Tracked<'static>, Failure> {
        Suspend::default().await;
        Ok(Tracked(self.0))
    }
}

impl AsyncPerform<Closing<AsyncLease>> for AsyncLeases<'static> {
    async fn perform_async(&mut self, lease: Closing<AsyncLease>) -> Result<(), Failure> {
        Suspend::default().await;
        self.0.lock().unwrap().push("closed");
        drop(lease);
        Ok(())
    }
}

/// Lends counters: a scope that is a query.
#[derive(Default)]
struct Counters {
    opened: std::sync::atomic::AtomicU32,
}
impl seam_core::Capability for Counters {}

impl Inspect<Opening<Lease>> for Counters {
    fn inspect(&self, _: Opening<Lease>) -> Result<u32, Failure> {
        Ok(self
            .opened
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

impl Inspect<Closing<Lease>> for Counters {
    fn inspect(&self, _: Closing<Lease>) -> Result<(), Failure> {
        Ok(())
    }
}

#[test]
fn lend_needs_only_shared_access_and_lifts_to_provide() {
    fn shared<C: Lend<Lease>>(c: &C) -> Result<u32, Failure> {
        c.lend(Lease, |c, n| {
            *n += 10; // local to the scope
            c.lend(Lease, |_, inner| Ok(*n + *inner))
        })
    }
    fn exclusive<C: Provide<Lease>>(c: &mut C) -> Result<u32, Failure> {
        c.acquire(Lease, |_, n| Ok(*n))
    }
    let mut c = Counters::default();
    assert_eq!(shared(&c), Ok(11));
    assert_eq!(exclusive(&mut c), Ok(2));
}

#[test]
fn acquire_closes_on_ok() {
    let mut p = Leases::default();
    assert_eq!(
        p.acquire(Lease, |_, n| {
            *n += 1;
            Ok::<_, Failure>("value")
        }),
        Ok("value")
    );
    assert_eq!(p.log, ["opened", "closed changed"]);
}

#[test]
fn acquire_drops_on_err_and_panic() {
    let mut p = Leases::default();
    assert_eq!(
        p.acquire(Lease, |_, _| Err::<(), _>(Failure::Work)),
        Err(Failure::Work)
    );
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        p.acquire(Lease, |_, _| -> Result<(), Failure> { panic!("boom") })
    }));
    assert!(panicked.is_err());
    assert_eq!(p.log, ["opened", "opened"], "neither lease was closed");
}

#[test]
fn acquire_returns_open_and_close_errors() {
    let mut p = Leases {
        fail_open: true,
        ..Leases::default()
    };
    assert_eq!(
        p.acquire(Lease, |_, _| Ok::<_, Failure>(())),
        Err(Failure::Open)
    );
    let mut p = Leases {
        fail_close: true,
        ..Leases::default()
    };
    assert_eq!(
        p.acquire(Lease, |_, _| Ok::<_, Failure>(())),
        Err(Failure::Close)
    );
}

/// The body gets the provider back, so it can acquire more while it holds
/// one resource; the inner scope closes first.
#[test]
fn acquisitions_nest() {
    let mut p = Leases::default();
    p.acquire(Lease, |p, outer| {
        *outer += 1;
        p.acquire(Lease, |_, _| Ok::<_, Failure>(()))
    })
    .unwrap();
    assert_eq!(p.log, ["opened", "opened", "closed", "closed changed"]);
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

fn leak_log() -> &'static Mutex<Vec<&'static str>> {
    Box::leak(Box::new(Mutex::new(Vec::new())))
}

fn log(m: &Mutex<Vec<&'static str>>) -> Vec<&'static str> {
    m.lock().unwrap().clone()
}

#[test]
fn sync_providers_lift_to_async() {
    let mut p = Leases::default();
    let out = block_on(p.acquire_async(Lease, async |_, n| {
        Suspend::default().await;
        *n += 1;
        Ok::<_, Failure>(())
    }));
    assert_eq!(out, Ok(()));
    assert_eq!(p.log, ["opened", "closed changed"]);
}

#[test]
fn async_acquire_closes_on_ok_and_drops_on_err() {
    let l = leak_log();
    let mut p = AsyncLeases(l);
    block_on(p.acquire_async(AsyncLease, async |_, _| Ok::<_, Failure>(()))).unwrap();
    assert_eq!(log(l), ["closed", "dropped"]);

    let l = leak_log();
    let mut p = AsyncLeases(l);
    let out = block_on(p.acquire_async(AsyncLease, async |_, _| Err::<(), _>(Failure::Work)));
    assert_eq!(out, Err(Failure::Work));
    assert_eq!(log(l), ["dropped"]);
}

/// Drops the future after `polls` polls, then reports what happened.
fn cancel_after(polls: usize) -> Vec<&'static str> {
    let l = leak_log();
    let mut p = AsyncLeases(l);
    {
        let fut = p.acquire_async(AsyncLease, async |_, _| {
            Suspend::default().await;
            Ok::<_, Failure>(())
        });
        let mut fut = std::pin::pin!(fut);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        for _ in 0..polls {
            if fut.as_mut().poll(&mut cx).is_ready() {
                break;
            }
        }
    }
    log(l)
}

/// Cancelling at every point: while opening (nothing to roll back), inside
/// the body (dropped), while closing (the resource is dropped inside the
/// closing future), and after finishing (closed). Never a half state.
#[test]
fn async_acquire_is_cancellation_safe_at_every_await() {
    assert_eq!(
        cancel_after(1),
        Vec::<&str>::new(),
        "cancelled while opening"
    );
    assert_eq!(cancel_after(2), ["dropped"], "cancelled in the body");
    assert_eq!(
        cancel_after(3),
        ["dropped"],
        "cancelled while closing: dropped, not closed"
    );
    assert_eq!(cancel_after(4), ["closed", "dropped"], "finished");
}

/// The future is `Send` when the provider and resource are, so it can run
/// in a task on a multithreaded runtime.
#[test]
fn async_acquire_is_send() {
    fn assert_send<F: Future + Send>(f: F) -> F {
        f
    }
    let l = leak_log();
    let mut p = AsyncLeases(l);
    let fut = assert_send(p.acquire_async(AsyncLease, async |_, _| Ok::<_, Failure>(())));
    std::thread::scope(|s| s.spawn(move || block_on(fut)).join().unwrap()).unwrap();
    assert_eq!(log(l), ["closed", "dropped"]);

    // Generic code proves it from the `Send` forms.
    async fn generic<C: SendProvide<Lease> + Send>(c: &mut C) -> Result<(), Failure> {
        c.acquire_send(Lease, async |_, _| Ok(())).await
    }
    let mut p = Leases::default();
    block_on(assert_send(generic(&mut p))).unwrap();
    assert_eq!(p.log, ["opened", "closed"]);
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

/// Checked at compile time: a generic core bounded on `SendProvide` gets a
/// `Send` `acquire_send` future.
#[allow(dead_code)]
fn generic_scopes_are_send<C: SendProvide<Lease> + Send>(c: &mut C) {
    fn assert_send<F: Future + Send>(_: F) {}
    assert_send(c.acquire_send(Lease, async |_, _| Ok::<_, Failure>(())));
}
