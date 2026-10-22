//! Simulated threads: [`JoinHandle`] and [`ThreadSet`].
#![allow(clippy::disallowed_types)] // a thread's result is shared with its joiner

use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex};
use std::task::Waker;

use seam_core::ops::task::{
    Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, YieldNow,
};
use seam_core::{Inspect, Perform, Provide, Rejected, Token};

use super::{Halted, Shared, ThreadSim, lock};

/// Where a thread leaves its result for whoever joins it.
struct Slot<T> {
    result: Option<Result<T, JoinError>>,
    waker: Option<Waker>,
}

type SlotRef<T> = Arc<Mutex<Slot<T>>>;

/// Where a set's threads report, by id, that they finished.
pub(crate) struct Done {
    ids: VecDeque<u64>,
    waker: Option<Waker>,
}

/// Runs `work` as a new simulated thread, catching a panic as `Panicked`.
/// Pushes `id` to `done` when it finishes, if given.
pub(crate) fn spawn<T, F>(
    shared: &Arc<Shared>,
    work: F,
    done: Option<(Arc<Mutex<Done>>, u64)>,
) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let slot: SlotRef<T> = Arc::new(Mutex::new(Slot {
        result: None,
        waker: None,
    }));
    let theirs = Arc::clone(&slot);
    shared.spawn(move || {
        let outcome = catch_unwind(AssertUnwindSafe(work)).map_err(|payload| {
            if payload.is::<Halted>() {
                resume_unwind(payload); // the run is stopping, not the core panicking
            }
            JoinError::Panicked
        });
        let waker = {
            let mut s = lock(&theirs);
            s.result = Some(outcome);
            s.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        if let Some((done, id)) = done {
            let waker = {
                let mut d = lock(&done);
                d.ids.push_back(id);
                d.waker.take()
            };
            if let Some(w) = waker {
                w.wake();
            }
        }
    });
    JoinHandle {
        slot,
        shared: Arc::clone(shared),
        joined: false,
        detached: false,
    }
}

/// A handle to a simulated thread, acquired with [`Spawn`] or `SpawnWith`.
/// Perform [`Join`], [`Detach`] or [`IsFinished`] on it.
///
/// It follows `seam-std`'s rules: when its scope succeeds, closing the
/// handle joins the thread and reports whether it panicked. When the scope
/// fails or panics, the thread is still joined, and its result discarded,
/// unless it was detached. A thread cannot be aborted.
pub struct JoinHandle<T> {
    slot: SlotRef<T>,
    shared: Arc<Shared>,
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
    fn poll_result(&self, waker: &Waker) -> Option<Result<T, JoinError>> {
        let mut s = lock(&self.slot);
        let result = s.result.take();
        if result.is_none() {
            s.waker = Some(waker.clone());
        }
        result
    }

    fn join(&mut self) -> Result<T, JoinError> {
        assert!(!self.joined, "seam-test: thread was already joined");
        let result = self.shared.wait_until(|w| self.poll_result(w));
        self.joined = true;
        result
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
/// the thread unless it was detached. When the run is stopping it does not
/// wait.
impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if !self.joined && !self.detached {
            let _ = self.shared.try_wait_until(|w| self.poll_result(w));
        }
    }
}

impl<T, F> Provide<Spawn<F, JoinHandle<T>>> for ThreadSim
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    fn provide(
        &mut self,
        op: Spawn<F, JoinHandle<T>>,
        _: Token,
    ) -> Result<JoinHandle<T>, JoinError> {
        Ok(spawn(&self.shared, op.task, None))
    }

    fn close(&mut self, thread: JoinHandle<T>) -> Result<(), JoinError> {
        thread.finish()
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
        self.shared.switch();
        self.joined || lock(&self.slot).result.is_some()
    }
}

/// A set of simulated threads, acquired with [`SpawnSet`]. Perform
/// [`SpawnInto`] to start one and [`JoinNext`] to take the next result as
/// threads finish.
///
/// When its scope succeeds, closing the set joins every thread still in it
/// and reports the first failure. When the scope fails or panics, the
/// threads are joined and their results discarded.
pub struct ThreadSet<T> {
    threads: BTreeMap<u64, JoinHandle<T>>,
    next: u64,
    done: Arc<Mutex<Done>>,
    shared: Arc<Shared>,
}

impl<T> core::fmt::Debug for ThreadSet<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ThreadSet")
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

impl<T: Send + 'static> Provide<SpawnSet<ThreadSet<T>>> for ThreadSim {
    fn provide(&mut self, _: SpawnSet<ThreadSet<T>>, _: Token) -> Result<ThreadSet<T>, JoinError> {
        Ok(ThreadSet {
            threads: BTreeMap::new(),
            next: 0,
            done: Arc::new(Mutex::new(Done {
                ids: VecDeque::new(),
                waker: None,
            })),
            shared: Arc::clone(&self.shared),
        })
    }

    fn close(&mut self, mut set: ThreadSet<T>) -> Result<(), JoinError> {
        let mut first = Ok(());
        while let Some(result) = set.join_next() {
            if let (Ok(()), Err(e)) = (&first, result) {
                first = Err(e);
            }
        }
        first
    }
}

impl<T> ThreadSet<T> {
    fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        while !self.threads.is_empty() {
            let id = self.shared.wait_until(|w| {
                let mut d = lock(&self.done);
                let id = d.ids.pop_front();
                if id.is_none() {
                    d.waker = Some(w.clone());
                }
                id
            });
            if let Some(mut thread) = self.threads.remove(&id) {
                return Some(thread.join());
            }
        }
        None
    }
}

/// Never full.
impl<T, F> Perform<SpawnInto<F>> for ThreadSet<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    fn perform(&mut self, op: SpawnInto<F>) -> Result<(), Rejected<F>> {
        let id = self.next;
        self.next += 1;
        let thread = spawn(&self.shared, op.task, Some((Arc::clone(&self.done), id)));
        self.threads.insert(id, thread);
        Ok(())
    }
}

/// Blocks until one of the threads finishes; `None` once the set is empty.
impl<T> Perform<JoinNext<T>> for ThreadSet<T> {
    fn perform(&mut self, _: JoinNext<T>) -> Option<Result<T, JoinError>> {
        self.join_next()
    }
}

/// Lets the scheduler run another thread.
impl Perform<YieldNow> for ThreadSim {
    fn perform(&mut self, _: YieldNow) {
        self.shared.switch();
    }
}
