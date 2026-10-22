//! Async capabilities.
//!
//! Async is a superset of sync:
//!
//! - Every [`Inspect`]/[`Perform`] implementation is also an
//!   [`AsyncInspect`]/[`AsyncPerform`] one, and a [`SendInspect`]/
//!   [`SendPerform`] one when it is thread-safe.
//! - [`block_on`] runs an async core to completion without a runtime.
//!
//! Use the `Send*` traits when the future must cross threads, as it must on
//! a multithreaded runtime.
//!
//! # Which bound to write
//!
//! | The core is | Bound on | Runs with |
//! |---|---|---|
//! | sync | `Perform`/`Inspect` | any sync backend, `Sim` |
//! | async, single-threaded | `AsyncPerform`/`AsyncInspect` | the above via [`block_on`], Tokio current-thread |
//! | async, spawned on threads | `SendPerform`/`SendInspect` | all of the above, Tokio multithreaded |
//!
//! Each row accepts everything the rows above it accept, so write the
//! weakest bound the core can live with.
//!
//! ```
//! use seam_core::future::{SendPerform, block_on};
//! use seam_core::{Operation, Perform};
//!
//! struct Fetch(u32);
//! impl Operation for Fetch {
//!     type Outcome = String;
//! }
//!
//! /// Written once, with `Send` bounds so it can run on Tokio...
//! async fn fetch_both<C: SendPerform<Fetch> + Send>(c: &mut C) -> String {
//!     let a = c.perform_send(Fetch(1)).await;
//!     let b = c.perform_send(Fetch(2)).await;
//!     a + &b
//! }
//!
//! /// ...and tested against a plain sync fake, which opts in to the lifts.
//! struct Fake;
//! impl seam_core::Capability for Fake {}
//! impl Perform<Fetch> for Fake {
//!     fn perform(&mut self, op: Fetch) -> String {
//!         format!("<{}>", op.0)
//!     }
//! }
//!
//! assert_eq!(block_on(fetch_both(&mut Fake)), "<1><2>");
//! ```
//!
//! # Native async implementations
//!
//! The sync lift is a blanket impl, so there is no second blanket impl from
//! `AsyncInspect` to `AsyncPerform`. A natively async implementation of a
//! [`Query`] should implement both, with `perform_async` delegating to
//! `inspect_async`. The same goes for the `Send*` pair.

use core::future::Future;
use core::pin::pin;
#[cfg(not(feature = "std"))]
use core::task::Waker;
use core::task::{Context, Poll};

use crate::{Capability, Inspect, Operation, Perform, Query};

/// Shared access, async: run a query and wait for the outcome.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to inspect `{Q}` asynchronously",
    label = "missing capability `AsyncInspect<{Q}>`",
    note = "a sync `Inspect<{Q}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
)]
pub trait AsyncInspect<Q: Query> {
    /// Runs the query.
    fn inspect_async(&self, query: Q) -> impl Future<Output = Q::Outcome>;
}

/// Exclusive access, async: run an operation and wait for the outcome.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}` asynchronously",
    label = "missing capability `AsyncPerform<{O}>`",
    note = "a sync `Perform<{O}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
)]
pub trait AsyncPerform<O: Operation> {
    /// Runs the operation.
    fn perform_async(&mut self, op: O) -> impl Future<Output = O::Outcome>;
}

/// [`AsyncInspect`] with a `Send` future, for multithreaded runtimes.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to inspect `{Q}` from a `Send` future",
    label = "missing capability `SendInspect<{Q}>`",
    note = "a sync `Inspect<{Q}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
)]
pub trait SendInspect<Q: Query> {
    /// Runs the query.
    fn inspect_send(&self, query: Q) -> impl Future<Output = Q::Outcome> + Send;
}

/// [`AsyncPerform`] with a `Send` future, for multithreaded runtimes.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}` from a `Send` future",
    label = "missing capability `SendPerform<{O}>`",
    note = "a sync `Perform<{O}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
)]
pub trait SendPerform<O: Operation> {
    /// Runs the operation.
    fn perform_send(&mut self, op: O) -> impl Future<Output = O::Outcome> + Send;
}

impl<T: Inspect<Q> + Capability + ?Sized, Q: Query> AsyncInspect<Q> for T {
    async fn inspect_async(&self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}

impl<T: Perform<O> + Capability + ?Sized, O: Operation> AsyncPerform<O> for T {
    async fn perform_async(&mut self, op: O) -> O::Outcome {
        self.perform(op)
    }
}

impl<T: Inspect<Q> + Capability + Sync + ?Sized, Q: Query + Send> SendInspect<Q> for T {
    async fn inspect_send(&self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}

impl<T: Perform<O> + Capability + Send + ?Sized, O: Operation + Send> SendPerform<O> for T {
    async fn perform_send(&mut self, op: O) -> O::Outcome {
        self.perform(op)
    }
}

/// Runs a future to completion on the current thread, with no runtime.
///
/// This lowers an async core to sync. Use it when every capability the core
/// is given is synchronous: a test fake, `seam_test::Sim`, or `seam_std::Std`.
/// Their futures are ready on the first poll, so `block_on` never waits.
///
/// ```
/// use seam_core::future::{AsyncPerform, block_on};
/// use seam_core::{Operation, Perform};
///
/// struct Double(u32);
/// impl Operation for Double {
///     type Outcome = u32;
/// }
///
/// struct Calc;
/// impl seam_core::Capability for Calc {} // so it works from async code
/// impl Perform<Double> for Calc {
///     fn perform(&mut self, op: Double) -> u32 {
///         op.0 * 2
///     }
/// }
///
/// // An async core...
/// async fn quadruple<C: AsyncPerform<Double>>(c: &mut C, n: u32) -> u32 {
///     let n = c.perform_async(Double(n)).await;
///     c.perform_async(Double(n)).await
/// }
///
/// // ...run from sync code, against a sync capability.
/// assert_eq!(block_on(quadruple(&mut Calc, 5)), 20);
/// ```
///
/// Do not use it for futures from a real async runtime: Tokio operations
/// need a running Tokio runtime (use `seam_tokio::block_on`), and calling
/// this inside a runtime task blocks that worker thread.
///
/// # Waiting
///
/// With the `std` feature, a pending future parks the thread until its waker
/// is called, so a future woken from elsewhere costs no CPU while it waits.
/// As the [`Future`] contract requires, a future that returns
/// `Poll::Pending` must arrange to be woken, or it is never polled again.
///
/// Without `std` there is no way to park, so `block_on` re-polls in a spin
/// loop instead.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    #[cfg(feature = "std")]
    let waker = parking::waker();
    #[cfg(feature = "std")]
    let mut cx = Context::from_waker(&waker);
    #[cfg(not(feature = "std"))]
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        #[cfg(feature = "std")]
        std::thread::park();
        #[cfg(not(feature = "std"))]
        core::hint::spin_loop();
    }
}

#[cfg(feature = "std")]
mod parking {
    use core::task::Waker;
    use std::sync::Arc;
    use std::task::Wake;
    use std::thread::Thread;

    /// Wakes `block_on` by unparking the thread it runs on. An unpark that
    /// arrives before the park is remembered, so no wakeup is lost.
    struct Unpark(Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    pub(super) fn waker() -> Waker {
        Waker::from(Arc::new(Unpark(std::thread::current())))
    }
}
