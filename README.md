# seam

Sans-effect programming for Rust.

```toml
[dependencies]
seam = { package = "seam-rs", version = "0.6", features = ["fs", "std"] }
```

The crates are published as `seam-rs`, `seam-rs-core`, `seam-rs-std`,
`seam-rs-tokio`, `seam-rs-smol`, `seam-rs-test`, and one `seam-rs-*` crate
per wrapped library (below); in code they are still `seam`, `seam_core`,
and so on.

Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every** effect:
I/O, time, randomness, shared state. The core of your program is written
against capability bounds and can be unit tested, or run, with no effects at
all. Real implementations or test fakes are plugged in at the edge, the seam.

A function's bounds are an audit of what it can do:

```rust
use seam::prelude::*;
use seam::ops::random::{FillRandom, random_u64};
use seam::ops::time::Sleep;
use core::time::Duration;

/// Can sleep and draw random numbers. Nothing else.
fn jitter<C: for<'a> Perform<FillRandom<'a>> + Perform<Sleep>>(c: &mut C) {
    let ms = random_u64(c) % 100;
    c.perform(Sleep(Duration::from_millis(ms)));
}
```

## Primitives and helpers

The operations are **primitives**: the smallest effects a backend must
provide, such as `Read` and `Write` on a handle, `Open` a file, `FillRandom`.
Everything that can be built from them is, in seam-core, as plain functions
next to the operations: `read_exact`, `write_all`, `read_file`,
`write_file`, `append_file`, `create_dir_all`, `copy_dir_all`, `exists`,
`resolve`, `connect_any`, `random_u64`, `random_range`, `shuffle`,
`sleep_until`, `tick`, `var_or`, `print`, `println`, `read_line`,
`run_checked`, `try_send`, and `BufReader` for lines (or any delimited
record) from any handle. A backend implements only the primitives, and every helper then
works on it, so lines come from stdin, a socket, a child or a file alike.

Rust cannot be generic over sync and async, or over whether a future is
`Send`, so each helper comes in three forms, generated from one body:
`write_all` for sync capabilities, `write_all_async`, and `write_all_send`
for multithreaded runtimes.

A few effects that could be built from primitives stay primitives, each for
a reason: `CreateAtomic` (cleanup on a panic or a cancelled future must run
in `Drop`, where a core cannot perform effects), recursive `RemoveDir`
(a walk that follows a symlink deletes outside the tree), `CopyFile`
(platforms copy faster, and copy permissions) and `RunCommand` (collecting
both outputs means reading two pipes at once).

A stream or child can be split for the length of a scope (`Split`,
`SplitChild`) into pieces that each keep exclusive access to their own
direction, so a core reads and writes at once without sharing a handle.

Files open in a mode named by a type, `Open<'a, ReadOnly, F>`,
`Open<'a, WriteOnly, F>`, ..., and a backend's handle implements `Read` or
`Write` only for modes that allow it. A core bounded on `ReadOnly` cannot
write, and that is checked at compile time.

Reading a whole file changes nothing outside the read, so it is a query: a
`View` opens a regular file to read at offsets (`ReadAt`, an `Inspect`),
and is lent from shared access:

```rust
fn word_count<C, F>(c: &C, path: &str) -> io::Result<usize>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    Ok(read_to_string(c, path)?.split_whitespace().count())
}
```

## Crates

| Crate | What it is |
|---|---|
| [`seam-core`](https://github.com/parzivale/seam/tree/main/crates/seam-core) (`seam-rs-core`) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Provide`/`Lend` and their async forms, `block_on`), the operation and acquisition types, and a `prelude`. `no_std`. |
| [`seam-std`](https://github.com/parzivale/seam/tree/main/crates/seam-std) (`seam-rs-std`) | The std backend: a `Std` capability (used alone, or as a part of a `Root`) and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](https://github.com/parzivale/seam/tree/main/crates/seam-tokio) (`seam-rs-tokio`) | The Tokio backend: a `Tokio` capability implementing the async and `Send` capability traits, for time, files, the network, stdio, processes, signals, tasks and channels. |
| [`seam-smol`](https://github.com/parzivale/seam/tree/main/crates/seam-smol) (`seam-rs-smol`) | The smol backend: a `Smol` capability implementing the async and `Send` traits, for time, files, the network, stdio, processes, Ctrl-C (Unix), tasks and channels. |
| [`seam-futures`](https://github.com/parzivale/seam/tree/main/crates/seam-futures) (`seam-rs-futures`) | The futures-rs backend: `Futures` (channels and shared cells that need no runtime) and `Executor` (tasks on any futures-rs executor), futures-io adapters both ways, `Stream`s of messages, lines and directory entries, and helpers that need concurrency within a task (`within`, `copy_bidirectional`, `pipe_through`). |
| [`seam-jiff`](https://github.com/parzivale/seam/tree/main/crates/seam-jiff) (`seam-rs-jiff`) | Time zones as effects, over jiff: `TimeZoneFor` and `SystemTimeZone` queries, answered by `TzDb` (the tz database, read through the filesystem of any capability, so a `Sim` decides what the zones are) or `Bundled` (compiled in), and `now_in`, `now_local` and friends over `WallClock`. |
| [`seam-rand`](https://github.com/parzivale/seam/tree/main/crates/seam-rand) (`seam-rs-rand`) | rand on seam's randomness: any `FillRandom` capability as a rand RNG, or a rand generator seeded from it. `no_std`. |
| [`seam-uuid`](https://github.com/parzivale/seam/tree/main/crates/seam-uuid) (`seam-rs-uuid`) | v4 UUIDs from `FillRandom`, time-ordered v7 UUIDs from `WallClock` and `FillRandom`. |
| [`seam-dirs`](https://github.com/parzivale/seam/tree/main/crates/seam-dirs) (`seam-rs-dirs`) | Home, config, cache and data directories by the rules of `dirs`/`directories`, from the environment seam reads. |
| [`seam-tracing`](https://github.com/parzivale/seam/tree/main/crates/seam-tracing) (`seam-rs-tracing`) | Logging as an effect (`Log`, `info!`, ...), sent to tracing, or captured and checked in world files. |
| [`seam-notify`](https://github.com/parzivale/seam/tree/main/crates/seam-notify) (`seam-rs-notify`) | File watching as a scope: the OS's events through notify, or rescanning through any capability's filesystem (so a `Sim` drives it). |
| [`seam-reqwest`](https://github.com/parzivale/seam/tree/main/crates/seam-reqwest) (`seam-rs-reqwest`) | HTTP requests as an effect (`Fetch`, over `http`'s types), sent with reqwest, or scripted per URL in tests. |
| [`seam-sqlx`](https://github.com/parzivale/seam/tree/main/crates/seam-sqlx) (`seam-rs-sqlx`) | SQL as effects: `Execute`, `Fetch`, transactions as scopes, portable values; sqlx's `Any` pool, or in-memory SQLite in tests. |
| [`seam-ash`](https://github.com/parzivale/seam/tree/main/crates/seam-ash) (`seam-rs-ash`) | The GPU as effects, over Vulkan through ash: buffers, images and programs as scopes; uploads, copies, dispatches, draws and downloads as operations; every barrier and layout transition worked out by the handler. `Import` brings in a Wayland client's dmabuf, a shared Direct3D texture or an IOSurface without a CPU copy. `Ash` is real Vulkan 1.3, blocking or async from one implementation; `SimGpu` moves real bytes and runs programs by CPU functions in tests. |
| [`seam-unix`](https://github.com/parzivale/seam/tree/main/crates/seam-unix) (`seam-rs-unix`) | Unix effects: mounts, `chmod`/`chown`, symlinks, IDs, signals, the host name, the umask; and `SimUnix`, which simulates them. |
| [`seam-rustix`](https://github.com/parzivale/seam/tree/main/crates/seam-rustix), [`seam-nix`](https://github.com/parzivale/seam/tree/main/crates/seam-nix) | `seam-unix`'s effects performed with rustix, or with nix. |
| [`seam-test`](https://github.com/parzivale/seam/tree/main/crates/seam-test) (`seam-rs-test`) | The pure test backend: a `Sim` world with a virtual clock, seeded randomness, in-memory filesystem and network, scripted commands and captured stdio. Its only real effect is allocation. |
| [`seam`](https://github.com/parzivale/seam/tree/main/crates/seam) (`seam-rs`) | Re-exports everything, behind feature flags. `no_std` by default. |

## Features

`seam` has no default features: you get the traits and `Push`, with no std.

Features come in two independent kinds. Capability groups choose which
operations exist; backends choose where effects really happen. **No
capability group turns on a backend**: that is always the end user's
choice. A library can depend on `seam` with just the groups its core needs,
and only the binary (or its tests) picks a backend.

| Capability group | Gives you |
|---|---|
| `time`, `random` | `Sleep`, `Now`, `FillRandom`, `random_u64`. No std needed. |
| `rt` | `Spawn`, `SpawnSet`, `SpawnWith`, `Join`, `Detach`, `YieldNow`, `AvailableParallelism`. No std or alloc needed. On `std` the tasks are closures on OS threads; on Tokio, smol, futures-rs' executors and the simulated runtime, futures. |
| `sync` | Channels (`Channel`, `SendMsg`, `RecvMsg`, `try_send`, `try_recv`) and shared cells (`Share`, `Load`, `Update`, `store`). No std or alloc needed. Turns on `time`: the `try_` helpers are a timeout of zero. |
| `alloc` | `Spawn::boxed` and `SpawnWith::boxed`, for tasks of unnameable type; the helpers that return owned data (`read_to_end`, `read_file`, `resolve`, ...). Each has a primitive beneath it that fills a buffer you pass. |
| `io`, `net`, `io-std`, `signal` | Those operation types. No std needed. |
| `fs`, `process`, `env` | Those operation types. Links std for their types (`Path`, `Command`, `OsString`). `env` includes `SetVar` and `RemoveVar`, which std serves only through `WritableEnv` (see Roots). |
| `full` | Every capability group. Still no backend. |

| Backend | Gives you |
|---|---|
| `std` | The std backend, `seam::stdlib`. |
| `tokio` | The Tokio backend, `seam::tokio`. |
| `futures` | The futures-rs backend, `seam::futures`. |
| `jiff`, `jiff-bundled` | Time zones over jiff, `seam::jiff`; `jiff-bundled` adds the compiled-in tz database. |
| `smol` | The smol backend, `seam::smol`. |
| `rand`, `uuid`, `dirs`, `tracing`, `notify`, `reqwest`, `sqlx` | The wrapper of that crate, as `seam::rand`, `seam::uuid`, and so on. |
| `ash` | The GPU over Vulkan, `seam::ash`. |
| `unix`, `rustix`, `nix` | Unix effects, `seam::unix`, and their handlers over rustix (`seam::rustix`) and nix (`seam::nix`). |
| `test` | The pure test backend, `seam::test`. No std on its own. |
| `rt-multi-thread`, `test-util` | Tokio runtime options, named as in Tokio. Only with `tokio`. |

A backend implements exactly the capability groups that are enabled, so
`features = ["tokio", "rt", "net", "time"]` gives you Tokio-backed
networking and clocks and nothing else.

## Backends

A program picks one backend for what waits (or none, if it is sync) and
puts `Std` beside it for the environment and randomness; the wrapper
crates' handlers sit next to them in the same root.

| Backend | Forms | Covers |
|---|---|---|
| `Std` (`seam::stdlib`) | sync, and async through the lifts (blocking) | the environment, arguments, randomness, clocks, files, the network, stdio, processes, threads, channels |
| `Tokio` (`seam::tokio`) | async, `Send` | time, files, the network, stdio, processes, signals, tasks, channels |
| `Smol` (`seam::smol`) | async, `Send` | time, files, the network, stdio, processes, Ctrl-C (Unix), tasks, channels |
| `Futures`, `Executor` (`seam::futures`) | async, any executor | channels and locks that need no runtime, tasks on any futures-rs executor, futures-io adapters, `within` |
| `Sim` (`seam::test`) | sync and async | the whole world, simulated: for tests |

Every wrapper crate works with every backend:

| Crate | Real handler | Forms | Needs in the root |
|---|---|---|---|
| `seam::jiff` | `TzDb` (`Bundled`) | sync; async through the lifts | `WallClock` (any backend); `TzDb` reads through `Std`'s filesystem |
| `seam::rand` | none: an RNG over the capability | sync (`FillRandom` never waits, so async cores use it too) | `FillRandom` from `Std` |
| `seam::uuid` | none: helpers | sync, async, `Send` | `FillRandom` from `Std`, and `WallClock` for v7 |
| `seam::dirs` | none: helpers | all three | `Var` from `Std` |
| `seam::tracing` | `Tracing` | sync; async through the lifts | nothing else |
| `seam::notify` | `Notify` | sync; async without blocking, any executor | nothing else |
| `seam::reqwest` | `Reqwest` | async, any executor (it brings its own Tokio runtime where there is none) | nothing else |
| `seam::sqlx` | `Sqlx` | async: on Tokio, or on smol with `smol` on | nothing else |
| `seam::rustix`, `seam::nix` | `Rustix`, `Nix` | sync; async through the lifts | nothing else |
| `seam::ash` | `Ash` | sync and async from one implementation; any executor | nothing else |

Each has its handler for tests too (`Scripted`, `Captured`, `Polling`
over a `Sim`, `Sqlx::memory`, `SimUnix`, `SimGpu`), and each crate's docs
show it in a program and in a test.

## Roots

A program's edge can hand its core one capability built from parts: a
`Root`. Each part lists the effects it serves, by their keys
(`effects::Now`, `effects::View`, `effects::Open<WriteOnly>`, ...), so one
root can mix backends, sync and async parts, simulated parts, and the
program's own effects, and grants the core exactly what it lists:

```rust
use seam::effects as fx;

let mut root = seam::root![
    Std<fx::Var, fx::FillRandom>,           // a backend, and the effects it serves
    Tokio<fx::Now, fx::Sleep, fx::View, fx::TcpBind>,
    Redis<Get, Put>::connect(url)?,         // built by one of its constructors
];
serve(&mut root).await;                     // bounded as usual: SendPerform<Sleep>, ...
```

A part is written the way Rust builds a value of its type, with the effects
it serves in brackets after the type's name: a unit struct (`Std<..>`), a
tuple struct (`Pool<fx::Query>(4)`), a struct literal
(`Pool<fx::Query> { size: 4 }`), or one of the type's constructors
(`Sim<fx::Args, fx::View>::from_world(WORLD)?`). Those four are all there
is.

A core asking for an effect the root does not list fails to compile: "the
root `Parts<..>` has no part for `fx::Open<WriteOnly>`". `Open` is keyed by
its mode, so a part can be granted reading without writing.

Each backend covers only what it is for: Tokio and smol have no
environment or randomness, so a Tokio or smol program takes those from
`Std` (see [Backends](#backends)). `Std` and `Tokio`
are zero-sized, and so is a root of them. `Std` only reads the
environment; to change it (`SetVar`, `RemoveVar`), serve `fx::SetVar` and
`fx::RemoveVar` with `seam_std::env::WritableEnv`, which is `unsafe` to
create, as `std::env::set_var` is to call.

The root forwards each operation to the part listed for its effect, in
whichever form that part implements: sync parts through the sync traits,
async parts through the async ones. Each effect is served once; listing two
parts for one effect fails to compile. A scope routes by its acquisition
(`fx::View` serves opening and closing a `View`). A program's own effect
names its key, usually itself:

```rust
impl Effect for Get { type Key = Get; }
```

`root!` evaluates to the root. It writes a struct of the parts and one
`Pick` impl per effect, which is why every part names its effects in
brackets: Rust cannot tell apart keys it would have to look up in another
crate. `root.part::<fx::Var>()` gives back a part; a named form
(`root! { struct App { clock: Clock<fx::Now> } }`) gives
the struct a name, for when the root's type must be written out.
Capability types opt in to seam's lifts (an `Inspect` is also a `Perform`,
a sync operation also works from async code) with
`impl Capability for MyType {}`; every type in seam's backends does.

## I/O errors

I/O operations fail with `seam::io::Error`, not `std::io::Error`, so the
same operation types work without std. It is an `ErrorKind` (the same
kinds as std's, `non_exhaustive`) and a `&'static str` message, and
allocates nothing. With std, it also keeps the `std::io::Error` it came
from: it converts both ways with `?` or `.into()`, `as_std()` borrows the
original, and its `Display` shows the original's message. Features only
add to it, so crates built with and without std agree on the type.

```rust
use seam::io;

match read_to_string(c, "config.toml") {
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
    other => parse(other?),
}
```

A core that has its own error to report builds one from a kind and a
static message: `io::Error::new(io::ErrorKind::InvalidData, "not a number")`.
For a message built at run time, keep it in your own error type, or on std
make a `std::io::Error` and convert it.

## Writing your own effects

A program's own effects (a key-value store, a payment API, a queue) are
operations like seam's. Four steps:

```rust
// 1. The operations: a type each, with its outcome and its key, the marker
//    a root routes it by. An effect of your own is usually its own key.
pub struct Get(pub String);
impl Operation for Get { type Outcome = io::Result<Option<String>>; }
impl Query for Get {}                       // read-only: `Inspect`, `&self`
impl Effect for Get { type Key = Get; }

pub struct Put(pub String, pub String);
impl Operation for Put { type Outcome = io::Result<()>; }
impl Effect for Put { type Key = Put; }

// 2. The core asks for them like any capability.
fn visit<C: Inspect<Get> + Perform<Put>>(c: &mut C, page: &str) -> io::Result<u64> { ... }

// 3. Implementations: a real one and a fake. `Capability` opts each in to
//    seam's lifts (an `Inspect` is also a `Perform`; sync also works async).
struct Redis { /* a connection */ }
impl Capability for Redis {}
impl Inspect<Get> for Redis { fn inspect(&self, op: Get) -> io::Result<Option<String>> { ... } }
impl Perform<Put> for Redis { fn perform(&mut self, op: Put) -> io::Result<()> { ... } }

#[derive(Default)]
struct Memory(BTreeMap<String, String>);
impl Capability for Memory {}
// ... the same two impls, on a map

// 4. At the edge, put the real one in a root next to a backend.
let mut root = seam::root![
    Std<fx::Var, fx::OpenStdout>,
    Redis<Get, Put>::connect()?,
];
visit(&mut root, "home")?;
```

- **Async implementations** implement `AsyncInspect`/`AsyncPerform`, and
  the `Send` forms (`SendInspect`, `SendPerform`) for multithreaded
  runtimes, returning `impl Future<Output = ...> + Send`.
- **Effects built on other effects** wrap a capability and use its
  operations: a store kept in a file is a `FileStore<C>` whose `Put` writes
  through `C`'s `CreateAtomic`. It then runs on every backend, the
  simulation included, and on the simulated runtime and threads its
  interleavings are explored like any other effects. See
  [`custom_ops`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/custom_ops.rs).
- **Another implementation of seam's effects** (another filesystem, say)
  implements seam's operations, so it is listed in a root under seam's keys
  (`fx::View`, `fx::Open<ReadOnly>`, ...) in place of a backend.

## Testing a core

`seam-test`'s `Sim` stands in for a real backend. Set up the world, run the
core, assert on what it left behind:

```rust
use seam::ops::fs::write_file;
use seam::test::Sim;

let mut sim = Sim::with_seed(7);
sim.add_dir("/data");
write_file(&mut sim, "/data/out", "ok").unwrap(); // your core here
assert_eq!(sim.file("/data/out").unwrap(), b"ok");
```

Used directly, nothing in the simulation waits: `Sleep` advances the
virtual clock, and an operation that would block fails with `WouldBlock`.
The same seed and calls always give the same results.

For concurrent cores, `seam::test::rt::Runtime` runs async code over a `Sim`
on a deterministic executor:

```rust
let rt = Runtime::with_seed(sim, seed);
rt.block_on(|sim| async move { serve(&mut sim.clone(), addr).await /* ... */ });
```

- Blocking operations (reads, accepts, receives, `CtrlC`) suspend until
  another task makes them possible.
- `Sleep` waits on a virtual clock that jumps to the next timer when every
  task is waiting, so hours of timeouts take no time.
- The seed picks which ready task runs next. Sweep seeds to explore
  interleavings and replay a failure from its seed. The tests include a
  lost-update race that only some seeds expose.
- When nothing can ever make progress, `block_on` reports a deadlock
  instead of hanging.
- Tasks (`Spawn`, `SpawnSet`, `SpawnWith`) work as on Tokio, so the
  `echo_server` example's whole accept loop is tested with concurrent
  clients under 16 schedules.

For sync cores that spawn threads, `seam::test::threads::Threads` does the
same with real threads run one at a time:

```rust
let threads = Threads::with_seed(sim, seed);
threads.run(|mut sim| run(&mut sim)); // `sim` is a cloneable `ThreadSim`
```

- Every effect is a scheduling point, where the seed picks which runnable
  thread continues. Threads can only share state through seam's cells and
  channels, so this reaches every interleaving a core can observe.
- Receives, sends to a full channel, joins, reads, accepts and `CtrlC`
  block the simulated thread until another makes them possible; `Sleep`
  uses the same jumping virtual clock; deadlocks are reported.
- Thread handles follow `seam-std`'s rules: no abort, and a failed scope
  joins its threads unless they were detached.
- The `word_count` example runs one worker-pool core on `seam-std`'s OS
  threads in `main` and on `Threads` in its tests, under 48 schedules. Its
  queue pop is one `Update`; made a separate load and store, seed 0 counts
  files twice.

## Resources and cancellation

A core never owns a file, socket or task. It acquires one for the length of
a closure, which borrows it and gets the capability back:

```rust
c.acquire(CreateAtomic::new("settings.conf"), |c, file| {
    write_all(file, b"...")?;        // an error, a panic, or (with
    Ok(())                           // acquire_async) cancellation here...
})?;                                 // ...leaves the old file untouched
```

Opening and closing are operations themselves (`Opening`, `Closing`), so
scopes work everywhere operations do: async, in a `Root`, through a
wrapper. A scope that changes nothing outside itself is lent (`Lend`) from
`&C`; one that does is provided (`Provide`) from `&mut C`.

When the body returns `Ok` the resource is committed: a file is synced, an
atomic file renamed into place, a stream shut down cleanly, a task or child
process waited for. Otherwise it is rolled back: a task is aborted, a child
process killed. Providers make dropping a resource equal rolling it back,
so cancelling at any `.await`, even while opening or closing, never
commits half a result. Atomic files on Tokio are tested by cancelling
after every possible number of polls.

Inside the scope, a file is read and written with the `io` operations, and
also moved (`Seek`), truncated or extended (`SetLen`), synced (`SyncAll`,
`SyncData`) and asked for its metadata (`Stat`). Standard input, output and
error are handles too (`OpenStdin`, `OpenStdout`, `OpenStderr`), so the
same helpers read and write them.

A resource reaches a task only through the provider: `SpawnWith` has a
listener accept a connection into a new task that owns it, running a named
`TaskBody`. `SpawnSet` opens a set of tasks to spawn into and collect from,
joined or aborted together. None of the task API allocates in seam itself;
`alloc` only adds boxed conveniences (`Spawn::boxed`, `SpawnWith::boxed`).
Temporary files left by a process that was killed outright are cleaned up
with `sweep_atomic`. Listing a directory works the same way: `ReadDir` lends
a listing that `NextEntry` steps through one path at a time.

Operations borrow their inputs (paths, buffers, names), so describing one
allocates nothing. Bounds name them for every lifetime:
`for<'a> Perform<Read<'a>>`.

## World files

A test can describe the world a core starts in, and what it should look
like afterwards, in [KDL](https://kdl.dev): one block per area of the
world (`env`, `fs`, `net`, ...).

```kdl
// app.world
seed 7
env {
    var USER ada
    args app "--config" "/etc/app.conf"
}
fs {
    dir "/etc" {
        file app.conf """
            port = 8080

            """
    }
}
net { host example.com "93.184.216.34" }
process { command git exit=0 stdout="clean\n" }
```

```rust
let mut sim = Sim::from_world(include_str!("app.world"))?;
run(&mut sim)?;
sim.assert_world(r#"
    env { var APP_STATE ready }
    fs { file "/var/log/app.log" "started on 8080\n"; absent "/tmp/app.lock" }
    stdio { stdout "ok\n" }
    process { ran git status }
"#);
```

Directives apply in order, and something set twice keeps its last value;
`load_world` layers a test's changes over a shared base. Directories nest.
Expectations check only what they list: the clock, variables, files,
output and the commands that ran. `sim.to_world()` writes the whole world
out in the same format, for a snapshot or a diff, and loading it back
gives the same world.
A failing expectation is reported with its line, all of them at once, and
a mistake in the file is an error with its line and column ("unknown `fil`
in `fs` (did you mean `file`?)"). The parser is seam-test's own, for the
part of KDL world files use, so seam-test still has no dependencies.

A root's other simulated parts read blocks of their own: a part
is an ordinary handler of its effect that also implements `World` (the
block names it reads, `load`, and `to_world`; its expectations are then a
subset of `to_world`), and one file sets up and checks the whole root
through the effects it lists:

```rust
let mut root = seam::root![
    Sim<fx::Args, fx::View, fx::OpenStdout>::new(),
    FakeMounts<BindMount, Unmount>::default(),   // reads `mounts { .. }`
];
seam::test::load_world::<(fx::Args, fx::View, BindMount), _>(&mut root, WORLD)?;
run(&mut root)?;
seam::test::check_world::<(fx::OpenStdout, BindMount), _>(&root, EXPECTED)?;
let snapshot = seam::test::to_world::<(fx::Args, BindMount), _>(&root); // the whole world, written out
```

Up to 32 effects are listed. Each block goes to the part that reads it, a
part serving several of them is used once, and a block no listed part reads
is an error, with a suggestion.

## Timeouts

Any operation that waits can be given a deadline by wrapping it in
`Timeout`. Its outcome stays the operation's own, so existing error handling
covers expiry:

```rust
match rx.perform(Timeout::new(Duration::from_secs(1), RecvMsg::new())) {
    Ok(job) => run(job),
    Err(RecvError::Empty) => check_for_shutdown(), // nothing within a second
    Err(RecvError::Closed) => return,
}
```

A receive that times out reports `Empty`, a send `Full` with the value
handed back, I/O `io::ErrorKind::TimedOut`, and a join `JoinError::TimedOut`
with the task still running. An expired operation had no effect.
`Timeout` wraps primitives: channel sends and receives, reads and writes
on sockets and children, accept (also with `SpawnWith`), connect,
`RecvFrom`, waits, joins and `CtrlC`, on `seam-std`, `seam-tokio` and the
simulation. A helper built from several primitives has no timed form; a
core that needs one times each primitive against a deadline it keeps. On the
simulated runtime and threads, timeouts run on the virtual clock, so a test
can check exactly when one fired.

## Keeping the core honest

The newtype handles stop effects leaking out of the backends, but nothing in
the type system stops a core crate from calling `std::fs::read` directly.
[`templates/clippy.toml`](https://github.com/parzivale/seam/blob/main/templates/clippy.toml) bans the std and Tokio
effect APIs (and shared-state types); copy it into each crate that should stay
sans-effect.

## Examples

Real programs in [`crates/seam/examples`](https://github.com/parzivale/seam/tree/main/crates/seam/examples), each with
tests that run its core in the simulation:

| Example | Shows |
|---|---|
| [`guess`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/guess.rs) | randomness and stdio; seeded, scripted tests, and a test of a playing strategy |
| [`backup`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/backup.rs) | filesystem, clock and arguments; bundling many capabilities into one trait; an atomic manifest marks complete snapshots, and crashed runs are cleaned up |
| [`echo_server`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/echo_server.rs) | an async `Send` core on a root of Tokio and std that spawns a task per connection; its handler is tested on the in-memory network with no runtime |
| [`custom_ops`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/custom_ops.rs) | defining your own operations and their kind; building a capability out of other capabilities; putting it in a root next to `Std`; atomic writes in a scope |
| [`word_count`](https://github.com/parzivale/seam/blob/main/crates/seam/examples/word_count.rs) | a worker pool on OS threads, tested on simulated threads under many schedules; a world file for its setup and expectations |

```text
cargo run  -p seam-rs --example guess --features full,std,tokio,rt,test
cargo test -p seam-rs --examples --features full,std,tokio,rt,test
```

## Guides

The `seam` crate's `docs` feature adds a `_docs` module of long-form
documentation, on [docs.rs](https://docs.rs/seam-rs/latest/seam/_docs/): what
seam is in effect-system terms and how it compares to other approaches,
usage guides (a first core, roots, resources, async and concurrency,
effects of your own, testing), the full world-file format, and how seam
works inside. Every example in it is a doctest.

## Design

See [DESIGN.md](https://github.com/parzivale/seam/blob/main/DESIGN.md). API documentation: `cargo doc --open --all-features`.

## License

MIT. See [LICENSE](https://github.com/parzivale/seam/blob/main/LICENSE).
