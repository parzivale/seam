//! Spawning tasks on the Tokio runtime, and the [`JoinHandle`] handle.

use core::future::Future;

use seam_core::ops::task::{Abort, Detach, IsFinished, Join, JoinError, Spawn, SpawnAll, YieldNow};
use seam_core::{AsyncProvide, Inspect, Perform, Token};

use crate::Tokio;

/// A handle to a task spawned on Tokio, acquired with [`Spawn`],
/// [`SpawnAll`] or `SpawnWith`. Perform [`Join`] to take its output,
/// [`Abort`] to cancel it, [`Detach`] to let it outlive its scope, or
/// [`IsFinished`] to check on it.
///
/// When its scope succeeds, closing the handle waits for the task. When the
/// scope fails, panics or is cancelled, the task is aborted, unless it was
/// detached.
#[derive(Debug)]
pub struct JoinHandle<T> {
    inner: tokio::task::JoinHandle<T>,
    joined: bool,
    detached: bool,
}

impl<T> JoinHandle<T> {
    pub(crate) fn new(inner: tokio::task::JoinHandle<T>) -> Self {
        Self {
            inner,
            joined: false,
            detached: false,
        }
    }

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

    /// Closes the handle: waits for the task unless it was detached or
    /// already joined, discarding its output.
    pub(crate) async fn finish(mut self) -> Result<(), JoinError> {
        if !self.joined && !self.detached {
            self.join().await?;
        }
        Ok(())
    }
}

/// Spawns onto the current Tokio runtime.
///
/// # Panics
///
/// If called outside a Tokio runtime.
impl<F> AsyncProvide<Spawn<F, JoinHandle<F::Output>>> for Tokio
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async fn provide_async(
        &mut self,
        op: Spawn<F, JoinHandle<F::Output>>,
        _: Token,
    ) -> Result<JoinHandle<F::Output>, JoinError> {
        Ok(JoinHandle::new(tokio::spawn(op.future)))
    }

    async fn close_async(&mut self, task: JoinHandle<F::Output>) -> Result<(), JoinError> {
        task.finish().await
    }
}

/// Spawns every task onto the current Tokio runtime. Closing waits for all
/// of them, then reports the first failure.
///
/// # Panics
///
/// If called outside a Tokio runtime.
impl<F> AsyncProvide<SpawnAll<F, JoinHandle<F::Output>>> for Tokio
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async fn provide_async(
        &mut self,
        op: SpawnAll<F, JoinHandle<F::Output>>,
        _: Token,
    ) -> Result<Vec<JoinHandle<F::Output>>, JoinError> {
        Ok(op
            .futures
            .into_iter()
            .map(|f| JoinHandle::new(tokio::spawn(f)))
            .collect())
    }

    async fn close_async(&mut self, tasks: Vec<JoinHandle<F::Output>>) -> Result<(), JoinError> {
        let mut first = Ok(());
        for task in tasks {
            let result = task.finish().await;
            if first.is_ok() {
                first = result;
            }
        }
        first
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

/// Dropping a handle, which is how a failed or cancelled scope abandons it,
/// aborts the task unless it was detached.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.detached {
            self.inner.abort();
        }
    }
}

async_op!(perform [] Tokio, YieldNow, |_s, _op| tokio::task::yield_now().await);
