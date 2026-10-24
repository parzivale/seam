//! Tasks on the simulated runtime: [`JoinHandle`] and [`TaskSet`].
#![allow(clippy::disallowed_types)] // a task's result is shared with its joiner

use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::boxed::Box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use seam_core::ops::task::{
    Abort, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, YieldNow,
};
use seam_core::{Closing, Inspect, Opening, Perform, Rejected};

use super::{AsyncSim, Shared, TaskId, async_op, lock};

/// Where a task leaves its result for whoever joins it.
struct JoinState<T> {
    result: Option<Result<T, JoinError>>,
    waker: Option<Waker>,
}

type State<T> = Arc<Mutex<JoinState<T>>>;

fn finish<T>(state: &State<T>, result: Result<T, JoinError>) {
    let waker = {
        let mut s = lock(state);
        s.result = Some(result);
        s.waker.take()
    };
    if let Some(w) = waker {
        w.wake();
    }
}

/// A spawned future, as the executor stores it: catches a panic as
/// `Panicked`, and reports `Cancelled` if it is dropped before finishing.
struct TaskFuture<F: Future> {
    future: Pin<Box<F>>,
    state: State<F::Output>,
    done: bool,
}

impl<F: Future> Future for TaskFuture<F> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = &mut *self;
        let polled = catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(cx)));
        let result = match polled {
            Ok(Poll::Pending) => return Poll::Pending,
            Ok(Poll::Ready(value)) => Ok(value),
            Err(_) => Err(JoinError::Panicked),
        };
        this.done = true;
        finish(&this.state, result);
        Poll::Ready(())
    }
}

impl<F: Future> Drop for TaskFuture<F> {
    fn drop(&mut self) {
        if !self.done {
            finish(&self.state, Err(JoinError::Cancelled));
        }
    }
}

/// Spawns `future` on the runtime, returning its id and result slot.
fn spawn<F>(shared: &Arc<Shared>, future: F) -> (TaskId, State<F::Output>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let state = Arc::new(Mutex::new(JoinState {
        result: None,
        waker: None,
    }));
    let task = TaskFuture {
        future: Box::pin(future),
        state: Arc::clone(&state),
        done: false,
    };
    (shared.spawn(Box::pin(task)), state)
}

/// Waits for a task's result.
async fn wait<T>(state: &State<T>) -> Result<T, JoinError> {
    poll_fn(|cx| {
        let mut s = lock(state);
        match s.result.take() {
            Some(result) => Poll::Ready(result),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

/// A handle to a task on the simulated runtime, acquired with [`Spawn`] or
/// `SpawnWith`. Perform [`Join`], [`Abort`], [`Detach`] or [`IsFinished`]
/// on it.
///
/// When its scope succeeds, closing the handle waits for the task. When the
/// scope fails, panics or is cancelled, the task is aborted, unless it was
/// detached.
pub struct JoinHandle<T> {
    id: TaskId,
    state: State<T>,
    pub(crate) shared: Arc<Shared>,
    joined: bool,
    detached: bool,
}

impl<T> JoinHandle<T> {
    pub(crate) fn spawn<F>(shared: &Arc<Shared>, future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (id, state) = spawn(shared, future);
        Self {
            id,
            state,
            shared: Arc::clone(shared),
            joined: false,
            detached: false,
        }
    }

    pub(crate) async fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam-test: task was already joined");
        let result = wait(&self.state).await;
        self.joined = true;
        result
    }

    /// Closes the handle: waits for the task unless it was detached or
    /// already joined, discarding its output.
    pub(crate) async fn finish(mut self) -> Result<(), JoinError> {
        if !self.joined && !self.detached {
            self.join().await?;
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        self.joined || lock(&self.state).result.is_some()
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.detached && !self.finished() {
            self.shared.abort(self.id);
        }
    }
}

impl<T> core::fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JoinHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

async_op!(
    [F] AsyncSim, Opening<Spawn<F, JoinHandle<F::Output>>>,
    |s, Opening(op, _)| {
        Ok(JoinHandle::spawn(&s.shared, op.task))
    },
    where F: Future + Send + 'static, F::Output: Send + 'static
);
async_op!(
    [F] AsyncSim, Closing<Spawn<F, JoinHandle<F::Output>>>,
    |_s, Closing(task, _)| {
        task.finish().await
    },
    where F: Future + Send + 'static, F::Output: Send + 'static
);

async_op!([T: Send] JoinHandle<T>, Join<T>, |s, _op| s.join().await);

impl<T> Perform<Abort> for JoinHandle<T> {
    fn perform(&mut self, _: Abort) {
        self.shared.abort(self.id);
    }
}

impl<T> Perform<Detach> for JoinHandle<T> {
    fn perform(&mut self, _: Detach) {
        self.detached = true;
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.finished()
    }
}

/// A set of tasks on the simulated runtime, acquired with [`SpawnSet`].
/// Perform [`SpawnInto`], [`JoinNext`] or [`Abort`] on it.
///
/// When its scope succeeds, closing the set waits for every task still in
/// it and reports the first failure. Otherwise every task is aborted.
/// [`JoinNext`] returns results as tasks finish, in an order the scheduler
/// chooses from the runtime's seed.
pub struct TaskSet<T> {
    pub(crate) shared: Arc<Shared>,
    tasks: Vec<(TaskId, State<T>)>,
}

impl<T> TaskSet<T> {
    pub(crate) async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        poll_fn(|cx| {
            if self.tasks.is_empty() {
                return Poll::Ready(None);
            }
            for i in 0..self.tasks.len() {
                let result = lock(&self.tasks[i].1).result.take();
                if let Some(result) = result {
                    self.tasks.swap_remove(i);
                    return Poll::Ready(Some(result));
                }
            }
            for (_, state) in &self.tasks {
                lock(state).waker = Some(cx.waker().clone());
            }
            Poll::Pending
        })
        .await
    }
}

impl<T> Drop for TaskSet<T> {
    fn drop(&mut self) {
        for (id, _) in &self.tasks {
            self.shared.abort(*id);
        }
    }
}

impl<T> core::fmt::Debug for TaskSet<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TaskSet")
            .field("tasks", &self.tasks.len())
            .finish()
    }
}

async_op!([T: Send + 'static] AsyncSim, Opening<SpawnSet<TaskSet<T>>>, |s, Opening(_, _)| {
    Ok(TaskSet {
        shared: Arc::clone(&s.shared),
        tasks: Vec::new(),
    })
});
async_op!([T: Send + 'static] AsyncSim, Closing<SpawnSet<TaskSet<T>>>, |_s, Closing(mut set, _)| {
    let mut first = Ok(());
    while let Some(result) = set.join_next().await {
        if let (Ok(()), Err(e)) = (&first, result) {
            first = Err(e);
        }
    }
    first
});

/// Never full.
impl<T, F> Perform<SpawnInto<F>> for TaskSet<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        let task = spawn(&self.shared, op.task);
        self.tasks.push(task);
        Ok(())
    }
}

async_op!([T: Send] TaskSet<T>, JoinNext<T>, |s, _op| s.join_next().await);

impl<T> Perform<Abort> for TaskSet<T> {
    fn perform(&mut self, _: Abort) {
        for (id, _) in &self.tasks {
            self.shared.abort(*id);
        }
    }
}

/// Lets the scheduler run another task before this one continues.
async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await;
}

async_op!([] AsyncSim, YieldNow, |_s, _op| yield_now().await);

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for JoinHandle<T> {}
impl<T> seam_core::Capability for TaskSet<T> {}
