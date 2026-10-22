# seam

Sans-effect programming for Rust.

```toml
[dependencies]
seam = { package = "seam-rs", version = "0.1", features = ["fs", "std"] }
```

The crates are published as `seam-rs`, `seam-rs-core`, `seam-rs-std`,
`seam-rs-tokio` and `seam-rs-test`; in code they are still `seam`,
`seam_core`, and so on.

Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every** effect:
I/O, time, randomness, shared state. The core of your program is written
against capability bounds and can be unit tested, or run, with no effects at
all. Real implementations or test fakes are plugged in at the edge, the seam.

A function's bounds are an audit of what it can do:

```rust
use seam::prelude::*;
use seam::ops::random::RandomU64;
use seam::ops::time::Sleep;
use core::time::Duration;

/// Can sleep and draw random numbers. Nothing else.
fn jitter<C: Perform<RandomU64> + Perform<Sleep>>(c: &mut C) {
    let ms = c.perform(RandomU64) % 100;
    c.perform(Sleep(Duration::from_millis(ms)));
}
```

## Crates

| Crate | What it is |
|---|---|
| [`seam-core`](crates/seam-core) (`seam-rs-core`) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Provide`/`Acquire` and their async forms, `block_on`), the operation and acquisition types, and a `prelude`. `no_std`. |
| [`seam-std`](crates/seam-std) (`seam-rs-std`) | The std backend: a `Std` capability (used alone, or as a part of a `Root`) and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](crates/seam-tokio) (`seam-rs-tokio`) | The Tokio backend: a `Tokio` capability implementing the async and `Send` capability traits, for time, files, the network, stdio, processes, signals, tasks and channels. |
| [`seam-test`](crates/seam-test) (`seam-rs-test`) | The pure test backend: a `Sim` world with a virtual clock, seeded randomness, in-memory filesystem and network, scripted commands and captured stdio. Its only real effect is allocation. |
| [`seam`](crates/seam) (`seam-rs`) | Re-exports everything, behind feature flags. `no_std` by default. |

## Features

`seam` has no default features: you get the traits and `Push`, with no std.

Features come in two independent kinds. Capability groups choose which
operations exist; backends choose where effects really happen. **No
capability group turns on a backend**: that is always the end user's
choice. A library can depend on `seam` with just the groups its core needs,
and only the binary (or its tests) picks a backend.

| Capability group | Gives you |
|---|---|
| `time`, `random` | `Sleep`, `Now`, `RandomU64`, `FillRandom`. No std needed. |
| `rt` | `Spawn`, `SpawnSet`, `SpawnWith`, `Join`, `Detach`, `YieldNow`, `AvailableParallelism`. No std or alloc needed. On `std` the tasks are closures on OS threads; on Tokio and the simulated runtime, futures. |
| `sync` | Channels (`Channel`, `SendMsg`, `RecvMsg`) and shared cells (`Share`, `Load`, `Store`, `Update`). No std or alloc needed. |
| `alloc` | `Spawn::boxed` and `SpawnWith::boxed`, for tasks of unnameable type. |
| `fs`, `net`, `process`, `env`, `io-std`, `signal` | Those operation types. Links std for their types. |
| `full` | Every capability group. Still no backend. |

| Backend | Gives you |
|---|---|
| `std` | The std backend, `seam::stdlib`. |
| `tokio` | The Tokio backend, `seam::tokio`. |
| `test` | The pure test backend, `seam::test`. No std on its own. |
| `rt-multi-thread`, `test-util` | Tokio runtime options, named as in Tokio. Only with `tokio`. |

A backend implements exactly the capability groups that are enabled, so
`features = ["tokio", "rt", "net", "time"]` gives you Tokio-backed
networking and clocks and nothing else.

## Roots

A program's edge can hand its core one capability built from parts: a
`Root`. Each part serves one or more kinds of operation (`kinds::Fs`,
`kinds::Time`, ...), so one root can mix backends, sync and async parts,
simulated parts, and the program's own effects:

```rust
let mut root = seam::root! {
    std: Std::new() => [kinds::Env, kinds::Random],
    tokio: Tokio::new() => [kinds::Time, kinds::Fs, kinds::Net],
    store: redis => [Store],                // the program's own kind
};
serve(&mut root).await;                     // bounded as usual: SendPerform<Sleep>, ...
```

Each backend covers only what it is for: Tokio has no environment or
randomness, so a Tokio program takes those from `Std`. `Std` and `Tokio`
are zero-sized, and so is a root of them.

The root forwards each operation to the part serving its kind, in whichever
form that part implements: sync parts through the sync traits, async parts
through the async ones. Each kind is served once; listing two parts for one
kind fails to compile. A program's own kind is a marker type, and each of
its operations names it:

```rust
struct Store;
impl InKind for Get { type Kind = Store; }
```

`root!` evaluates to the root. It writes a struct of the parts and one
`Pick` impl per kind; a named form (`root! { struct App { ... } }`) gives
the struct a name, for when the root's type must be written out. Capability types opt in to seam's lifts (an `Inspect` is
also a `Perform`, a sync operation also works from async code) with
`impl Capability for MyType {}`; every type in seam's backends does.

## Writing your own effects

A program's own effects (a key-value store, a payment API, a queue) are
operations like seam's. Four steps:

```rust
// 1. The operations: a type each, with its outcome and its kind. A kind is
//    a marker type, and groups operations one part serves.
pub struct Store;
pub struct Get(pub String);
impl Operation for Get { type Outcome = io::Result<Option<String>>; }
impl Query for Get {}                       // read-only: `Inspect`, `&self`
impl InKind for Get { type Kind = Store; }

pub struct Put(pub String, pub String);
impl Operation for Put { type Outcome = io::Result<()>; }
impl InKind for Put { type Kind = Store; }

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
let mut root = seam::root! {
    std: Std::new() => [kinds::Env, kinds::Stdio],
    store: Redis::connect()? => [Store],
};
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
  [`custom_ops`](crates/seam/examples/custom_ops.rs).
- **Effects of a kind seam has** (another filesystem, say) use seam's
  operations and kind (`kinds::Fs`), so they can stand in for a backend's
  part in a root.

## Testing a core

`seam-test`'s `Sim` stands in for a real backend. Set up the world, run the
core, assert on what it left behind:

```rust
use seam::Perform;
use seam::ops::fs::WriteFile;
use seam::test::Sim;

let mut sim = Sim::with_seed(7);
sim.add_dir("/data");
sim.perform(WriteFile::new("/data/out", "ok")).unwrap(); // your core here
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
    file.perform(WriteAll(b"..."))?; // an error, a panic, or (with
    Ok(())                           // acquire_async) cancellation here...
})?;                                 // ...leaves the old file untouched
```

When the body returns `Ok` the resource is committed: a file is synced, an
atomic file renamed into place, a stream shut down cleanly, a task waited
for. Otherwise it is rolled back, and a task is aborted. Providers make
dropping a resource equal rolling it back, so cancelling at any `.await`,
even while opening or closing, never commits half a result. Atomic files on
Tokio are tested by cancelling after every possible number of polls.

A resource reaches a task only through the provider: `SpawnWith` has a
listener accept a connection into a new task that owns it, running a named
`TaskBody`. `SpawnSet` opens a set of tasks to spawn into and collect from,
joined or aborted together. None of the task API allocates in seam itself;
`alloc` only adds boxed conveniences (`Spawn::boxed`, `SpawnWith::boxed`).
Temporary files left by a process that was killed outright are cleaned up
with `SweepAtomic`.

## World files

A test can describe the world a core starts in, and what it should look
like afterwards, in [KDL](https://kdl.dev): one block per kind of
operation, the same kinds a root is built from.

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
    fs { file "/var/log/app.log" "started on 8080\n"; absent "/tmp/app.lock" }
    stdio { stdout "ok\n" }
    process { ran git status }
"#);
```

Directives apply in order, and something set twice keeps its last value;
`load_world` layers a test's changes over a shared base. Directories nest.
A failing expectation is reported with its line, all of them at once, and
a mistake in the file is an error with its line and column ("unknown `fil`
in `fs` (did you mean `file`?)"). The parser is seam-test's own, for the
part of KDL world files use, so seam-test still has no dependencies.

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
with the task still running. An expired operation had no effect, except
`ReadToEnd`, which keeps what arrived, and `WriteAll`, which may have written
part of its buffer. `Timeout` covers channel sends and receives, socket
reads and writes, accept (also with `SpawnWith`), connect, `RecvFrom`, joins
and `CtrlC`, on `seam-std`, `seam-tokio` and the simulation. On the
simulated runtime and threads, timeouts run on the virtual clock, so a test
can check exactly when one fired.

## Keeping the core honest

The newtype handles stop effects leaking out of the backends, but nothing in
the type system stops a core crate from calling `std::fs::read` directly.
[`templates/clippy.toml`](templates/clippy.toml) bans the std and Tokio
effect APIs (and shared-state types); copy it into each crate that should stay
sans-effect.

## Examples

Real programs in [`crates/seam/examples`](crates/seam/examples), each with
tests that run its core in the simulation:

| Example | Shows |
|---|---|
| [`guess`](crates/seam/examples/guess.rs) | randomness and stdio; seeded, scripted tests, and a test of a playing strategy |
| [`backup`](crates/seam/examples/backup.rs) | filesystem, clock and arguments; bundling many capabilities into one trait; an atomic manifest marks complete snapshots, and crashed runs are cleaned up |
| [`echo_server`](crates/seam/examples/echo_server.rs) | an async `Send` core on a root of Tokio and std that spawns a task per connection; its handler is tested on the in-memory network with no runtime |
| [`custom_ops`](crates/seam/examples/custom_ops.rs) | defining your own operations and their kind; building a capability out of other capabilities; putting it in a root next to `Std`; atomic writes in a scope |
| [`word_count`](crates/seam/examples/word_count.rs) | a worker pool on OS threads, tested on simulated threads under many schedules; a world file for its setup and expectations |

```text
cargo run  -p seam-rs --example guess --features full,std,tokio,rt,test
cargo test -p seam-rs --examples --features full,std,tokio,rt,test
```

## Design

See [DESIGN.md](DESIGN.md). API documentation: `cargo doc --open --all-features`.

## License

MIT. See [LICENSE](LICENSE).
