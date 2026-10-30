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
//! Pick the capability groups the core uses, plus the handlers to run it
//! on. Handler crates are never turned on for you:
//!
//! ```toml
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.7", features = ["fs", "io-std", "std"] }
//!
//! [dev-dependencies]
//! seam = { package = "seam-rs", version = "0.7", features = ["test"] }
//! ```
//!
//! ```
//! # #[cfg(all(feature = "std", feature = "test", feature = "fs", feature = "io-std"))] {
//! use seam::io;
//! use seam::prelude::*;
//! use seam::ops::fs::{ReadAt, View, read_to_string};
//! use seam::ops::io::Write;
//! use seam::ops::stdio::{OpenStdout, print};
//!
//! /// The core. It can read files and print, and nothing else. `F` and `O`
//! /// are the handler's file and stdout types, inferred.
//! fn shout<C, F, O>(c: &mut C, path: &str) -> io::Result<()>
//! where
//!     C: for<'a> Lend<View<'a, F>> + Provide<OpenStdout<O>>,
//!     F: for<'a> Inspect<ReadAt<'a>>,
//!     O: for<'a> Perform<Write<'a>>,
//! {
//!     let text = read_to_string(c, path)?;
//!     print(c, text.to_uppercase().as_bytes())
//! }
//!
//! // In `main`, the real world:
//! //     shout(&mut seam::stdlib::Std, "notes.txt")
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
//! [`Query`]s. Ready-made operations for the OS are in [`ops`]: the
//! primitives a handler provides, and beside them the helpers built from
//! those (`read_file`, `write_all`, lines through `BufReader`, `print`, ...),
//! in sync, `_async` and `_send` forms. You can define your own operations
//! too (see the `custom_ops` example).
//!
//! **Capabilities** run operations. [`Inspect<Q>`] runs a query through
//! `&self`; [`Perform<O>`] runs anything through `&mut self`. Every
//! `Inspect` is also a `Perform` (for types that opt in with
//! [`Capability`], as every handler type does), so a stronger capability is
//! accepted where a weaker one is asked for. A core asks for capabilities
//! as bounds and receives one value that provides them.
//!
//! **Handlers** perform effects, over the real world or a simulated one,
//! and are chosen only at the edge of the program:
//!
//! | Crate | Feature | Handler | For |
//! |---|---|---|---|
//! | `seam::stdlib` | `std` | `Std` | sync programs, blocking I/O |
//! | `seam::tokio` | `tokio` | `Tokio` | async programs, multithreaded |
//! | `seam::test` | `test` | `Sim`, `rt::AsyncSim`, `threads::ThreadSim` | tests: a deterministic simulated world, with a seeded runtime for async cores and a seeded scheduler for threaded ones |
//!
//! Handlers hand out handles (files, sockets) as newtypes with no std or
//! Tokio I/O traits, so effects on those also go through the capability
//! traits.
//!
//! A [`Root`] combines handlers, and a program's own effects, into one
//! capability: each part lists the effects it serves, by their keys in
//! [`effects`], and the root grants a core exactly those.
//!
//! ```
//! # #[cfg(all(feature = "std", feature = "test", feature = "env", feature = "time"))] {
//! use seam::effects as fx;
//! use seam::ops::env::Var;
//! use seam::ops::time::Now;
//! use seam::prelude::*;
//!
//! use seam::stdlib::Std;
//! use seam::test::Sim;
//!
//! let root = seam::root![
//!     Std<fx::Var>,
//!     Sim<fx::Now>::new(), // a simulated clock
//! ];
//! let _home = root.inspect(Var::new("HOME"));
//! assert_eq!(root.inspect(Now).since_epoch().as_secs(), 0);
//! # }
//! ```
//!
//! # Async
//!
//! Async capabilities are in [`future`]. Every sync capability is also an
//! async one (again, for types that opt in with [`Capability`]), so an
//! async core runs on `Std`, `Sim` or a hand-written fake through
//! [`block_on`](future::block_on), and on Tokio through
//! `seam::tokio::block_on`. Write `SendPerform`, `SendInspect`,
//! `SendProvide` and `SendLend` bounds for a core that must run on a
//! multithreaded runtime; its scopes are `acquire_send` and `lend_send`,
//! from [`SendScope`].
//!
//! # Resources
//!
//! A core never owns a file, a socket or a task. It asks for the capability
//! to [`Provide`] one, and [`acquire`](Provide::acquire)s it for the length
//! of a closure, which borrows it and gets the capability back:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use seam::io;
//! use seam::prelude::*;
//! use seam::ops::fs::CreateAtomic;
//! use seam::ops::io::{Write, write_all};
//!
//! fn save<C, F>(c: &mut C, text: &str) -> io::Result<()>
//! where
//!     C: for<'a> Provide<CreateAtomic<'a, F>>,
//!     F: for<'a> Perform<Write<'a>>,
//! {
//!     c.acquire(CreateAtomic::new("notes.txt"), |_c, file| {
//!         write_all(file, text.as_bytes())
//!     })
//! }
//! # }
//! ```
//!
//! When the body returns `Ok`, the resource is committed (here, renamed
//! into place) and any error from that is returned. When it returns `Err`,
//! panics, or (with [`acquire_async`](AsyncProvide::acquire_async)) is
//! cancelled, the resource is rolled back. Providers make dropping a
//! resource equal rolling it back, so a cancellation at any `.await` never
//! commits half a result. Tasks follow the same rule: a scope waits for its
//! task on success and aborts it otherwise.
//!
//! Opening and closing are operations, [`Opening`] and [`Closing`], and
//! `Provide` is the pair of them, so a scope works wherever operations do:
//! async, through a [`Root`], through a wrapper. A scope that changes
//! nothing outside itself, such as reading a file through a
//! [`View`](ops::fs::View), is a query: [`Lend`] it, from shared access.
//! See [`acquire`] for details.
//!
//! When a body's only error path is a `?`, Rust may not be able to infer its
//! error type; end it with `io::Result::Ok(..)` (or your error type).
//!
//! # Guides
//!
//! With the `docs` feature (on docs.rs, always), the `_docs` module holds
//! the long-form documentation: what seam is in effect-system terms, usage
//! guides from a first core to simulated effects of your own, the world-file
//! format, and how seam works inside.
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
//! - `word_count`: a worker pool on OS threads, tested on simulated threads
//!   under many schedules, with a world file for its setup and
//!   expectations.
//!
//! # Features
//!
//! `seam` is `no_std` with no default features: you get the traits, the
//! prelude and [`Push`].
//!
//! Features come in two independent kinds. **Capability groups** choose
//! which operations exist; **handler crates** bring in the handlers that
//! perform them. No capability group turns on a handler crate, so a library
//! crate can depend on `seam` with only the groups its core needs, and only
//! the final binary (or its tests) picks handlers.
//!
//! | Capability group | Gives you |
//! |---|---|
//! | `time`, `random` | operation types; no std needed |
//! | `rt` | task operations (`Spawn`, `Join`, ...); no std needed |
//! | `sync` | channels and shared cells; no std needed |
//! | `alloc` | `Spawn::boxed`, `SpawnWith::boxed` (tasks of unnameable type, boxed); the helpers returning owned data (`read_to_end`, `resolve`, ...) |
//! | `io`, `net`, `io-std`, `signal` | operation types; no std needed |
//! | `fs`, `process`, `env` | operation types (links std for `Path`, `Command`, `OsString`) |
//! | `full` | every group above |
//!
//! | Handler crate | Gives you |
//! |---|---|
//! | `std` | `Std`, the std handler, `seam::stdlib`: the environment, randomness, and sync files, network, processes |
//! | `tokio` | `Tokio`, the Tokio handler, `seam::tokio` |
//! | `smol` | `Smol`, the smol handler, `seam::smol` |
//! | `futures` | `Futures` and `Executor`, the futures-rs handlers, `seam::futures`: channels, locks and tasks on any executor |
//! | `test` | `Sim`, the pure test handler, `seam::test`: a simulated world (and the wrappers' world-file blocks) |
//! | `rt-multi-thread`, `test-util` | Tokio runtime options, as in Tokio; only with `tokio` |
//!
//! | Wrapper | Gives you |
//! |---|---|
//! | `jiff`, `jiff-bundled` | time zones over jiff, `seam::jiff`; `jiff-bundled` adds the compiled-in tz database |
//! | `rand`, `uuid`, `dirs` | rand on seam's randomness, UUIDs, platform directories |
//! | `tracing` | logging as an effect, `seam::tracing` |
//! | `notify` | file watching, `seam::notify` |
//! | `reqwest` | HTTP, `seam::reqwest`, with reqwest and rustls |
//! | `sqlx` | SQL, `seam::sqlx`: SQLite on Tokio; with `smol`, on smol too |
//! | `ash` | the GPU over Vulkan, `seam::ash` |
//! | `unix`, `rustix`, `nix` | Unix effects, `seam::unix`, performed with rustix or nix |
//!
//! A program on Tokio or smol takes the environment and randomness from
//! `std`, in the same root: `seam::root![Std<fx::Var>, Smol<fx::Sleep>]`.
//! Each wrapper's handler sits beside any of these, under any executor;
//! each one's docs show it in a program.
//!
//! Each handler implements the operations of exactly the capability groups
//! that are enabled.
//! A typical split:
//!
//! ```toml
//! # The library holding the core: operations only.
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.7", features = ["fs", "time"] }
//!
//! # The binary: also picks the real handlers...
//! [dependencies]
//! seam = { package = "seam-rs", version = "0.7", features = ["fs", "time", "std"] }
//!
//! # ...and its tests pick the simulation.
//! [dev-dependencies]
//! seam = { package = "seam-rs", version = "0.7", features = ["test"] }
//! ```
#![forbid(unsafe_code)]
#![no_std]

#[cfg(feature = "docs")]
pub mod _docs;

pub use seam_core::*;

/// The traits you need to write and run a core, in one import.
///
/// ```
/// use seam::prelude::*;
/// ```
///
/// The capability traits ([`Inspect`], [`Perform`] and their async and
/// `Send` forms), [`Operation`] and [`Query`] for defining operations,
/// [`Provide`] and [`Lend`] (and their async forms, and [`SendScope`]) for
/// resources,
/// [`Push`], and
/// [`block_on`](crate::future::block_on). Operation types and handlers are
/// imported separately, so that a module's imports also show what it can
/// do.
pub mod prelude {
    pub use seam_core::prelude::*;
}

/// The std handler.
#[cfg(feature = "std")]
pub use seam_std as stdlib;

/// The pure test handler: a simulated world whose only real effect is
/// allocation.
#[cfg(feature = "test")]
pub use seam_test as test;

/// The Tokio handler.
#[cfg(feature = "tokio")]
pub use seam_tokio as tokio;

/// The futures-rs handler: channels, cells and tasks on any executor,
/// futures-io and `Stream` adapters, and concurrent async helpers.
#[cfg(feature = "futures")]
pub use seam_futures as futures;

/// Time zones as effects, over jiff: looked up through the root's
/// filesystem, so tests decide what they are.
#[cfg(feature = "jiff")]
pub use seam_jiff as jiff;

/// The smol handler.
#[cfg(feature = "smol")]
pub use seam_smol as smol;

/// rand on seam's randomness.
#[cfg(feature = "rand")]
pub use seam_rand as rand;

/// UUIDs from seam's randomness and clock.
#[cfg(feature = "uuid")]
pub use seam_uuid as uuid;

/// Platform directories, from seam's environment.
#[cfg(feature = "dirs")]
pub use seam_dirs as dirs;

/// Logging as an effect, sent to tracing or captured.
#[cfg(feature = "tracing")]
pub use seam_tracing as tracing;

/// File watching, from the OS or by rescanning.
#[cfg(feature = "notify")]
pub use seam_notify as notify;

/// HTTP requests as an effect, with reqwest or scripted.
#[cfg(feature = "reqwest")]
pub use seam_reqwest as reqwest;

/// SQL queries and transactions as effects, with sqlx.
#[cfg(feature = "sqlx")]
pub use seam_sqlx as sqlx;

/// The GPU as effects, over Vulkan through ash, and simulated.
#[cfg(feature = "ash")]
pub use seam_ash as ash;

/// Unix effects: mounts, permissions, links, IDs, signals.
#[cfg(feature = "unix")]
pub use seam_unix as unix;

/// Unix effects performed with rustix.
#[cfg(feature = "rustix")]
pub use seam_rustix as rustix;

/// Unix effects performed with nix.
#[cfg(feature = "nix")]
pub use seam_nix as nix;
