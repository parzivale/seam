//! Spawning tasks on the Tokio runtime, and the [`JoinHandle`] handle.

use core::future::Future;

use seam_core::ops::task::{Abort, Detach, IsFinished, Join, JoinError, Spawn, YieldNow};
use seam_core::{AsyncFinalize, Inspect, Perform};

use crate::Tokio;

/// A handle to a task spawned on Tokio. Perform [`Join`] to wait for its
/// output, [`Abort`] to cancel it, [`Detach`] to let it outlive the handle,
/// or [`IsFinished`] to check on it.
///
/// Unlike Tokio's own handle, dropping this one aborts the task unless it
/// was detached. In a scope, [`AsyncFinalize::close_async`] waits for the
/// task to finish, and abandoning the scope aborts it.
#[derive(Debug)]
pub struct JoinHandle<T> {
    inner: tokio::task::JoinHandle<T>,
    joined: bool,
    detached: bool,
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
            detached: false,
        }
    }
}

impl<T> JoinHandle<T> {
    async fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam: task was already joined");
        let result = (&mut self.inner).await;
        self.joined = true;
        result.map_err(|e| {
            if e.is_panic() {
                JoinError::Panicked
            } else {
                JoinError::Cancelled
            }
        })
    }
}

async_op!(perform [T: Send] JoinHandle<T>, Join<T>, |s, _op| s.join().await);

impl<T> Perform<Abort> for JoinHandle<T> {
    fn perform(&mut self, _: Abort) {
        self.inner.abort();
    }
}

impl<T> Perform<Detach> for JoinHandle<T> {
    fn perform(&mut self, _: Detach) {
        self.detached = true;
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.inner.is_finished()
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.detached {
            self.inner.abort();
        }
    }
}

impl<T: Send> AsyncFinalize for JoinHandle<T> {
    type Error = JoinError;

    /// Waits for the task to finish, discarding its output. A task that was
    /// already joined counts as finished.
    async fn close_async(mut self) -> Result<(), JoinError> {
        if !self.joined {
            self.join().await?;
        }
        Ok(())
    }
}

async_op!(perform [] Tokio, YieldNow, |_s, _op| tokio::task::yield_now().await);
