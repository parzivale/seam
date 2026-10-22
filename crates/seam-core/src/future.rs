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
//! # Native async implementations
//!
//! The sync lift is a blanket impl, so there is no second blanket impl from
//! `AsyncInspect` to `AsyncPerform`. A natively async implementation of a
//! [`Query`] should implement both, with `perform_async` delegating to
//! `inspect_async`. The same goes for the `Send*` pair.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use crate::{Inspect, Operation, Perform, Query};

/// Shared access, async: run a query and wait for the outcome.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to inspect `{Q}` asynchronously",
    label = "missing capability `AsyncInspect<{Q}>`"
)]
pub trait AsyncInspect<Q: Query> {
    /// Runs the query.
    fn inspect_async(&self, query: Q) -> impl Future<Output = Q::Outcome>;
}

/// Exclusive access, async: run an operation and wait for the outcome.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}` asynchronously",
    label = "missing capability `AsyncPerform<{O}>`"
)]
pub trait AsyncPerform<O: Operation> {
    /// Runs the operation.
    fn perform_async(&mut self, op: O) -> impl Future<Output = O::Outcome>;
}

/// [`AsyncInspect`] with a `Send` future, for multithreaded runtimes.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to inspect `{Q}` from a `Send` future",
    label = "missing capability `SendInspect<{Q}>`"
)]
pub trait SendInspect<Q: Query> {
    /// Runs the query.
    fn inspect_send(&self, query: Q) -> impl Future<Output = Q::Outcome> + Send;
}

/// [`AsyncPerform`] with a `Send` future, for multithreaded runtimes.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}` from a `Send` future",
    label = "missing capability `SendPerform<{O}>`"
)]
pub trait SendPerform<O: Operation> {
    /// Runs the operation.
    fn perform_send(&mut self, op: O) -> impl Future<Output = O::Outcome> + Send;
}

impl<T: Inspect<Q> + ?Sized, Q: Query> AsyncInspect<Q> for T {
    async fn inspect_async(&self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}

impl<T: Perform<O> + ?Sized, O: Operation> AsyncPerform<O> for T {
    async fn perform_async(&mut self, op: O) -> O::Outcome {
        self.perform(op)
    }
}

impl<T: Inspect<Q> + Sync + ?Sized, Q: Query + Send> SendInspect<Q> for T {
    async fn inspect_send(&self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}

impl<T: Perform<O> + Send + ?Sized, O: Operation + Send> SendPerform<O> for T {
    async fn perform_send(&mut self, op: O) -> O::Outcome {
        self.perform(op)
    }
}

/// Runs a future to completion on the current thread, with no runtime.
///
/// This works for core futures because they only suspend on operations the
/// program itself defines, so a pending future can simply be polled again.
/// It busy-polls: a future that waits on an external wakeup (a socket, a
/// timer from a real runtime) will spin forever.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        core::hint::spin_loop();
    }
}
