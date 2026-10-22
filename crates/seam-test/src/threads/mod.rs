//! Deterministic threads, for testing sync cores that spawn threads.
//!
//! [`Threads`] runs a sync core over a [`Sim`] with a scheduler in charge of
//! its threads. The core gets a [`ThreadSim`]: a cloneable capability over
//! the same world, so each thread it spawns can have its own.
//!
//! ```
//! # #[cfg(all(feature = "time", feature = "io-std"))] {
//! use std::time::Duration;
//! use seam_core::prelude::*;
//! use seam_core::ops::stdio::Print;
//! use seam_core::ops::task::{Join, Spawn};
//! use seam_core::ops::time::Sleep;
//! use seam_test::Sim;
//! use seam_test::threads::{JoinHandle, Threads};
//!
//! let threads = Threads::new(Sim::new());
//! threads.run(|mut sim| {
//!     let mut worker = sim.clone();
//!     let late = Spawn::<_, JoinHandle<()>>::new(move || {
//!         worker.perform(Sleep(Duration::from_secs(60)));
//!         let _ = worker.perform(Print(b"a minute later\n"));
//!     });
//!     sim.acquire(late, |sim, thread| {
//!         let _ = sim.perform(Print(b"right away\n"));
//!         thread.perform(Join::new())
//!     })
//!     .unwrap();
//! });
//! // A minute of simulated sleep takes no time at all.
//! assert_eq!(threads.with_sim(|s| s.take_stdout()), b"right away\na minute later\n");
//! # }
//! ```
//!
//! # How it schedules
//!
//! Every simulated thread runs on a real OS thread, but only one runs at a
//! time. Each effect a thread performs is a scheduling point: the scheduler
//! picks, from the runtime's seed, which runnable thread continues. Cores
//! can only share state through seam's cells and channels, which are
//! effects too, so switching at every effect covers every interleaving that
//! can change what a core observes. The same seed always gives the same
//! interleaving; sweeping seeds explores others.
//!
//! # Waiting
//!
//! Operations that would block a real thread block a simulated one, and are
//! woken by exactly what they wait for: receiving on an empty channel,
//! sending to a full one, joining a thread, reading a stream with no data,
//! accepting with no connection pending, waiting for Ctrl-C, and `Sleep`.
//! When no thread can run, the clock jumps straight to the next timer. When
//! no thread can run and no timer is set, that is a deadlock, which
//! [`Threads::run`] reports by panicking and [`Threads::try_run`] by
//! returning [`Deadlock`].
//!
//! # Threads
//!
//! `ThreadSim` provides `Spawn` and `SpawnSet` with closures, and its
//! listeners provide `SpawnWith`, with the same rules as `seam-std`:
//! handles cannot be aborted, and a scope that fails waits for its threads
//! unless they were detached. A panic in a thread is caught and reported as
//! `JoinError::Panicked`. When the main closure returns, threads still
//! running are stopped.
#![allow(clippy::disallowed_types)] // the scheduler's own shared state

use core::fmt;
use core::time::Duration;
use std::boxed::Box;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Wake, Waker};
use std::vec::Vec;

use crate::Sim;

#[cfg(feature = "net")]
pub mod net;
mod ops;
#[cfg(feature = "sync")]
pub mod sync;
mod task;

pub use task::{JoinHandle, ThreadSet};

/// Runs a sync core, and the threads it spawns, deterministically over a
/// [`Sim`].
///
/// Set the world up with `Sim`'s methods first, hand it over with
/// [`Threads::new`], run the core with [`run`](Self::run), and inspect the
/// world afterwards with [`with_sim`](Self::with_sim).
pub struct Threads {
    shared: Arc<Shared>,
}

/// The capability a core running under [`Threads`] performs effects
/// through. Cheap to clone; every clone works on the same world, so move
/// one into each thread.
///
/// It performs the same operations as [`Sim`], except that operations which
/// would block a real thread block the simulated one until they can
/// finish. Sockets and channels acquired through it are this module's own
/// ([`net`], [`sync`]).
#[derive(Clone)]
pub struct ThreadSim {
    pub(crate) shared: Arc<Shared>,
}

/// Every thread is blocked, and nothing it waits for can ever happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadlock {
    /// How many threads were blocked, besides the main one.
    pub threads: usize,
}

impl fmt::Display for Deadlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seam-test: deadlock: the main thread and {} other thread(s) are all blocked, and no timer is set",
            self.threads
        )
    }
}

impl core::error::Error for Deadlock {}

pub(crate) type ThreadId = u64;

const MAIN: ThreadId = 0;

pub(crate) struct Shared {
    sim: Mutex<Sim>,
    state: Mutex<State>,
    turn: Condvar,
    /// The OS threads behind spawned simulated threads, joined at the end.
    os_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Threads waiting for Ctrl-C.
    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    signal_waiters: Mutex<Vec<Waker>>,
}

#[derive(Default)]
struct State {
    /// The scheduler's SplitMix64 state, separate from `RandomU64`'s.
    rng: u64,
    /// The one thread allowed to run.
    running: ThreadId,
    runnable: BTreeSet<ThreadId>,
    alive: BTreeSet<ThreadId>,
    next_id: ThreadId,
    /// Sleeping threads, by deadline, then registration order.
    timers: BTreeMap<(Duration, u64), ThreadId>,
    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    next_timer: u64,
    halt: Option<Halt>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// The main closure finished: stop everything else.
    Done,
    /// Nothing can ever run again.
    Deadlock(usize),
}

/// The payload a simulated thread unwinds with when the run halts.
pub(crate) struct Halted;

/// Unwinds the calling thread because the run is halting. Does not run the
/// panic hook, so nothing is printed.
pub(crate) fn halt() -> ! {
    resume_unwind(Box::new(Halted))
}

std::thread_local! {
    static CURRENT: Cell<Option<ThreadId>> = const { Cell::new(None) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The simulated thread the calling OS thread is running.
pub(crate) fn current() -> ThreadId {
    CURRENT.with(Cell::get).expect(
        "seam-test: a ThreadSim operation was performed outside Threads::run, \
         or on a thread not spawned through it",
    )
}

/// Marks a thread runnable when woken.
struct ThreadWaker {
    id: ThreadId,
    shared: Weak<Shared>,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(shared) = self.shared.upgrade() {
            let mut st = lock(&shared.state);
            if st.alive.contains(&self.id) {
                st.runnable.insert(self.id);
            }
        }
    }
}

impl State {
    fn next_random(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl Shared {
    pub(crate) fn sim(&self) -> MutexGuard<'_, Sim> {
        lock(&self.sim)
    }

    pub(crate) fn waker(self: &Arc<Self>) -> Waker {
        Waker::from(Arc::new(ThreadWaker {
            id: current(),
            shared: Arc::downgrade(self),
        }))
    }

    /// Whether the run is stopping, so that drops must not block.
    pub(crate) fn halting(&self) -> bool {
        lock(&self.state).halt.is_some()
    }

    /// Chooses who runs next and hands over, with `st` locked. Advances
    /// the clock to the next timer when nobody is runnable, and halts on a
    /// deadlock.
    fn hand_over(&self, st: &mut State) {
        loop {
            if !st.runnable.is_empty() {
                let n = (st.next_random() % st.runnable.len() as u64) as usize;
                st.running = *st.runnable.iter().nth(n).expect("n is in range");
                break;
            }
            let Some((&(deadline, _), _)) = st.timers.iter().next() else {
                let blocked = st.alive.iter().filter(|&&id| id != MAIN).count();
                st.halt = Some(Halt::Deadlock(blocked));
                break;
            };
            let later = st.timers.split_off(&(deadline, u64::MAX));
            let due = core::mem::replace(&mut st.timers, later);
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
            for id in due.into_values() {
                if st.alive.contains(&id) {
                    st.runnable.insert(id);
                }
            }
        }
        self.turn.notify_all();
    }

    /// Waits until it is `me`'s turn. Returns `false`, instead, if the run
    /// halts.
    fn wait_turn(&self, me: ThreadId) -> bool {
        let mut st = lock(&self.state);
        while st.running != me && st.halt.is_none() {
            st = self.turn.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.halt.is_none()
    }

    /// Hands over to the scheduler, after `before` adjusts the state, and
    /// waits for the calling thread's next turn. Returns `false` if the run
    /// halts, before or during the wait.
    fn yield_turn(&self, before: impl FnOnce(&mut State, ThreadId)) -> bool {
        let me = current();
        let mut st = lock(&self.state);
        if st.halt.is_some() {
            return false;
        }
        before(&mut st, me);
        self.hand_over(&mut st);
        drop(st);
        self.wait_turn(me)
    }

    /// A scheduling point: lets the scheduler pick another runnable thread
    /// before the calling one continues. Unwinds if the run halts.
    pub(crate) fn switch(&self) {
        if !self.yield_turn(|_, _| {}) {
            halt();
        }
    }

    /// Blocks until `poll` returns `Some`. `poll` gets a waker to register
    /// with whatever it waits on. Unwinds if the run halts.
    pub(crate) fn wait_until<R>(self: &Arc<Self>, poll: impl FnMut(&Waker) -> Option<R>) -> R {
        self.try_wait_until(poll).unwrap_or_else(|| halt())
    }

    /// Blocks like [`wait_until`](Self::wait_until), but returns `None`
    /// instead of unwinding if the run halts: for waiting in a `Drop`,
    /// which may already be unwinding.
    pub(crate) fn try_wait_until<R>(
        self: &Arc<Self>,
        mut poll: impl FnMut(&Waker) -> Option<R>,
    ) -> Option<R> {
        let waker = self.waker();
        loop {
            if let Some(r) = poll(&waker) {
                return Some(r);
            }
            if !self.yield_turn(|st, me| {
                st.runnable.remove(&me);
            }) {
                return None;
            }
        }
    }

    /// Blocks the calling thread until the clock reaches `deadline`.
    #[cfg(feature = "time")]
    pub(crate) fn sleep_until(self: &Arc<Self>, deadline: Duration) {
        let _ = self.wait_until_deadline(deadline, |_| None::<()>);
    }

    /// Blocks until `poll` returns `Some`, for at most `after` on the
    /// virtual clock; `None` if the time runs out first. Unwinds if the
    /// run halts.
    #[cfg(feature = "time")]
    pub(crate) fn wait_within<R>(
        self: &Arc<Self>,
        after: Duration,
        poll: impl FnMut(&Waker) -> Option<R>,
    ) -> Option<R> {
        let deadline = self.deadline_after(after);
        self.wait_until_deadline(deadline, poll)
    }

    /// The point on the virtual clock `after` from now.
    #[cfg(feature = "time")]
    pub(crate) fn deadline_after(&self, after: Duration) -> Duration {
        self.sim().clock.saturating_add(after)
    }

    /// Blocks until `poll` returns `Some`, or until the clock reaches
    /// `deadline`; `None` if it does first.
    #[cfg(feature = "time")]
    pub(crate) fn wait_until_deadline<R>(
        self: &Arc<Self>,
        deadline: Duration,
        mut poll: impl FnMut(&Waker) -> Option<R>,
    ) -> Option<R> {
        /// Removes the timer however the wait ends, so a wait that did not
        /// time out does not move the clock later.
        struct Timer<'a>(&'a Shared, Option<(Duration, u64)>);
        impl Drop for Timer<'_> {
            fn drop(&mut self) {
                if let Some(key) = self.1.take() {
                    lock(&self.0.state).timers.remove(&key);
                }
            }
        }

        let me = current();
        let mut timer = Timer(self, None);
        self.wait_until(|waker| {
            if let Some(r) = poll(waker) {
                return Some(Some(r));
            }
            if self.sim().clock >= deadline {
                return Some(None);
            }
            if timer.1.is_none() {
                let mut st = lock(&self.state);
                st.next_timer += 1;
                let key = (deadline, st.next_timer);
                st.timers.insert(key, me);
                timer.1 = Some(key);
            }
            None
        })
    }

    /// Starts `work` as a new simulated thread, which first waits for its
    /// turn.
    pub(crate) fn spawn(self: &Arc<Self>, work: impl FnOnce() + Send + 'static) {
        let id = {
            let mut st = lock(&self.state);
            if st.halt.is_some() {
                drop(st);
                halt();
            }
            st.next_id += 1;
            let id = st.next_id;
            st.alive.insert(id);
            st.runnable.insert(id);
            id
        };
        let shared = Arc::clone(self);
        let os = std::thread::spawn(move || {
            CURRENT.with(|c| c.set(Some(id)));
            let ran = catch_unwind(AssertUnwindSafe(|| {
                if shared.wait_turn(id) {
                    work();
                }
            }));
            let mut st = lock(&shared.state);
            st.alive.remove(&id);
            st.runnable.remove(&id);
            if st.halt.is_none() {
                shared.hand_over(&mut st);
            }
            drop(st);
            // `work` catches the core's own panics; anything else here is
            // the run halting.
            drop(ran);
        });
        lock(&self.os_threads).push(os);
        self.switch();
    }

    #[cfg_attr(not(feature = "signal"), allow(dead_code))]
    pub(crate) fn wait_for_signal(&self, waker: &Waker) {
        let mut waiters = lock(&self.signal_waiters);
        if !waiters.iter().any(|w| w.will_wake(waker)) {
            waiters.push(waker.clone());
        }
    }

    /// Stops every thread still alive and waits for their OS threads.
    fn shutdown(&self) {
        {
            let mut st = lock(&self.state);
            if st.halt.is_none() {
                st.halt = Some(Halt::Done);
            }
            self.turn.notify_all();
        }
        let os = core::mem::take(&mut *lock(&self.os_threads));
        for t in os {
            let _ = t.join();
        }
    }
}

impl Threads {
    /// Runs threads over `sim`, scheduling with seed 0.
    pub fn new(sim: Sim) -> Self {
        Self::with_seed(sim, 0)
    }

    /// Runs threads over `sim`, with scheduling choices drawn from `seed`.
    /// The seed only orders threads; `RandomU64` still draws from the
    /// `Sim`'s own seed.
    pub fn with_seed(sim: Sim, seed: u64) -> Self {
        let state = State {
            rng: seed,
            running: MAIN,
            runnable: BTreeSet::from([MAIN]),
            alive: BTreeSet::from([MAIN]),
            ..State::default()
        };
        Self {
            shared: Arc::new(Shared {
                sim: Mutex::new(sim),
                state: Mutex::new(state),
                turn: Condvar::new(),
                os_threads: Mutex::new(Vec::new()),
                signal_waiters: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Runs `main` on the calling thread as the main simulated thread, with
    /// the threads it spawns, and returns its result. Threads still running
    /// when `main` returns are stopped.
    ///
    /// A `Threads` runs once; make a new one for each run.
    ///
    /// # Panics
    ///
    /// On a [`Deadlock`], if `main` panics, and if called a second time.
    pub fn run<R>(&self, main: impl FnOnce(ThreadSim) -> R) -> R {
        self.try_run(main)
            .unwrap_or_else(|deadlock| panic!("{deadlock}"))
    }

    /// Runs `main` like [`run`](Self::run), but returns a [`Deadlock`]
    /// instead of panicking on one.
    pub fn try_run<R>(&self, main: impl FnOnce(ThreadSim) -> R) -> Result<R, Deadlock> {
        assert!(
            !self.shared.halting(),
            "seam-test: a Threads runs only once"
        );
        let previous = CURRENT.with(|c| c.replace(Some(MAIN)));
        let sim = ThreadSim {
            shared: Arc::clone(&self.shared),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| main(sim)));
        let halt = lock(&self.shared.state).halt;
        self.shared.shutdown();
        CURRENT.with(|c| c.set(previous));
        match outcome {
            Ok(value) => Ok(value),
            Err(payload) => match (payload.downcast::<Halted>(), halt) {
                (Ok(_), Some(Halt::Deadlock(threads))) => Err(Deadlock { threads }),
                (Ok(_), _) => unreachable!("only a deadlock halts the main thread"),
                (Err(payload), _) => resume_unwind(payload),
            },
        }
    }

    /// Runs `f` on the world: for setting up and for assertions afterwards.
    pub fn with_sim<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> R {
        f(&mut self.shared.sim())
    }
}

impl Drop for Threads {
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

impl fmt::Debug for Threads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Threads").finish_non_exhaustive()
    }
}

impl ThreadSim {
    /// Runs `f` on the world, outside the capability system: for tests that
    /// drive the world from inside a run.
    pub fn with_sim<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> R {
        f(&mut self.shared.sim())
    }

    /// Delivers a Ctrl-C, waking a thread waiting for one.
    #[cfg(feature = "signal")]
    pub fn raise_ctrl_c(&self) {
        self.shared.sim().raise_ctrl_c();
        for w in core::mem::take(&mut *lock(&self.shared.signal_waiters)) {
            w.wake();
        }
    }
}

impl fmt::Debug for ThreadSim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadSim").finish_non_exhaustive()
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for ThreadSim {}
