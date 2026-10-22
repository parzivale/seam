//! Sans-effect programming.
//!
//! Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every**
//! effect: I/O, time, randomness, shared state. The core of a program is
//! written against capability bounds and never touches the outside world
//! itself, so it can be unit tested, or run, with no effects at all. Real
//! implementations or a simulation are plugged in at the edge: the seam.
//!
//! As a side effect (of the good kind), a function's bounds are an audit of
//! everything it can do.
//!
//! # Quick start
//!
//! Pick the capability groups the core uses, plus a backend to run it on.
//! Backends are never turned on for you:
//!
//! ```toml
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.1", features = ["fs", "io-std", "std"] }
//!
//! [dev-dependencies]
//! seam = { package = "seam-rs", version = "0.1", features = ["test"] }
//! ```
//!
//! ```
//! # #[cfg(all(feature = "std", feature = "test", feature = "fs", feature = "io-std"))] {
//! use std::io;
//! use seam::prelude::*;
//! use seam::ops::fs::ReadToString;
//! use seam::ops::stdio::Print;
//!
//! /// The core. It can read files and print, and nothing else.
//! fn shout<C>(c: &mut C, path: &str) -> io::Result<()>
//! where
//!     C: Inspect<ReadToString> + for<'a> Perform<Print<'a>>,
//! {
//!     let text = c.inspect(ReadToString::new(path))?;
//!     c.perform(Print(text.to_uppercase().as_bytes()))
//! }
//!
//! // In `main`, the real world:
//! //     shout(&mut seam::stdlib::Std::new(), "notes.txt")
//!
//! // In a test, a simulated one:
//! let mut sim = seam::test::Sim::new();
//! sim.add_file("/notes.txt", "hello\n");
//! shout(&mut sim, "/notes.txt").unwrap();
//! assert_eq!(sim.stdout(), b"HELLO\n");
//! # }
//! ```
//!
//! # The pieces
//!
//! **Operations** describe effects as values. Each is a type implementing
//! [`Operation`], which names what it produces. Read-only ones are also
//! [`Query`]s. Ready-made operations for the OS are in [`ops`], and you can
//! define your own (see the `custom_ops` example).
//!
//! **Capabilities** run operations. [`Inspect<Q>`] runs a query through
//! `&self`; [`Perform<O>`] runs anything through `&mut self`. Every
//! `Inspect` is also a `Perform` (for types that opt in with
//! [`Capability`], as every backend type does), so a stronger capability is
//! accepted where a weaker one is asked for. A core asks for capabilities as bounds and
//! receives one value that provides them.
//!
//! **Backends** provide capabilities over the real world, and are chosen
//! only at the edge of the program:
//!
//! | Backend | Feature | Root capability | For |
//! |---|---|---|---|
//! | `seam::stdlib` | `std` | `Std` | sync programs, blocking I/O |
//! | `seam::tokio` | `tokio` | `Tokio` | async programs, multithreaded |
//! | `seam::test` | `test` | `Sim`, `rt::AsyncSim`, `threads::ThreadSim` | tests: a deterministic simulated world, with a seeded runtime for async cores and a seeded scheduler for threaded ones |
//!
//! Backends hand out handles (files, sockets) as newtypes with no std or
//! Tokio I/O traits, so effects on those also go through the capability
//! traits.
//!
//! A [`Root`] combines backends, and a program's own effects, into one
//! capability: each part serves some [`kinds`] of operation.
//!
//! ```
//! # #[cfg(all(feature = "std", feature = "test", feature = "env", feature = "time"))] {
//! use seam::kinds;
//! use seam::ops::env::Var;
//! use seam::ops::time::Now;
//! use seam::prelude::*;
//!
//! seam::root! {
//!     struct App {
//!         std: seam::stdlib::Std => [kinds::Env],
//!         clock: seam::test::Sim => [kinds::Time], // a simulated clock
//!     }
//! }
//! let root = Root::new(App { std: seam::stdlib::Std::new(), clock: seam::test::Sim::new() });
//! let _home = root.inspect(Var::new("HOME"));
//! assert_eq!(root.inspect(Now).since_epoch().as_secs(), 0);
//! # }
//! ```
//!
//! # Async
//!
//! Async capabilities are in [`future`]. Every sync capability is also an
//! async one (again, for types that opt in with [`Capability`]), so an async core runs on `Std`, `Sim` or a hand-written fake
//! through [`block_on`](future::block_on), and on Tokio through
//! `seam::tokio::block_on`. Write `SendPerform`/`SendInspect` bounds for a
//! core that must run on a multithreaded runtime.
//!
//! # Resources
//!
//! A core never owns a file, a socket or a task. It asks for the capability
//! to [`Provide`] one, and [`acquire`](Acquire::acquire)s it for the length
//! of a closure, which borrows it and gets the capability back:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use std::io;
//! use seam::prelude::*;
//! use seam::ops::fs::CreateAtomic;
//! use seam::ops::io::WriteAll;
//!
//! fn save<C, F>(c: &mut C, text: &str) -> io::Result<()>
//! where
//!     C: Provide<CreateAtomic<F>>,
//!     F: for<'a> Perform<WriteAll<'a>>,
//! {
//!     c.acquire(CreateAtomic::new("notes.txt"), |_c, file| {
//!         file.perform(WriteAll(text.as_bytes()))
//!     })
//! }
//! # }
//! ```
//!
//! When the body returns `Ok`, the resource is committed (here, renamed
//! into place) and any error from that is returned. When it returns `Err`,
//! panics, or (with [`acquire_async`](AsyncAcquire::acquire_async)) is
//! cancelled, the resource is rolled back. Providers make dropping a
//! resource equal rolling it back, so a cancellation at any `.await` never
//! commits half a result. Tasks follow the same rule: a scope waits for its
//! task on success and aborts it otherwise. See [`acquire`] for details.
//!
//! When a body's only error path is a `?`, Rust may not be able to infer its
//! error type; end it with `io::Result::Ok(..)` (or your error type).
//!
//! # Keeping a core honest
//!
//! Nothing in the type system stops a core from calling `std::fs::read`
//! directly. The repository's `templates/clippy.toml` bans the std and Tokio
//! effect APIs; copy it into each crate that should stay sans-effect.
//!
//! # Examples
//!
//! Each example is a real program whose tests run its core in the
//! simulation. Run one with `cargo run -p seam-rs --example <name> --features
//! full,std,tokio,rt,test`, and test them all with `cargo test -p seam-rs
//! --examples --features full,std,tokio,rt,test`.
//!
//! - `guess`: a number guessing game. Randomness and stdio, with seeded,
//!   scripted tests, including a test of a playing strategy.
//! - `backup`: snapshots a directory and prunes old snapshots. Filesystem,
//!   clock and arguments, with the capabilities bundled into one trait.
//! - `echo_server`: a TCP echo server on Tokio that spawns a task per
//!   connection. Its async `Send` connection handler is tested on the
//!   in-memory network with no runtime.
//! - `custom_ops`: defining your own operations, and building a capability
//!   out of other capabilities.
//!
//! # Features
//!
//! `seam` is `no_std` with no default features: you get the traits, the
//! prelude and [`Push`].
//!
//! Features come in two independent kinds. **Capability groups** choose
//! which operations exist; **backends** choose where effects really happen.
//! No capability group turns on a backend, so a library crate can depend on
//! `seam` with only the groups its core needs, and only the final binary
//! (or its tests) picks a backend.
//!
//! | Capability group | Gives you |
//! |---|---|
//! | `time`, `random` | operation types; no std needed |
//! | `rt` | task operations (`Spawn`, `Join`, ...); no std needed |
//! | `sync` | channels and shared cells; no std needed |
//! | `alloc` | `Spawn::boxed`, `SpawnWith::boxed`: tasks of unnameable type, boxed |
//! | `fs`, `net`, `process`, `env`, `io-std`, `signal` | operation types (links std for their types) |
//! | `full` | every group above |
//!
//! | Backend | Gives you |
//! |---|---|
//! | `std` | the std backend, `seam::stdlib` |
//! | `tokio` | the Tokio backend, `seam::tokio` |
//! | `test` | the pure test backend, `seam::test`: a simulated world |
//! | `rt-multi-thread`, `test-util` | Tokio runtime options, as in Tokio; only with `tokio` |
//!
//! Each backend implements exactly the capability groups that are enabled.
//! A typical split:
//!
//! ```toml
//! # The library holding the core: operations only.
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.1", features = ["fs", "time"] }
//!
//! # The binary: also picks the real backend...
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.1", features = ["fs", "time", "std"] }
//!
//! # ...and its tests pick the simulation.
//! [dev-dependencies]
//! seam = { package = "seam-rs", version = "0.1", features = ["test"] }
//! ```
#![no_std]

pub use seam_core::*;

/// The traits you need to write and run a core, in one import.
///
/// ```
/// use seam::prelude::*;
/// ```
///
/// The capability traits ([`Inspect`], [`Perform`] and their async and
/// `Send` forms), [`Operation`] and [`Query`] for defining operations,
/// [`Provide`] and [`Acquire`] (and their async forms) for resources,
/// [`Push`], and
/// [`block_on`](crate::future::block_on). Operation types and backends are
/// imported separately, so that a module's imports also show what it can
/// do.
pub mod prelude {
    pub use seam_core::prelude::*;
}

/// The std backend.
#[cfg(feature = "std")]
pub use seam_std as stdlib;

/// The pure test backend: a simulated world whose only real effect is
/// allocation.
#[cfg(feature = "test")]
pub use seam_test as test;

/// The Tokio backend.
#[cfg(feature = "tokio")]
pub use seam_tokio as tokio;
