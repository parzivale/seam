//! Spawning tasks on the Tokio runtime, and the [`JoinHandle`] handle.

use core::future::Future;

use seam_core::ops::task::{Abort, IsFinished, Join, JoinError, Spawn, YieldNow};
use seam_core::{Finalize, Inspect, Perform};

use crate::Tokio;

/// A handle to a task spawned on Tokio. Perform [`Join`] to wait for its
/// output, [`Abort`] to cancel it, or [`IsFinished`] to check on it.
///
/// Dropping the handle detaches the task, as with Tokio's. Through
/// [`Scoped`](seam_core::Scoped), [`Finalize::close`] detaches it and
/// [`Finalize::abandon`] aborts it, so a task tied to a scope is cancelled
/// if the scope panics.
#[derive(Debug)]
pub struct JoinHandle<T> {
    inner: tokio::task::JoinHandle<T>,
    joined: bool,
}

/// Spawns onto the current Tokio runtime.
///
/// # Panics
///
/// If called outside a Tokio runtime.
impl<F> Perform<Spawn<F, JoinHandle<F::Output>>> for Tokio
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn perform(&mut self, op: Spawn<F, JoinHandle<F::Output>>) -> JoinHandle<F::Output> {
        JoinHandle {
            inner: tokio::spawn(op.future),
            joined: false,
        }
    }
}

async_op!(perform [T: Send] JoinHandle<T>, Join<T>, |s, _op| {
    assert!(!s.joined, "seam: task was already joined");
    let result = (&mut s.inner).await;
    s.joined = true;
    result.map_err(|e| if e.is_panic() { JoinError::Panicked } else { JoinError::Cancelled })
});

impl<T> Perform<Abort> for JoinHandle<T> {
    fn perform(&mut self, _: Abort) {
        self.inner.abort();
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.inner.is_finished()
    }
}

impl<T> Finalize for JoinHandle<T> {
    /// Detaches the task: it keeps running.
    fn close(self) {}

    /// Aborts the task.
    fn abandon(self) {
        self.inner.abort();
    }
}

async_op!(perform [] Tokio, YieldNow, |_s, _op| tokio::task::yield_now().await);
