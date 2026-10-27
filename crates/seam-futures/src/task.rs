//! Spawning tasks on a futures-rs executor, and the [`JoinHandle`] and
//! [`TaskSet`] handles.
//!
//! [`Executor`] wraps anything that implements `futures::task::Spawn`: a
//! `futures::executor::ThreadPool`, a `LocalPool`'s spawner, or another
//! runtime's adapter. Tasks are `Send + 'static` futures, as with
//! `futures::task::SpawnExt::spawn`.

use core::future::Future;
use core::panic::AssertUnwindSafe;
use std::boxed::Box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::vec::Vec;

use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::{AbortHandle, Abortable, Aborted};
use futures::stream::FuturesUnordered;
use futures::task::FutureObj;
use seam_core::ops::task::{
    Abort, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, YieldNow,
};
use seam_core::{Closing, Inspect, Opening, Perform, Rejected};

/// Spawns on the futures-rs executor `S`, as
/// `seam::root![Executor<fx::Spawn, fx::SpawnSet>(pool)]`.
///
/// `S` is anything that implements `futures::task::Spawn`. A closed
/// executor (a `ThreadPool` whose last handle is gone) fails spawning with
/// `JoinError::Cancelled`.
#[derive(Debug, Default, Clone)]
pub struct Executor<S>(pub S);

/// What a task produced: its output, or that it was aborted, or the
/// panic it ended in.
type Outcome<T> = Result<Result<T, Aborted>, Box<dyn core::any::Any + Send>>;

/// A spawned task, as the handle and the set keep it.
struct Task<T> {
    output: oneshot::Receiver<Outcome<T>>,
    abort: AbortHandle,
    finished: Arc<AtomicBool>,
}

fn spawn<S, F>(spawner: &S, task: F) -> Result<Task<F::Output>, F>
where
    S: futures::task::Spawn + ?Sized,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    if spawner.status().is_err() {
        return Err(task);
    }
    let (abort, registration) = AbortHandle::new_pair();
    let (tx, output) = oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let done = Arc::clone(&finished);
    let body = async move {
        let outcome = AssertUnwindSafe(Abortable::new(task, registration))
            .catch_unwind()
            .await;
        done.store(true, Ordering::Release);
        // The handle may be gone: nobody wants the output.
        let _ = tx.send(outcome);
    };
    // The executor closed between the check and the spawn: the task is
    // gone, and its handle reports it cancelled.
    let _ = spawner.spawn_obj(FutureObj::new(Box::new(body)));
    Ok(Task {
        output,
        abort,
        finished,
    })
}

fn join_result<T>(received: Result<Outcome<T>, oneshot::Canceled>) -> Result<T, JoinError> {
    match received {
        Ok(Ok(Ok(output))) => Ok(output),
        Ok(Ok(Err(Aborted))) => Err(JoinError::Cancelled),
        Ok(Err(_panic)) => Err(JoinError::Panicked),
        // The executor dropped the task without running it to the end.
        Err(oneshot::Canceled) => Err(JoinError::Cancelled),
    }
}

/// A handle to a task spawned with [`Spawn`]. Perform [`Join`] to take its
/// output, [`Abort`] to cancel it, [`Detach`] to let it outlive its scope,
/// or [`IsFinished`] to check on it.
///
/// When its scope succeeds, closing the handle waits for the task. When the
/// scope fails, panics or is cancelled, the task is aborted, unless it was
/// detached. A cancelled `Join` leaves the task running.
pub struct JoinHandle<T> {
    task: Task<T>,
    joined: bool,
    detached: bool,
}

impl<T> core::fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JoinHandle")
            .field("joined", &self.joined)
            .field("detached", &self.detached)
            .finish_non_exhaustive()
    }
}

impl<T> JoinHandle<T> {
    async fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam: task was already joined");
        let received = (&mut self.task.output).await;
        self.joined = true;
        join_result(received)
    }

    /// Closes the handle: waits for the task unless it was detached or
    /// already joined, discarding its output.
    async fn finish(mut self) -> Result<(), JoinError> {
        if !self.joined && !self.detached {
            self.join().await?;
        }
        Ok(())
    }
}

/// Dropping a handle, which is how a failed or cancelled scope abandons it,
/// aborts the task unless it was detached.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.detached {
            self.task.abort.abort();
        }
    }
}

impl<S, F> Perform<Opening<Spawn<F, JoinHandle<F::Output>>>> for Executor<S>
where
    S: futures::task::Spawn,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn perform(
        &mut self,
        Opening(op, _): Opening<Spawn<F, JoinHandle<F::Output>>>,
    ) -> Result<JoinHandle<F::Output>, JoinError> {
        let task = spawn(&self.0, op.task).map_err(|_| JoinError::Cancelled)?;
        Ok(JoinHandle {
            task,
            joined: false,
            detached: false,
        })
    }
}

// Closing never touches the executor, so a `!Send` one (a `LocalPool`'s
// spawner) closes from any future; only the `Send` form needs it `Send`.
impl<S, F> seam_core::future::AsyncPerform<Closing<Spawn<F, JoinHandle<F::Output>>>> for Executor<S>
where
    S: futures::task::Spawn,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async fn perform_async(
        &mut self,
        Closing(task, _): Closing<Spawn<F, JoinHandle<F::Output>>>,
    ) -> Result<(), JoinError> {
        task.finish().await
    }
}

impl<S, F> seam_core::future::SendPerform<Closing<Spawn<F, JoinHandle<F::Output>>>> for Executor<S>
where
    S: futures::task::Spawn + Send,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    async fn perform_send(
        &mut self,
        Closing(task, _): Closing<Spawn<F, JoinHandle<F::Output>>>,
    ) -> Result<(), JoinError> {
        task.finish().await
    }
}

async_op!(perform [T: Send] JoinHandle<T>, Join<T>, |s, _op| s.join().await);

impl<T> Perform<Abort> for JoinHandle<T> {
    fn perform(&mut self, _: Abort) {
        self.task.abort.abort();
    }
}

impl<T> Perform<Detach> for JoinHandle<T> {
    fn perform(&mut self, _: Detach) {
        self.detached = true;
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.task.finished.load(Ordering::Acquire)
    }
}

/// A set of tasks on the executor, acquired with [`SpawnSet`]. Perform
/// [`SpawnInto`] to add a task, [`JoinNext`] to take the next output as
/// tasks finish, and [`Abort`] to cancel them all.
///
/// When its scope succeeds, closing the set waits for every task still in
/// it and reports the first failure. When the scope fails, panics or is
/// cancelled, every task is aborted. A closed executor hands a task back
/// from `SpawnInto`.
pub struct TaskSet<T> {
    spawner: Box<dyn futures::task::Spawn + Send>,
    outputs: FuturesUnordered<oneshot::Receiver<Outcome<T>>>,
    aborts: Vec<AbortHandle>,
}

impl<T> core::fmt::Debug for TaskSet<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TaskSet")
            .field("tasks", &self.outputs.len())
            .finish_non_exhaustive()
    }
}

impl<T> TaskSet<T> {
    async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        futures::StreamExt::next(&mut self.outputs)
            .await
            .map(join_result)
    }

    fn abort_all(&self) {
        for abort in &self.aborts {
            abort.abort();
        }
    }
}

impl<T> Drop for TaskSet<T> {
    fn drop(&mut self) {
        self.abort_all();
    }
}

impl<S, T> Perform<Opening<SpawnSet<TaskSet<T>>>> for Executor<S>
where
    S: futures::task::Spawn + Clone + Send + 'static,
{
    fn perform(&mut self, _: Opening<SpawnSet<TaskSet<T>>>) -> Result<TaskSet<T>, JoinError> {
        Ok(TaskSet {
            spawner: Box::new(self.0.clone()),
            outputs: FuturesUnordered::new(),
            aborts: Vec::new(),
        })
    }
}

async_op!(
    perform [S, T: Send] Executor<S>, Closing<SpawnSet<TaskSet<T>>>,
    |_s, Closing(mut set, _)| {
        let mut first = Ok(());
        while let Some(result) = set.join_next().await {
            if let (Ok(()), Err(e)) = (&first, result) {
                first = Err(e);
            }
        }
        first
    },
    where S: futures::task::Spawn + Clone + Send + 'static
);

impl<T, F> Perform<SpawnInto<F>> for TaskSet<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        let task = spawn(&*self.spawner, op.task).map_err(Rejected)?;
        self.outputs.push(task.output);
        self.aborts.push(task.abort);
        Ok(())
    }
}

async_op!(perform [T: Send] TaskSet<T>, JoinNext<T>, |s, _op| s.join_next().await);

impl<T> Perform<Abort> for TaskSet<T> {
    fn perform(&mut self, _: Abort) {
        self.abort_all();
    }
}

/// Lets other tasks run: wakes itself and returns pending once.
async fn yield_now() {
    let mut yielded = false;
    core::future::poll_fn(|cx| {
        if yielded {
            core::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        }
    })
    .await;
}

impl<S> seam_core::future::AsyncPerform<YieldNow> for Executor<S> {
    async fn perform_async(&mut self, _: YieldNow) {
        yield_now().await;
    }
}

impl<S: Send> seam_core::future::SendPerform<YieldNow> for Executor<S> {
    async fn perform_send(&mut self, _: YieldNow) {
        yield_now().await;
    }
}

// Every capability type opts in to seam's lifts.
impl<S> seam_core::Capability for Executor<S> {}
impl<T> seam_core::Capability for JoinHandle<T> {}
impl<T> seam_core::Capability for TaskSet<T> {}
