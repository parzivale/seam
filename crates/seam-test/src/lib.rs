//! A pure test backend for seam.
//!
//! A simulated `World` implements the same operations as the real
//! backends, but the clock is virtual, randomness is a seeded PRNG, the
//! filesystem and network live in memory, commands are scripted and stdio
//! is captured. Its only real effect is allocation. The crate is linted
//! with seam's own `clippy.toml`, which bans the std effect APIs.
//!
//! Every effect is simulated on its own key, against a state of the world
//! (the clock, the filesystem, ...), the same way a program simulates its
//! own effects: see the `sim` module. `world!` builds a world that grants
//! exactly the effects it lists, and `Sim` is the world of every built-in
//! one.
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
//! Every world has plain methods, outside the capability system, for the
//! test to arrange the world before the core runs and inspect it
//! afterwards:
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
//! file, in KDL, and loaded with `Sim::from_world` or `load_world`;
//! `check_world` checks the world afterwards against expectations in the
//! same format, and `to_world` writes the whole world out in it. See the
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
//! A world is `Send + Sync`, and implements the async and `Send` forms of
//! its operations, so async cores (including ones with `Send` bounds,
//! written for Tokio) run in it through
//! [`block_on`](seam_core::future::block_on). Every future is ready on its
//! first poll.
//!
//! # Blocking operations
//!
//! Used directly, nothing in a world waits. An operation that would block on
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
//! Worlds need std. Without it, `time` and `random` give a small `Sim` of
//! a virtual clock and seeded random numbers, with no allocation. `fs`,
//! `net`, `env`, `process`, `io-std`, `signal` and `sync` need std's types,
//! but still make no system calls of their own. `rt` adds the
//! deterministic runtime and threads. `full` turns on everything.
#![forbid(unsafe_code)]
#![no_std]

#[cfg(feature = "std")]
extern crate std;

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
#[cfg(feature = "io-std")]
pub mod stdio;
#[cfg(feature = "sync")]
pub mod sync;
#[cfg(feature = "rt")]
mod tasks;
#[cfg(feature = "rt")]
pub mod threads;
#[cfg(any(feature = "time", feature = "fs"))]
mod time;
#[cfg(all(feature = "time", feature = "std"))]
mod timeouts;
#[cfg(feature = "std")]
pub mod world_file;

#[cfg(feature = "process")]
pub use process::{Exited, Invocation, catch_exit};
#[cfg(feature = "std")]
pub use sim::{Sim, World};
#[cfg(feature = "std")]
pub use world_file::{Failure, WorldError, WorldMismatch};

/// A simulated world without std: a virtual clock and seeded random
/// numbers. With std, `Sim` is the world of every built-in
/// effect.
#[cfg(not(feature = "std"))]
#[derive(Debug, Default)]
pub struct Sim {
    #[cfg(feature = "time")]
    clock: core::time::Duration,
    #[cfg(feature = "random")]
    rng: random::SplitMix64,
}

#[cfg(not(feature = "std"))]
impl Sim {
    /// A fresh world with seed 0. The clock reads `Instant::EPOCH`.
    pub fn new() -> Self {
        Self::with_seed(0)
    }

    /// A fresh world whose random numbers come from `seed`.
    pub fn with_seed(seed: u64) -> Self {
        #[cfg(not(feature = "random"))]
        let _ = seed;
        Self {
            #[cfg(feature = "time")]
            clock: core::time::Duration::ZERO,
            #[cfg(feature = "random")]
            rng: random::SplitMix64(seed),
        }
    }

    /// Runs `core` against this world and returns the world, for chaining
    /// assertions.
    pub fn run(mut self, core: impl FnOnce(&mut Self)) -> Self {
        core(&mut self);
        self
    }
}

// Without std, `Sim` implements its operations itself, and opts in to
// seam's lifts.
#[cfg(not(feature = "std"))]
impl seam_core::Capability for Sim {}

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
