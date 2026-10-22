//! A deterministic runtime for async cores, over the simulated world.
//!
//! [`Runtime`] runs async code on a single-threaded executor that owns a
//! [`Sim`]. Cores get an [`AsyncSim`]: a cloneable capability, so that tasks
//! can have their own, over the same world.
//!
//! ```
//! # #[cfg(all(feature = "time", feature = "io-std"))] {
//! use std::time::Duration;
//! use seam_core::prelude::*;
//! use seam_core::ops::stdio::Print;
//! use seam_core::ops::task::{Spawn, Join};
//! use seam_core::ops::time::Sleep;
//! use seam_test::Sim;
//! use seam_test::rt::{JoinHandle, Runtime};
//!
//! let rt = Runtime::new(Sim::new());
//! rt.block_on(|mut sim| async move {
//!     let mut worker = sim.clone();
//!     let late = Spawn::<_, JoinHandle<()>>::new(async move {
//!         worker.perform_send(Sleep(Duration::from_secs(60))).await;
//!         let _ = worker.perform_send(Print(b"a minute later\n")).await;
//!     });
//!     sim.acquire_async(late, async |sim, task| {
//!         let _ = sim.perform_send(Print(b"right away\n")).await;
//!         task.perform_send(Join::new()).await
//!     })
//!     .await
//!     .unwrap();
//! });
//! // An hour of simulated waiting takes no time at all.
//! assert_eq!(rt.with_sim(|s| s.take_stdout()), b"right away\na minute later\n");
//! # }
//! ```
//!
//! # Scheduling
//!
//! Whenever more than one task can run, the next one is chosen at random
//! from the runtime's seed, so the same seed always gives the same
//! interleaving, and different seeds explore different ones. Run a test
//! over many seeds to look for ordering bugs, and replay a failure with the
//! seed that found it.
//!
//! # Waiting
//!
//! Operations that would wait on a real system suspend instead of failing:
//! reading a stream with no data yet, accepting with no connection pending,
//! receiving with no datagram, waiting for Ctrl-C, and `Sleep`. Each is
//! woken by exactly the event it waits for.
//!
//! When no task can run, the runtime moves the clock straight to the next
//! timer, so simulated time costs nothing. When no task can run and no
//! timer is set, nothing can ever happen again: that is a deadlock, which
//! [`Runtime::block_on`] reports by panicking and
//! [`Runtime::try_block_on`] by returning [`Deadlock`].
//!
//! # Tasks
//!
//! `AsyncSim` provides `Spawn` and `SpawnSet`, and the runtime's listeners
//! provide `SpawnWith<Accept<_>, ..>`, with the same rules as the real
//! backends: a successful scope waits for its tasks, a failed or cancelled
//! one aborts them, and `Detach` opts out. A panic in a task is caught and
//! reported as `JoinError::Panicked`. When `block_on` returns, tasks still
//! running are dropped.
#![allow(clippy::disallowed_types)] // the runtime's own state is shared

use core::fmt;
use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};
use core::time::Duration;
use std::boxed::Box;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::Wake;
use std::vec::Vec;

use crate::Sim;

#[cfg(feature = "net")]
pub mod net;
mod ops;
#[cfg(feature = "sync")]
pub mod sync;
mod task;
#[cfg(feature = "time")]
mod timeouts;

pub use task::{JoinHandle, TaskSet};

/// Implements the async capability traits for an operation from one async
/// body: `AsyncPerform` and `SendPerform`.
macro_rules! async_op {
    ([$($gen:tt)*] $ty:ty, $op:ty, |$s:ident, $o:ident| $body:expr) => {
        impl<$($gen)*> seam_core::future::AsyncPerform<$op> for $ty {
            fn perform_async(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> {
                let $s = self;
                async move { $body }
            }
        }

        impl<$($gen)*> seam_core::future::SendPerform<$op> for $ty {
            fn perform_send(
                &mut self,
                $o: $op,
            ) -> impl core::future::Future<Output = <$op as seam_core::Operation>::Outcome> + Send
            {
                let $s = self;
                async move { $body }
            }
        }
    };
}
use async_op;

/// A deterministic, single-threaded runtime over a [`Sim`].
///
/// Set the world up with `Sim`'s methods first, hand it over with
/// [`Runtime::new`], run async code with [`block_on`](Self::block_on), and
/// inspect the world afterwards with [`with_sim`](Self::with_sim).
pub struct Runtime {
    shared: Arc<Shared>,
}

/// The capability that async code running on a [`Runtime`] performs
/// effects through. Cheap to clone; every clone works on the same world.
///
/// It performs the same operations as [`Sim`], except that operations
/// which would wait on a real system suspend until they can finish, and it
/// provides tasks. Sockets acquired through it are the runtime's own
/// (`rt::net`), whose reads and accepts wait.
#[derive(Clone)]
pub struct AsyncSim {
    pub(crate) shared: Arc<Shared>,
}

/// Every task is waiting, and nothing it waits for can ever happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadlock {
    /// How many spawned tasks were still waiting, besides the main one.
    pub tasks: usize,
}

impl fmt::Display for Deadlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seam-test: deadlock: the main future and {} task(s) are all waiting, and no timer is set",
            self.tasks
        )
    }
}

impl core::error::Error for Deadlock {}

pub(crate) struct Shared {
    sim: Mutex<Sim>,
    exec: Mutex<Exec>,
}

type TaskId = u64;

/// The main future, which is polled in place rather than stored.
const MAIN: TaskId = 0;

type BoxedTask = Pin<Box<dyn Future<Output = ()> + Send>>;

enum Slot {
    /// Waiting to be polled.
    Idle(BoxedTask),
    /// Being polled right now.
    Running,
    /// Aborted while being polled: dropped once the poll returns.
    RunningAborted,
    /// Aborted: dropped the next time the executor reaches it.
    Aborted(BoxedTask),
}

#[derive(Default)]
struct Exec {
    /// The scheduler's own SplitMix64 state, separate from `RandomU64`'s, so
    /// that scheduling does not shift a core's random numbers.
    rng: u64,
    ready: BTreeSet<TaskId>,
    tasks: BTreeMap<TaskId, Slot>,
    next_id: TaskId,
    /// Sleeping tasks, by deadline, then registration order.
    timers: BTreeMap<(Duration, u64), Waker>,
    next_timer: u64,
    /// Tasks waiting for Ctrl-C.
    signal_waiters: Vec<Waker>,
}

impl Exec {
    fn next_random(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Picks the next task to run, at random among those that are ready.
    fn pick(&mut self) -> Option<TaskId> {
        if self.ready.is_empty() {
            return None;
        }
        let n = (self.next_random() % self.ready.len() as u64) as usize;
        let id = *self.ready.iter().nth(n).expect("n is in range");
        self.ready.remove(&id);
        Some(id)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wakes a task by marking it ready. Holds the runtime weakly, so that
/// wakers kept in the world do not keep it alive.
struct TaskWaker {
    id: TaskId,
    shared: Weak<Shared>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(shared) = self.shared.upgrade() {
            let mut exec = lock(&shared.exec);
            if self.id == MAIN || exec.tasks.contains_key(&self.id) {
                exec.ready.insert(self.id);
            }
        }
    }
}

impl Shared {
    fn waker(self: &Arc<Self>, id: TaskId) -> Waker {
        Waker::from(Arc::new(TaskWaker {
            id,
            shared: Arc::downgrade(self),
        }))
    }

    pub(crate) fn sim(&self) -> MutexGuard<'_, Sim> {
        lock(&self.sim)
    }

    fn exec(&self) -> MutexGuard<'_, Exec> {
        lock(&self.exec)
    }

    /// Adds a task and marks it ready.
    pub(crate) fn spawn(&self, task: BoxedTask) -> TaskId {
        let mut exec = self.exec();
        exec.next_id += 1;
        let id = exec.next_id;
        exec.tasks.insert(id, Slot::Idle(task));
        exec.ready.insert(id);
        id
    }

    /// Aborts a task: it is dropped without being polled again.
    pub(crate) fn abort(&self, id: TaskId) {
        let mut exec = self.exec();
        let Some(slot) = exec.tasks.get_mut(&id) else {
            return;
        };
        match core::mem::replace(slot, Slot::Running) {
            Slot::Idle(task) | Slot::Aborted(task) => *slot = Slot::Aborted(task),
            Slot::Running | Slot::RunningAborted => *slot = Slot::RunningAborted,
        }
        exec.ready.insert(id);
    }

    /// Wakes `waker` once the clock reaches `deadline`. Returns the timer's
    /// key, for [`remove_timer`](Self::remove_timer).
    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    pub(crate) fn add_timer(&self, deadline: Duration, waker: Waker) -> (Duration, u64) {
        let mut exec = self.exec();
        exec.next_timer += 1;
        let key = (deadline, exec.next_timer);
        exec.timers.insert(key, waker);
        key
    }

    /// Removes a timer that is no longer needed, so that it does not move
    /// the clock when nothing else is left to run.
    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    pub(crate) fn remove_timer(&self, key: (Duration, u64)) {
        self.exec().timers.remove(&key);
    }

    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    pub(crate) fn wait_for_signal(&self, waker: &Waker) {
        let mut exec = self.exec();
        if !exec.signal_waiters.iter().any(|w| w.will_wake(waker)) {
            exec.signal_waiters.push(waker.clone());
        }
    }

    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    fn wake_signal_waiters(&self) {
        let waiters = core::mem::take(&mut self.exec().signal_waiters);
        for w in waiters {
            w.wake();
        }
    }

    /// Polls one spawned task.
    fn run(self: &Arc<Self>, id: TaskId) {
        let task = {
            let mut exec = self.exec();
            match exec
                .tasks
                .get_mut(&id)
                .map(|slot| core::mem::replace(slot, Slot::Running))
            {
                Some(Slot::Idle(task)) => task,
                Some(Slot::Aborted(task)) => {
                    exec.tasks.remove(&id);
                    drop(exec);
                    drop(task); // reports `Cancelled` to its joiner
                    return;
                }
                Some(other) => {
                    // Woken while already running or finishing: nothing to do.
                    *exec.tasks.get_mut(&id).expect("just found") = other;
                    return;
                }
                None => return,
            }
        };
        let mut task = task;
        let waker = self.waker(id);
        let done = task
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_ready();
        let mut exec = self.exec();
        let aborted = matches!(exec.tasks.get(&id), Some(Slot::RunningAborted));
        if done || aborted {
            exec.tasks.remove(&id);
            drop(exec);
            drop(task);
        } else {
            exec.tasks.insert(id, Slot::Idle(task));
        }
    }

    /// Moves the clock to the earliest timer and wakes everything due then.
    /// Returns `false` if no timer is set.
    fn fire_timers(&self) -> bool {
        let due = {
            let mut exec = self.exec();
            let Some((&(deadline, _), _)) = exec.timers.iter().next() else {
                return false;
            };
            let later = exec.timers.split_off(&(deadline, u64::MAX));
            let due = core::mem::replace(&mut exec.timers, later);
            (deadline, due)
        };
        let (deadline, due) = due;
        #[cfg(feature = "time")]
        {
            let mut sim = self.sim();
            if deadline > sim.clock {
                let step = deadline - sim.clock;
                sim.advance(step);
            }
        }
        #[cfg(not(feature = "time"))]
        let _ = deadline;
        for waker in due.into_values() {
            waker.wake();
        }
        true
    }

    /// Drops every task still running, and forgets timers and waiters.
    fn shutdown(&self) {
        let tasks = {
            let mut exec = self.exec();
            exec.ready.clear();
            exec.timers.clear();
            exec.signal_waiters.clear();
            core::mem::take(&mut exec.tasks)
        };
        drop(tasks);
    }
}

impl Runtime {
    /// A runtime over `sim`, scheduling with seed 0.
    pub fn new(sim: Sim) -> Self {
        Self::with_seed(sim, 0)
    }

    /// A runtime over `sim`, whose scheduling choices come from `seed`.
    ///
    /// The seed only orders tasks; `RandomU64` in the core still draws from
    /// the `Sim`'s own seed.
    pub fn with_seed(sim: Sim, seed: u64) -> Self {
        let exec = Exec {
            rng: seed,
            ..Exec::default()
        };
        Self {
            shared: Arc::new(Shared {
                sim: Mutex::new(sim),
                exec: Mutex::new(exec),
            }),
        }
    }

    /// A capability over this runtime's world, for code that sets up tasks
    /// before calling [`block_on`](Self::block_on).
    pub fn handle(&self) -> AsyncSim {
        AsyncSim {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Runs `main` to completion, with the tasks it spawns, and returns its
    /// output. Tasks still running when `main` finishes are dropped.
    ///
    /// # Panics
    ///
    /// On a [`Deadlock`], and if `main` panics.
    pub fn block_on<F: Future>(&self, main: impl FnOnce(AsyncSim) -> F) -> F::Output {
        self.try_block_on(main)
            .unwrap_or_else(|deadlock| panic!("{deadlock}"))
    }

    /// Runs `main` to completion like [`block_on`](Self::block_on), but
    /// returns a [`Deadlock`] instead of panicking on one.
    pub fn try_block_on<F: Future>(
        &self,
        main: impl FnOnce(AsyncSim) -> F,
    ) -> Result<F::Output, Deadlock> {
        let mut main = pin!(main(self.handle()));
        let main_waker = self.shared.waker(MAIN);
        self.shared.exec().ready.insert(MAIN);
        let result = loop {
            let next = self.shared.exec().pick();
            match next {
                Some(MAIN) => {
                    if let Poll::Ready(out) =
                        main.as_mut().poll(&mut Context::from_waker(&main_waker))
                    {
                        break Ok(out);
                    }
                }
                Some(id) => self.shared.run(id),
                None if self.shared.fire_timers() => {}
                None => {
                    break Err(Deadlock {
                        tasks: self.shared.exec().tasks.len(),
                    });
                }
            }
        };
        self.shared.shutdown();
        result
    }

    /// Runs `f` on the world: for setting up between runs, and for
    /// assertions afterwards.
    pub fn with_sim<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> R {
        f(&mut self.shared.sim())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

impl AsyncSim {
    /// Runs `f` on the world, outside the capability system: for tests
    /// that drive the world from inside the runtime. Do not hold on to
    /// anything across an `.await` inside `f`; it cannot await.
    pub fn with_sim<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> R {
        f(&mut self.shared.sim())
    }

    /// Delivers a Ctrl-C, waking a task waiting for one.
    #[cfg(feature = "signal")]
    pub fn raise_ctrl_c(&self) {
        self.shared.sim().raise_ctrl_c();
        self.shared.wake_signal_waiters();
    }
}

impl fmt::Debug for AsyncSim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncSim").finish_non_exhaustive()
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for AsyncSim {}
