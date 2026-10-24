//! Spawning tasks on the Tokio runtime, and the [`JoinHandle`] handle.

use core::future::Future;

use seam_core::ops::task::{
    Abort, AvailableParallelism, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto,
    SpawnSet, YieldNow,
};
use seam_core::{Closing, Inspect, Opening, Perform, Rejected};

use crate::Tokio;

/// A handle to a task spawned on Tokio, acquired with [`Spawn`] or
/// `SpawnWith`. Perform [`Join`] to take its output,
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

    pub(crate) async fn join(&mut self) -> Result<T, JoinError> {
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

// Spawns onto the current Tokio runtime.
//
// # Panics
//
// If called outside a Tokio runtime.
async_op!(
    perform [F] Tokio, Opening<Spawn<F, JoinHandle<F::Output>>>,
    |_s, Opening(op, _)| {
        Ok(JoinHandle::new(tokio::spawn(op.task)))
    },
    where F: Future + Send + 'static, F::Output: Send + 'static
);
async_op!(
    perform [F] Tokio, Closing<Spawn<F, JoinHandle<F::Output>>>,
    |_s, Closing(task, _)| {
        task.finish().await
    },
    where F: Future + Send + 'static, F::Output: Send + 'static
);

/// A set of tasks on Tokio, acquired with [`SpawnSet`]. Perform
/// [`SpawnInto`] to add a task, [`JoinNext`] to take the next output as
/// tasks finish, and [`Abort`] to cancel them all.
///
/// When its scope succeeds, closing the set waits for every task still in
/// it and reports the first failure. When the scope fails, panics or is
/// cancelled, every task is aborted. Backed by Tokio's `JoinSet`.
#[derive(Debug)]
pub struct TaskSet<T>(tokio::task::JoinSet<T>);

impl<T: 'static> TaskSet<T> {
    pub(crate) async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        self.0.join_next().await.map(|r| r.map_err(join_error))
    }
}

fn join_error(e: tokio::task::JoinError) -> JoinError {
    if e.is_panic() {
        JoinError::Panicked
    } else {
        JoinError::Cancelled
    }
}

async_op!(perform [T: Send + 'static] Tokio, Opening<SpawnSet<TaskSet<T>>>, |_s, Opening(_, _)| {
    Ok(TaskSet(tokio::task::JoinSet::new()))
});
async_op!(
    perform [T: Send + 'static] Tokio, Closing<SpawnSet<TaskSet<T>>>,
    |_s, Closing(mut set, _)| {
        let mut first = Ok(());
        while let Some(result) = set.0.join_next().await {
            if let (Ok(()), Err(e)) = (&first, result) {
                first = Err(join_error(e));
            }
        }
        first
    }
);

/// Spawns onto the current Tokio runtime. Never full.
///
/// # Panics
///
/// If called outside a Tokio runtime.
impl<T, F> Perform<SpawnInto<F>> for TaskSet<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        self.0.spawn(op.task);
        Ok(())
    }
}

async_op!(perform [T: Send + 'static] TaskSet<T>, JoinNext<T>, |s, _op| s.join_next().await);

impl<T: 'static> Perform<Abort> for TaskSet<T> {
    fn perform(&mut self, _: Abort) {
        self.0.abort_all();
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

impl Inspect<AvailableParallelism> for Tokio {
    fn inspect(&self, _: AvailableParallelism) -> std::num::NonZeroUsize {
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN)
    }
}

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for JoinHandle<T> {}
impl<T> seam_core::Capability for TaskSet<T> {}
