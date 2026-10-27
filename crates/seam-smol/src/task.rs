//! Spawning tasks on smol's global executor, and the [`JoinHandle`] and
//! [`TaskSet`] handles.

use core::future::Future;
use core::panic::AssertUnwindSafe;
use std::vec::Vec;

use seam_core::ops::task::{
    Abort, AvailableParallelism, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto,
    SpawnSet, YieldNow,
};
use seam_core::{Closing, Inspect, Opening, Perform, Rejected};
use smol::future::FutureExt;

use crate::Smol;

/// What a task produced, or the panic it ended in.
type Outcome<T> = std::thread::Result<T>;

/// A handle to a task spawned on smol, acquired with [`Spawn`] or
/// `SpawnWith`. Perform [`Join`] to take its output, [`Abort`] to cancel
/// it, [`Detach`] to let it outlive its scope, or [`IsFinished`] to check
/// on it.
///
/// When its scope succeeds, closing the handle waits for the task. When the
/// scope fails, panics or is cancelled, the task is cancelled, unless it
/// was detached. A cancelled `Join` leaves the task running.
#[derive(Debug)]
pub struct JoinHandle<T> {
    task: Option<smol::Task<Outcome<T>>>,
    joined: bool,
    detached: bool,
}

impl<T: Send + 'static> JoinHandle<T> {
    /// Spawns `future` on smol's global executor.
    pub(crate) fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self {
            task: Some(smol::spawn(AssertUnwindSafe(future).catch_unwind())),
            joined: false,
            detached: false,
        }
    }
}

impl<T> JoinHandle<T> {
    pub(crate) async fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam: task was already joined");
        let Some(task) = self.task.as_mut() else {
            self.joined = true;
            return Err(JoinError::Cancelled);
        };
        // Awaiting by reference: a cancelled join leaves the task running.
        let outcome = task.await;
        self.joined = true;
        outcome.map_err(|_| JoinError::Panicked)
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

/// Dropping a handle cancels the task unless it was detached.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            if self.detached {
                task.detach();
            }
        }
    }
}

async_op!(
    perform [F] Smol, Opening<Spawn<F, JoinHandle<F::Output>>>,
    |_s, Opening(op, _)| {
        Ok(JoinHandle::spawn(op.task))
    },
    where F: Future + Send + 'static, F::Output: Send + 'static
);
async_op!(
    perform [F] Smol, Closing<Spawn<F, JoinHandle<F::Output>>>,
    |_s, Closing(task, _)| task.finish().await,
    where F: Future + Send + 'static, F::Output: Send + 'static
);

async_op!(perform [T: Send] JoinHandle<T>, Join<T>, |s, _op| s.join().await);

impl<T> Perform<Abort> for JoinHandle<T> {
    fn perform(&mut self, _: Abort) {
        // Dropping a smol task cancels it.
        self.task = None;
    }
}

impl<T> Perform<Detach> for JoinHandle<T> {
    fn perform(&mut self, _: Detach) {
        self.detached = true;
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.task.as_ref().is_none_or(smol::Task::is_finished)
    }
}

/// A set of tasks on smol, acquired with [`SpawnSet`]. Perform
/// [`SpawnInto`] to add a task, [`JoinNext`] to take the next output as
/// tasks finish, and [`Abort`] to cancel them all.
///
/// When its scope succeeds, closing the set waits for every task still in
/// it and reports the first failure. When the scope fails, panics or is
/// cancelled, every task is cancelled. Never full.
#[derive(Debug)]
pub struct TaskSet<T> {
    tasks: Vec<smol::Task<()>>,
    done_tx: smol::channel::Sender<Result<T, JoinError>>,
    done: smol::channel::Receiver<Result<T, JoinError>>,
    /// Outputs not yet taken: tasks running, and results waiting.
    outstanding: usize,
}

impl<T> TaskSet<T> {
    pub(crate) async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        if self.outstanding == 0 {
            return None;
        }
        // Receiving is cancel-safe: a result not taken stays queued.
        let result = self.done.recv().await.ok()?;
        self.outstanding -= 1;
        Some(result)
    }

    fn abort_all(&mut self) {
        self.tasks.clear();
        // Only the results already in are left to take.
        self.outstanding = self.done.len();
    }
}

async_op!(perform [T: Send + 'static] Smol, Opening<SpawnSet<TaskSet<T>>>, |_s, Opening(_, _)| {
    let (done_tx, done) = smol::channel::unbounded();
    Ok(TaskSet { tasks: Vec::new(), done_tx, done, outstanding: 0 })
});
async_op!(
    perform [T: Send + 'static] Smol, Closing<SpawnSet<TaskSet<T>>>,
    |_s, Closing(mut set, _)| {
        let mut first = Ok(());
        while let Some(result) = set.join_next().await {
            if let (Ok(()), Err(e)) = (&first, result) {
                first = Err(e);
            }
        }
        first
    }
);

impl<T, F> Perform<SpawnInto<F>> for TaskSet<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        self.tasks.retain(|t| !t.is_finished());
        let done = self.done_tx.clone();
        let task = AssertUnwindSafe(op.task).catch_unwind();
        self.tasks.push(smol::spawn(async move {
            let result = task.await.map_err(|_| JoinError::Panicked);
            let _ = done.send(result).await;
        }));
        self.outstanding += 1;
        Ok(())
    }
}

async_op!(perform [T: Send + 'static] TaskSet<T>, JoinNext<T>, |s, _op| s.join_next().await);

impl<T> Perform<Abort> for TaskSet<T> {
    fn perform(&mut self, _: Abort) {
        self.abort_all();
    }
}

async_op!(perform [] Smol, YieldNow, |_s, _op| smol::future::yield_now().await);

impl Inspect<AvailableParallelism> for Smol {
    fn inspect(&self, _: AvailableParallelism) -> std::num::NonZeroUsize {
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN)
    }
}

#[cfg(feature = "time")]
mod timeouts {
    use seam_core::ops::time::{Timeout, TimesOut};

    use super::*;

    // Joining by reference is cancel-safe, so a join that times out can be
    // tried again.
    async_op!(perform [T: Send] JoinHandle<T>, Timeout<Join<T>>, |s, op| {
        match crate::within(op.after, s.join()).await {
            Some(result) => result,
            None => op.op.timed_out(),
        }
    });

    async_op!(perform [T: Send + 'static] TaskSet<T>, Timeout<JoinNext<T>>, |s, op| {
        match crate::within(op.after, s.join_next()).await {
            Some(next) => next,
            None => op.op.timed_out(),
        }
    });
}

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for JoinHandle<T> {}
impl<T> seam_core::Capability for TaskSet<T> {}
