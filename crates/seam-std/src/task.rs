//! Tasks on OS threads: [`JoinHandle`] and [`TaskSet`].
//!
//! On this backend a task is a closure, `FnOnce() -> T + Send + 'static`,
//! run on its own thread. Give it its own capability by moving a clone of
//! `Std` in. Joining blocks the calling thread.
//!
//! A running thread cannot be stopped from outside, so these handles do not
//! implement `Abort`. When a scope holding them fails or panics, it waits
//! for its threads to finish and discards their results, the way
//! `std::thread::scope` does. To stop a thread early, give it a way to be
//! told: a `Shared<bool>` it checks, or a channel it reads.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

use seam_core::ops::task::{
    AvailableParallelism, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto,
    SpawnSet, YieldNow,
};
use seam_core::{Inspect, Perform, Provide, Rejected, Token};

use crate::Std;

type Slot<T> = Arc<Mutex<Option<Result<T, JoinError>>>>;

/// A handle to a closure running on its own thread, acquired with [`Spawn`]
/// or `SpawnWith`. Perform [`Join`], [`Detach`] or [`IsFinished`] on it.
///
/// When its scope succeeds, closing the handle joins the thread and
/// reports whether it panicked. When the scope fails or panics, the thread
/// is still joined, and its result discarded, unless it was detached.
#[derive(Debug)]
pub struct JoinHandle<T> {
    thread: Option<thread::JoinHandle<()>>,
    result: Slot<T>,
    joined: bool,
    detached: bool,
}

/// Runs `work` on a new thread, catching a panic as `Panicked`. Sends `id`
/// on `done` when it finishes, if given.
pub(crate) fn spawn<T, F>(work: F, done: Option<(mpsc::Sender<u64>, u64)>) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let result: Slot<T> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&result);
    let thread = thread::spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(work)).map_err(|_| JoinError::Panicked);
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        if let Some((tx, id)) = done {
            let _ = tx.send(id);
        }
    });
    JoinHandle {
        thread: Some(thread),
        result,
        joined: false,
        detached: false,
    }
}

impl<T> JoinHandle<T> {
    fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam: thread was already joined");
        if let Some(thread) = self.thread.take() {
            let _ = thread.join(); // the closure's panic was already caught
        }
        self.joined = true;
        self.result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or(Err(JoinError::Cancelled))
    }

    /// Closes the handle: joins the thread unless it was detached or
    /// already joined, discarding its output.
    pub(crate) fn finish(mut self) -> Result<(), JoinError> {
        if !self.joined && !self.detached {
            self.join()?;
        }
        Ok(())
    }
}

/// Dropping a handle, which is how a failed scope abandons it, waits for
/// the thread unless it was detached. A thread cannot be aborted.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.detached {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

impl<T, F> Provide<Spawn<F, JoinHandle<T>>> for Std
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    fn provide(
        &mut self,
        op: Spawn<F, JoinHandle<T>>,
        _: Token,
    ) -> Result<JoinHandle<T>, JoinError> {
        Ok(spawn(op.task, None))
    }

    fn close(&mut self, task: JoinHandle<T>) -> Result<(), JoinError> {
        task.finish()
    }
}

/// Blocks until the thread finishes.
impl<T> Perform<Join<T>> for JoinHandle<T> {
    fn perform(&mut self, _: Join<T>) -> Result<T, JoinError> {
        self.join()
    }
}

impl<T> Perform<Detach> for JoinHandle<T> {
    fn perform(&mut self, _: Detach) {
        self.detached = true;
    }
}

impl<T> Inspect<IsFinished> for JoinHandle<T> {
    fn inspect(&self, _: IsFinished) -> bool {
        self.joined
            || self
                .thread
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
    }
}

/// A set of closures running on their own threads, acquired with
/// [`SpawnSet`]. Perform [`SpawnInto`] to start one and [`JoinNext`] to
/// take the next result as threads finish.
///
/// When its scope succeeds, closing the set joins every thread still in it
/// and reports the first failure. When the scope fails or panics, the
/// threads are joined and their results discarded.
#[derive(Debug)]
pub struct TaskSet<T> {
    threads: BTreeMap<u64, JoinHandle<T>>,
    next: u64,
    done_tx: mpsc::Sender<u64>,
    done_rx: mpsc::Receiver<u64>,
}

impl<T: Send + 'static> Provide<SpawnSet<TaskSet<T>>> for Std {
    fn provide(&mut self, _: SpawnSet<TaskSet<T>>, _: Token) -> Result<TaskSet<T>, JoinError> {
        let (done_tx, done_rx) = mpsc::channel();
        Ok(TaskSet {
            threads: BTreeMap::new(),
            next: 0,
            done_tx,
            done_rx,
        })
    }

    fn close(&mut self, mut set: TaskSet<T>) -> Result<(), JoinError> {
        let mut first = Ok(());
        while let Some(result) = set.join_next() {
            if let (Ok(()), Err(e)) = (&first, result) {
                first = Err(e);
            }
        }
        first
    }
}

impl<T> TaskSet<T> {
    fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        while !self.threads.is_empty() {
            let id = self.done_rx.recv().expect("the set holds a sender");
            if let Some(mut thread) = self.threads.remove(&id) {
                return Some(thread.join());
            }
        }
        None
    }
}

/// Never full.
impl<T, F> Perform<SpawnInto<F>> for TaskSet<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        let id = self.next;
        self.next += 1;
        let thread = spawn(op.task, Some((self.done_tx.clone(), id)));
        self.threads.insert(id, thread);
        Ok(())
    }
}

/// Blocks until one of the threads finishes; `None` once the set is empty.
impl<T> Perform<JoinNext<T>> for TaskSet<T> {
    fn perform(&mut self, _: JoinNext<T>) -> Option<Result<T, JoinError>> {
        self.join_next()
    }
}

/// Lets the OS run another thread.
impl Perform<YieldNow> for Std {
    fn perform(&mut self, _: YieldNow) {
        thread::yield_now();
    }
}

impl Inspect<AvailableParallelism> for Std {
    fn inspect(&self, _: AvailableParallelism) -> NonZeroUsize {
        thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
    }
}
