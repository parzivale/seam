//! A pure test backend for seam.
//!
//! [`Sim`] is a simulated world. It implements the same operations as the
//! real backends, but the clock is virtual, randomness is a seeded PRNG,
//! the filesystem and network live in memory, commands are scripted and
//! stdio is captured. Its only real effect is allocation. The crate is
//! linted with seam's own `clippy.toml`, which bans the std effect APIs.
//!
//! Everything is deterministic: the same seed and the same calls give the
//! same results, in the same order.
//!
//! ```
//! # #[cfg(all(feature = "time", feature = "random"))] {
//! use core::time::Duration;
//! use seam_core::{Inspect, Perform};
//! use seam_core::ops::random::{FillRandom, random_u64};
//! use seam_core::ops::time::{Now, Sleep};
//! use seam_test::Sim;
//!
//! fn backoff<C: for<'a> Perform<FillRandom<'a>> + Perform<Sleep>>(c: &mut C) {
//!     let ms = 100 + random_u64(c) % 100;
//!     c.perform(Sleep(Duration::from_millis(ms)));
//! }
//!
//! let mut sim = Sim::with_seed(7);
//! backoff(&mut sim); // returns at once: the clock is virtual
//! let waited = sim.inspect(Now).since_epoch();
//! assert!(waited >= Duration::from_millis(100));
//! assert_eq!(Sim::with_seed(7).run(backoff).inspect(Now).since_epoch(), waited);
//! # }
//! ```
//!
//! # Setting up and checking the world
//!
//! `Sim` has plain methods, outside the capability system, for the test to
//! arrange the world before the core runs and inspect it afterwards:
//!
//! | Group | Arrange | Assert |
//! |---|---|---|
//! | time | `advance`, `set_wall_clock` | perform `Now` |
//! | fs | `add_file`, `add_dir` | `file`, `exists` |
//! | env | `set_var`, `remove_var`, `set_args`, `set_current_dir` | inspect `Var` |
//! | net | `add_host` | bind and connect sockets |
//! | io-std | `feed_stdin` | `stdout`, `stderr`, `take_stdout` |
//! | process | `on_command` | `invocations`, `catch_exit` |
//! | signal | `raise_ctrl_c` | |
//!
//! Each row needs its feature. The same setup can be written as a world
//! file, in KDL, and loaded with `Sim::from_world`; `Sim::check_world`
//! checks the world afterwards against expectations in the same format
//! (files, output, variables, commands run, the clock). See the
//! `world_file` module.
//!
//! ```
//! # #[cfg(all(feature = "fs", feature = "env", feature = "io-std"))] {
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::env::Var;
//! use seam_core::ops::fs::{Open, ReadAt, View, WriteOnly, read_to_string, write_file};
//! use seam_core::ops::io::Write;
//! use seam_core::ops::stdio::{OpenStdout, print};
//! use seam_test::Sim;
//!
//! /// The core under test: greets `$USER` and remembers it did.
//! fn greet<C, R, W, O>(c: &mut C) -> io::Result<()>
//! where
//!     C: for<'a> Inspect<Var<'a>>
//!         + for<'a> Lend<View<'a, R>>
//!         + for<'a> Provide<Open<'a, WriteOnly, W>>
//!         + Provide<OpenStdout<O>>,
//!     R: for<'a> Inspect<ReadAt<'a>>,
//!     W: for<'a> Perform<Write<'a>>,
//!     O: for<'a> Perform<Write<'a>>,
//! {
//!     let user = c.inspect(Var::new("USER")).map(|u| u.to_string_lossy().into_owned());
//!     let user = user.unwrap_or_else(|| "stranger".into());
//!     let seen = read_to_string(c, "/tmp/seen").unwrap_or_default();
//!     let word = if seen.lines().any(|l| l == user) { "Welcome back" } else { "Hello" };
//!     print(c, format!("{word}, {user}!\n").as_bytes())?;
//!     write_file(c, "/tmp/seen", &format!("{seen}{user}\n"))
//! }
//!
//! let mut sim = Sim::new();
//! sim.set_var("USER", "ada");
//! greet(&mut sim).unwrap();
//! greet(&mut sim).unwrap();
//! assert_eq!(sim.stdout(), b"Hello, ada!\nWelcome back, ada!\n");
//! assert_eq!(sim.file("/tmp/seen").unwrap(), b"ada\nada\n");
//! # }
//! ```
//!
//! # Async cores
//!
//! `Sim` is `Send + Sync`, and its operations are synchronous, so async cores
//! (including ones with `Send` bounds, written for Tokio) run in it through
//! seam's blanket impls and [`block_on`](seam_core::future::block_on). Every
//! future is ready on its first poll.
//!
//! # Blocking operations
//!
//! Used directly, nothing in `Sim` waits. An operation that would block on
//! a real system (reading an empty socket, accepting with no pending
//! connection, waiting for Ctrl-C that was never raised) fails with
//! `io::ErrorKind::WouldBlock` instead, and `Sleep` advances the virtual
//! clock and returns.
//!
//! # Concurrency: the deterministic runtime
//!
//! For cores that spawn tasks, or whose tasks wait on each other, the `rt`
//! module runs async code over a `Sim` on a deterministic executor. There,
//! blocking operations suspend until another task makes them possible,
//! `Sleep` waits on a virtual clock that jumps straight to the next timer,
//! the next task to run is chosen from a seed so that interleavings are
//! reproducible, and a deadlock is reported rather than hanging.
//!
//! For sync cores that spawn threads, the `threads` module does the same
//! with real threads, run one at a time: every effect is a scheduling point
//! chosen from a seed, blocking operations block the simulated thread until
//! another makes them possible, and `Sleep` uses the same virtual clock.
//!
//! # Features
//!
//! `time` and `random` work without std. `fs`, `net`, `env`, `process`,
//! `io-std`, `signal` and `sync` need std's types, but still make no system
//! calls of their own. `rt` adds the deterministic runtime and threads.
//! `full` turns on everything.
#![forbid(unsafe_code)]
#![no_std]

#[cfg(feature = "std")]
extern crate std;
// So that `sim_world` names this crate as an outside crate would.
#[cfg(feature = "std")]
extern crate self as seam_test;

#[cfg(feature = "env")]
mod env;
#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "std")]
mod kdl;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "process")]
pub mod process;
#[cfg(feature = "random")]
mod random;
#[cfg(feature = "rt")]
pub mod rt;
#[cfg(feature = "signal")]
mod signal;
#[cfg(feature = "std")]
pub mod sim;
#[cfg(feature = "std")]
mod sim_world;
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
pub mod threads;
#[cfg(feature = "time")]
mod time;
#[cfg(all(feature = "time", feature = "std"))]
mod timeouts;
mod world;
#[cfg(feature = "std")]
pub mod world_file;

#[cfg(feature = "process")]
pub use process::{Exited, Invocation, catch_exit};
#[cfg(feature = "std")]
pub use world_file::{Failure, World, WorldError, WorldMismatch, check_world, load_world};

/// A simulated world: the test backend's capability.
///
/// Create one per test. Set up the world it starts in (files, variables,
/// scripted commands, stdin), hand it to the core in place of a real
/// backend, then assert on what the core left behind.
///
/// With std, a `Sim` is a handle: every clone is the same world, so a part
/// of a root that works over the simulation (a store that keeps its data in
/// a simulated file, say) is given a clone, and the test asserts through
/// any of them. Without std, a `Sim` is the world itself, and is not
/// `Clone`.
///
/// ```
/// # #[cfg(feature = "fs")] {
/// let sim = seam_test::Sim::new();
/// let mut other = sim.clone();
/// other.add_file("/tmp/a", "hi");
/// assert_eq!(sim.file("/tmp/a").unwrap(), b"hi");
/// # }
/// ```
#[cfg_attr(feature = "std", derive(Clone))]
pub struct Sim {
    #[cfg(feature = "std")]
    world: world::Shared,
    #[cfg(not(feature = "std"))]
    world: world::World,
}

impl Sim {
    /// A fresh world with seed 0.
    ///
    /// The clock reads `Instant::EPOCH`,
    /// the wall clock reads the Unix epoch, the current directory is `/`, and
    /// the filesystem holds an empty `/` and `/tmp`.
    pub fn new() -> Self {
        Self::with_seed(0)
    }

    /// A fresh world whose random numbers come from `seed`.
    pub fn with_seed(seed: u64) -> Self {
        #[allow(unused_mut)]
        let mut sim = Self {
            #[cfg(feature = "std")]
            world: world::Shared::new(world::World::new(seed)),
            #[cfg(not(feature = "std"))]
            world: world::World::new(seed),
        };
        #[cfg(feature = "fs")]
        sim.add_dir("/tmp");
        sim
    }

    /// Reads the world.
    #[cfg(feature = "std")]
    #[allow(dead_code)]
    fn state(&self) -> std::sync::MutexGuard<'_, world::World> {
        self.world.lock()
    }

    /// Changes the world.
    #[cfg(feature = "std")]
    #[allow(dead_code)]
    fn state_mut(&mut self) -> std::sync::MutexGuard<'_, world::World> {
        self.world.lock()
    }

    #[cfg(not(feature = "std"))]
    #[allow(dead_code)]
    fn state(&self) -> &world::World {
        &self.world
    }

    #[cfg(not(feature = "std"))]
    #[allow(dead_code)]
    fn state_mut(&mut self) -> &mut world::World {
        &mut self.world
    }

    /// Runs `core` against this world and returns the world, for chaining
    /// assertions.
    pub fn run(mut self, core: impl FnOnce(&mut Self)) -> Self {
        core(&mut self);
        self
    }
}

#[cfg(feature = "rt")]
impl Sim {
    /// Sets what `AvailableParallelism` reports. The default is 1.
    pub fn set_parallelism(&mut self, n: core::num::NonZeroUsize) -> &mut Self {
        self.world.lock().parallelism = n;
        self
    }
}

#[cfg(feature = "rt")]
impl seam_core::Inspect<seam_core::ops::task::AvailableParallelism> for Sim {
    fn inspect(&self, _: seam_core::ops::task::AvailableParallelism) -> core::num::NonZeroUsize {
        self.world.lock().parallelism
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Sim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sim").finish_non_exhaustive()
    }
}

/// Builds an `io::Error` with a message that names the simulation.
#[cfg(any(
    feature = "fs",
    feature = "net",
    feature = "process",
    feature = "signal"
))]
fn error(kind: seam_core::io::ErrorKind, msg: &'static str) -> seam_core::io::Error {
    seam_core::io::Error::new(kind, msg)
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Sim {}
