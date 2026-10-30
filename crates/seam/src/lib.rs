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
//! **Handlers** perform operations: a handler for an operation is any type
//! that implements `Perform` or `Inspect` for it. None is special. The
//! crates below each pair a family of effects with handlers for them, real
//! and simulated, and a program's own handlers (a fake, a cache, a client
//! of its own service) are written the same way. Handles a handler lends
//! (files, sockets, transactions) are handlers too, for the operations on
//! them, and expose nothing else, so every effect goes through the
//! capability traits.
//!
//! A [`Root`] is how a program chooses, at its edge: one capability built
//! from handlers, each listed with the effects it serves (by their keys in
//! [`effects`]), granting a core exactly those. A root mixes real handlers,
//! simulated ones and the program's own, sync and async:
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
//!     Std<fx::Var>,        // the real environment
//!     Sim<fx::Now>::new(), // a simulated clock
//! ];
//! let _home = root.inspect(Var::new("HOME"));
//! assert_eq!(root.inspect(Now).since_epoch().as_secs(), 0);
//! # }
//! ```
//!
//! # Effects and their handlers
//!
//! | Module (feature) | Effects | Handlers |
//! |---|---|---|
//! | [`ops`] | the OS: files, network, processes, stdio, environment, time, randomness, tasks, channels, shared cells, signals | `Std` (`std`); `Tokio` (`tokio`) and `Smol` (`smol`), async; `Futures` and `Executor` (`futures`) for channels, cells and tasks on any executor; `Sim`, `rt::AsyncSim` and `threads::ThreadSim` (`test`), a simulated world with a seeded runtime and a seeded thread scheduler |
//! | `seam::jiff` (`jiff`) | time zones | `TzDb`, over any filesystem; `Bundled` (`jiff-bundled`) |
//! | `seam::tracing` (`tracing`) | `Log`, `Span` | `Tracing`; `Discard`; `Captured` |
//! | `seam::notify` (`notify`) | `Watch`, `NextEvent` | `Notify` (the OS); `Polling` over any filesystem |
//! | `seam::reqwest` (`reqwest`) | `Fetch` | `Reqwest`; `Scripted` |
//! | `seam::sqlx` (`sqlx`) | `Execute`, `Fetch`, `Begin`, `Acquire` | `Sqlx`, and `Sqlx::memory` (in-memory SQLite) |
//! | `seam::ash` (`ash`) | the GPU: buffers, images, samplers, programs, draws, dispatches | `Ash` (Vulkan); `SimGpu` |
//! | `seam::unix` (`unix`, `rustix`, `nix`) | mounts, permissions, links, IDs, signals | `Rustix`, `Nix`; `SimUnix` |
//! | `seam::rand`, `seam::uuid`, `seam::dirs` (`rand`, `uuid`, `dirs`) | none of their own: rand's API, UUIDs and platform directories over `FillRandom`, `WallClock` and `Var` | whichever handles those |
//!
//! Each handler serves what it is for: Tokio and smol have no environment
//! or randomness, so an async program takes those from `Std` in the same
//! root, `seam::root![Std<fx::Var>, Smol<fx::Sleep>]`; `TzDb` and
//! `Polling` work over whichever filesystem they are given, a `Sim`'s in
//! tests. Each module's docs show its handlers in a program and in a test.

//!
//! # Async
//!
//! Async capabilities are in [`future`]. Every sync capability is also an
//! async one (again, for types that opt in with [`Capability`]), so an
//! async core runs on any handler, sync or async, under any executor:
//! seam's own [`block_on`](future::block_on), Tokio's, smol's,
//! futures-rs', or the simulated runtime's. A sync handler raised to
//! async still blocks while it works; async handlers (`Tokio`, `Smol`,
//! `Reqwest`, ...) wait without blocking. Write `SendPerform`, `SendInspect`,
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
//! The repository's `crates/seam/examples` has a program for each module
//! above, each a core with a `main` on real handlers and tests on
//! simulated ones: `guess`, `backup`, `custom_ops` and `word_count` (the
//! OS, and your own effects), `echo_server` (Tokio), `checksums` (smol),
//! `word_pipe` (futures), `world_clock` (jiff), `dice` (rand), `ids`
//! (uuid), `settings` (dirs), `import_log` (tracing), `changes` (notify),
//! `titles` (reqwest), `todo` (sqlx), `shared_dir` (unix) and
//! `gpu_double` (ash, with a shader written in Rust). Each one's header
//! says the features it needs; `cargo test -p seam-rs --examples
//! --all-features` runs them all.
//!
//! # Features
//!
//! `seam` is `no_std` with no default features: you get the traits, the
//! prelude and [`Push`].
//!
//! Features come in two independent kinds. **Capability groups** choose
//! which of [`ops`]' operations exist. **Crates** bring in a module of the
//! table above: its effects and its handlers. No capability group turns on
//! a crate, so a library crate can depend on `seam` with only the groups
//! its core needs, and only the final binary (or its tests) picks
//! handlers.
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
//! | Crate | Gives you |
//! |---|---|
//! | `std` | `seam::stdlib`: `Std` |
//! | `tokio` | `seam::tokio`: `Tokio` |
//! | `smol` | `seam::smol`: `Smol` |
//! | `futures` | `seam::futures`: `Futures`, `Executor`, and futures-rs' API |
//! | `test` | `seam::test`: `Sim`, `AsyncSim`, `ThreadSim`; with the other crates on, their world-file blocks |
//! | `jiff`, `jiff-bundled` | `seam::jiff`; `jiff-bundled` adds `Bundled`, the compiled-in tz database |
//! | `rand`, `uuid`, `dirs` | `seam::rand`, `seam::uuid`, `seam::dirs` |
//! | `tracing`, `notify`, `reqwest`, `sqlx`, `ash` | that module, with its real handler (`reqwest` with rustls; `sqlx` with SQLite on Tokio, and on smol with `smol`) |
//! | `unix`, `rustix`, `nix` | `seam::unix`; `rustix` and `nix` add `Rustix` and `Nix` |
//! | `rt-multi-thread`, `test-util` | Tokio's runtime options, as in Tokio; only with `tokio` |
//!
//! The OS handlers implement the operations of exactly the capability
//! groups that are enabled.
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

/// The OS's effects, performed with std: `Std`.
#[cfg(feature = "std")]
pub use seam_std as stdlib;

/// The OS's effects, simulated: `Sim`, and its seeded runtime and thread
/// scheduler. Its only real effect is allocation.
#[cfg(feature = "test")]
pub use seam_test as test;

/// The OS's effects, performed with Tokio: `Tokio`.
#[cfg(feature = "tokio")]
pub use seam_tokio as tokio;

/// Channels, cells and tasks on any executor, performed with futures-rs;
/// futures-io and `Stream` adapters, and concurrent async helpers.
#[cfg(feature = "futures")]
pub use seam_futures as futures;

/// Time zones as effects, over jiff: looked up through the root's
/// filesystem, so tests decide what they are.
#[cfg(feature = "jiff")]
pub use seam_jiff as jiff;

/// The OS's effects, performed with smol: `Smol`.
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
