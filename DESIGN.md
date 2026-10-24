# seam: design notes

`seam` is a Rust workspace for an effect/capability trait system. The core
(`seam-core`) depends on nothing and targets `core`; std comes in only with
the capability groups that need it.

## Goal: sans-effect, not just sans-IO

Sans-IO pulls I/O out of protocol logic. `seam` takes the same idea further:
**every** effect is hoisted out of the core, including time, randomness and
shared state. The core can then be unit tested, and run, with no effects at
all. Effects live at the edge of the program (the "seam"), where real
implementations or test fakes are plugged in.

Secondary goal: a program's required capabilities show up in its signatures,
so you can audit what a piece of code is able to do by reading its bounds.

## What counts as an effect

An effect is anything that **escapes unique ownership**: I/O, time and
clocks, randomness, shared or interior-mutable state.

Mutation through `&mut` is **not** an effect; the borrow checker already
tracks it. Control effects (errors, early return, async suspension) use
`Result` and futures, not `Perform`.

## Core traits

- `Operation` (with `type Outcome`) and the `Query` marker: each operation is
  its own type and names its own return type.
- `Inspect<Q: Query>` (`&self`) and `Perform<O>` (`&mut self`), with a blanket
  `impl<T: Inspect<Q>> Perform<Q> for T`, the way `&mut T` reborrows as `&T`.
- Both carry `#[diagnostic::on_unimplemented]`, so a missing capability reads
  as "`X` does not have the capability to perform `Y`".
- Inspects of the same resource commute with each other, not with performs.

## Operation types

`seam-core::ops` defines the operation types, one module per capability
feature, so a core written against them runs on any backend. They are
**primitives**: the smallest set of effects every backend provides (see
**Primitives and helpers** below for what is built from them).

- `io` (on handles): `Read`, `Write`, `Flush`, `Seek(SeekFrom)` with seam's
  own `SeekFrom`
- `fs`: `Metadata` (a query); `CreateDir`, `RemoveFile`, `RemoveDir`
  (recursive or not), `Rename`, `CopyFile`; acquisitions `Open<'a, M, F>`,
  `CreateAtomic<'a, F>` and `ReadDir<'a, D>`, a directory listing that
  `NextEntry` steps through one path at a time; on an open file, `Stat` (a
  query), `SetLen`, `SyncAll` and `SyncData`
- `net`: `ResolveInto` (DNS is an effect), `SendTo`, `RecvFrom`,
  `Shutdown(How)`; `LocalAddr` and `PeerAddr` queries; acquisitions
  `TcpConnect<S>`, `TcpBind<L>`, `Accept<S>` (from a listener) and
  `UdpBind<U>`
- `time`: `Sleep`, `Now` (no_std); `WallClock` (std); `Timeout<O>`
- `env`: `Var`, `Args`, `CurrentDir`, `TempDir` (queries); `SetVar` and
  `RemoveVar`, which check names and values instead of panicking as std
  does. Changing the environment while another thread reads it is
  undefined behaviour on most platforms, which is why `std::env::set_var`
  is `unsafe`. So `Std` does not change it: `seam_std::env::WritableEnv`
  does, made by an `unsafe fn new()` that carries `set_var`'s contract, and
  `!Send`, so it stays on the thread the promise was made for. It reads
  the environment too, so it can serve `kinds::Env` in a root. It is the
  only `unsafe` in seam: the workspace denies `unsafe_code`, every crate
  but seam-std forbids it, and seam-std allows it on those items alone.
- `process`: `RunCommand`, `Exit` (outcome `Infallible`), and `SpawnChild`,
  an acquisition lending a running child to a scope: written to and read
  like a stream (stdin and stdout), with `ReadErr`, `CloseStdin`, `Wait`,
  `Kill` and `ChildId`
- `random`: `FillRandom` (no_std)
- `stdio`: acquisitions `OpenStdin<H>`, `OpenStdout<H>` and `OpenStderr<H>`,
  lending handles that `Read`, or `Write` and `Flush`
- `signal`: `CtrlC`
- `sync` (feature `sync`, no_std, no alloc): `Channel<S, R>` creating owned
  sender and receiver endpoints, with `SendMsg` and `RecvMsg`;
  `Share<T, C>` creating a cloneable cell, with `Load` and `Update` (an
  atomic read-modify-write that borrows its closure, so bounds can name
  it). See **Shared state** below.
- `task` (feature `rt`, no_std, no alloc): the acquisitions `Spawn<F, H>`,
  `SpawnSet<S>` and `SpawnWith<A, B, H>`; `Join<T>`, `Abort`, `IsFinished`
  and `Detach` on a handle; `SpawnInto<F>` (which hands the future back if a
  fixed-capacity set is full) and `JoinNext<T>` on a set; `YieldNow`. A bound
  must name the spawned future's type, so a core takes the task as a
  function argument (the type is inferred at the call site), and a
  `SpawnWith` body is a named `TaskBody` type, whose `run` declares a `Send`
  future. With `alloc`, `Spawn::boxed` and `SpawnWith::boxed` accept
  unnameable futures and closures instead. Tasks are `'static`, so they get
  their own clone of the root capability (`Std` and `Tokio` are `Clone`).

Outcome types must be constructible without the OS, or a test backend could
not produce them. So `Now` returns seam's own `time::Instant` (a duration
since a backend-chosen epoch) instead of `std::time::Instant`, `Metadata`
returns `fs::FileInfo` instead of `std::fs::Metadata`, and `RunCommand`
returns `process::Output` instead of `std::process::Output`.

I/O outcomes use seam's own `io::Error` for the same reason, and so that
`io`, `net`, `stdio` and `signal` need no std. It is an `ErrorKind` (std's
kinds, `non_exhaustive`) and a `&'static str`, so building one allocates
nothing and works on any target. With std it also carries the
`std::io::Error` it was converted from, and converts back to it losslessly.
That field exists only under std, but nothing depends on its absence:
features only add, so a no_std library and a std binary in one build see
one type. The alternative, using `std::io::Error` when std is on and a
custom type when it is off, would make a feature change a type, and two
crates in a build would disagree. `net` has its own `How` (for `Shutdown`)
for the same reason. Only the helpers returning owned data (`read_to_end`,
`resolve`, ...) need `alloc`: the primitives fill buffers the caller
passes, `ResolveInto` reporting how many addresses there were, so a caller
can tell its buffer was too small, and `BufReader` handing back a record
too long for its buffer a piece at a time (`Line::complete`). `fs`,
`process` and `env` need std for `Path`, `Command` and `OsString`.

Operations borrow their inputs: paths are `&Path`, buffers `&[u8]`, `Var`'s
name `&OsStr`, `ResolveInto`'s host `&str`. Describing an
operation then allocates nothing, and a caller that already holds a
`PathBuf` lends it instead of cloning it. The cost is in bounds, which name
the operation for every lifetime: `for<'a> Inspect<Metadata<'a>>`, as the
`io` operations' buffers always did. A provider that must keep an input
past the call copies it: an atomic file keeps its target path for the
rename on close, and Tokio's blocking pool needs owned paths, which Tokio
itself copies. Listing a directory is an acquisition rather than a query
returning a `Vec`, so a core holds one entry at a time and can stop early.

Acquisitions are generic over the backend's handle type (`Open<'a, M, F>`), since
each backend has its own. Bounds name the handle as a type parameter and
inference fills it in.

`Push<T>` (outcome `Result<(), Rejected<T>>`) lets tests count or cap the
items a core emits, to check that output scales with input size.

## Primitives and helpers

Everything that can be built from the primitives is, in seam-core, as plain
functions beside the operations they use, not as operations:

| Helper | Built from |
|---|---|
| `read_exact`, `write_all`, `copy`, `read_to_end` (alloc) | `Read`, `Write` |
| `BufReader<N>`: `read_until`, `read_line`, `read` | `Read`, through a fixed buffer |
| `read_file`, `read_to_string` | `View`, `ReadAt` |
| `write_file` | `Open` and `write_all` |
| `create_dir_all` | `CreateDir`, `Metadata` |
| `sweep_atomic` | `ReadDir`, `Metadata`, `RemoveFile`, `WallClock` |
| `resolve` (alloc) | `ResolveInto` |
| `read_err_to_end` | `ReadErr` |
| `random_u64` | `FillRandom` |
| `print`, `eprint` | `OpenStdout`/`OpenStderr` and `write_all` |
| `try_send`, `try_recv` | `SendMsg`/`RecvMsg` with a `Timeout` of zero |
| `store` | `Update` |

A backend then implements only the primitives, and each helper works on
every backend, and on every handle that has the primitives it needs: lines
come from stdin, a socket, a child or a file through the same `BufReader`.
The reader keeps what it read past a record, in the core, where std's
`BufReader` would keep it; a stream is read through one reader for as long
as it is in use.

**Three forms.** Rust cannot be generic over sync and async, nor over
whether a future is `Send`, which is why the capability traits come in
three forms. Helpers do too: `write_all` (`Perform`), `write_all_async`
(`AsyncPerform`) and `write_all_send` (`SendPerform`, so that a generic
core spawned on a multithreaded runtime can prove its future is `Send`).
A crate-private macro, `three_ways!`, writes all three from one body, in
which `run!`, `ask!`, `acquire!`, `lend!` and `call!` perform an effect, run
a query, open a scope (or lend one) and call another helper in the form
being generated.

**What stays a primitive.** Four effects could be built from others but are
not, each because the built version would be worse in a way that matters:

- `CreateAtomic`: its promise is that a failed, panicking or cancelled
  scope deletes the temporary file. That cleanup runs in `Drop`, and a core
  cannot perform an effect in `Drop`.
- Recursive `RemoveDir`: a walk over `RemoveFile` that follows a symlink
  to a directory deletes outside the tree, and doing it safely while other
  processes change the tree needs platform calls (std's `remove_dir_all`
  has had a security fix for exactly that race, CVE-2022-21658).
- `CopyFile`: platforms copy faster than a loop of reads and writes
  (reflinks, `copy_file_range`), and copy permissions with the contents.
- `RunCommand`: collecting both outputs means reading the two pipes at
  once, or the child fills one while the core waits on the other; a core
  has no way to read two at once.

**Modes as types.** `Open<'a, M, F>` names its mode as a type, `ReadOnly`,
`WriteOnly`, `Append` or `ReadWrite`, and a backend's `File<M>` implements
`Read` only for readable modes and `Write`, `SetLen` and the syncs only for
writable ones. A core bounded on `Open<'a, ReadOnly, F>` cannot write, and
the compiler checks it, so bounds stay an audit even though reading a file
is now "open, then read".

**Costs.**

- **Bounds are longer.** `read_file` needs `for<'a> Lend<View<'a, F>>`
  and `F: for<'a> Inspect<ReadAt<'a>>` where one `Inspect` bound used to
  do, and a core names a type parameter per handle type (`F` for files,
  `O` for stdout). Inference fills them in.
- **Timeouts cover primitives only.** A helper made of several has no timed
  form; a core that needs one times each primitive against a deadline it
  keeps.
- **No sync non-blocking receive on async-only channels.** `try_recv` is a
  zero `Timeout`, which Tokio's and the simulated runtime's receivers
  implement asynchronously, so sync code holding one (inside an `Update`,
  say) cannot use it; async code uses `try_recv_send`.
- **Writing a whole file syncs it.** `write_file` opens, writes and
  closes, and closing a writable file syncs it, where `std::fs::write` does
  not.

## Backends

Each backend has a zero-sized capability (`Std`, `Tokio`), used alone or as
a part of a `Root`, constructed at the edge, and newtype handles that
expose no std or Tokio I/O traits and no way back to the inner value. Every
effect on them goes through `Perform`/`Inspect`.

In the `seam` umbrella crate, capability-group features (`fs`, `time`, ...)
never enable a backend; backends (`std`, `tokio`, `test`) are only ever
enabled by the end user. Tokio's runtime options (`rt`, ...) do nothing
without `tokio`.

Feature flags: `seam-std` has `fs`, `net`, `time`, `env`, `process`,
`random`, `io-std`, `sync`, `rt` and `full`. `seam-tokio` copies Tokio's
names (`rt`, `rt-multi-thread`, `fs`, `net`, `time`, `process`, `io-std`,
`signal`, `sync`, `io-util`, `test-util`, `full`); it has no `env` or
`random`, which a Tokio program takes from `Std` (see **Roots**). Tokio's
`macros` and `parking_lot` are not mirrored: `#[tokio::main]` cannot be
re-exported without exposing Tokio itself.

## Test backend

`seam-test` provides `Sim`, a simulated world implementing every operation
group with no real effects other than allocation:

- **time**: a virtual monotonic clock and wall clock (starting at the Unix
  epoch). `Sleep` advances both and returns at once.
- **random**: SplitMix64 from a seed. Same seed, same sequence.
- **fs**: an in-memory tree of inodes. Errors match a real system
  (`NotFound`, `IsADirectory`, `NotADirectory`, `DirectoryNotEmpty`, ...),
  `..` only steps out of directories that exist, and open handles survive
  renames, as on Unix: `Stat` on a handle follows its file, not its path.
  `ReadDir` lists a snapshot taken when it is opened, sorted by name. Files
  seek past their end (a write there fills the gap with zeros), and
  `SetLen` needs a handle open for writing (`InvalidInput`, as on Linux).
  Nothing is lost in a simulated crash, so `SyncAll` and `SyncData` do
  nothing. An atomic file's temporary file is visible beside its target
  until the scope ends. No symlinks or permissions.
- **net**: in-memory TCP (pipe pairs, with shutdown and `BrokenPipe`) and UDP
  (truncation, drops to unbound addresses), plus a hosts table for
  `ResolveInto`.
- **env**, **io-std**: configured variables, arguments and directories,
  with `SetVar` and `RemoveVar` changing the variables (no `unsafe`
  needed: nothing real changes); scripted stdin, captured stdout and
  stderr, in the shared world so that the stdio handles reach them. Input
  is a finite script: once it is read, reads report the end of input.
- **process**: commands are answered by scripted handlers and logged. A
  spawned child is a filter: its handler runs the first time its output is
  needed (a read, a wait, or the end of its scope), on the input written
  so far, and later input is refused. Children inherit nothing until they
  run; their inherited output is printed then, at spawn or when the scope
  closes.
  `Exit` unwinds with an `Exited` payload, without running the panic hook;
  `catch_exit` turns it back into a code.
- **signal**: `raise_ctrl_c` queues a Ctrl-C for `CtrlC` to consume.

Used directly, nothing blocks: operations that would wait on a real system
fail with `WouldBlock`. `Sim` is `Send + Sync`, so `Send`-bound async cores
(the kind written for Tokio) run in it through the sync lifts and
`block_on`, as long as they never need another task to make progress.

### The deterministic runtime

`seam_test::rt::Runtime` owns a `Sim` and runs async code on a
single-threaded executor. Cores get `AsyncSim`, a cloneable capability over
the same world. Operations that never wait delegate to the `Sim`, so they
behave identically in both modes. Those that would wait are native async:

- **Waiting.** Reads, accepts and receives on the runtime's own sockets
  (`rt::net`) suspend, and the in-memory network wakes exactly the waiters
  of the pipe, listener or socket that changed. `CtrlC` waits for
  `AsyncSim::raise_ctrl_c`. `YieldNow` lets another task run.
- **Virtual time.** `Sleep` registers a timer. When no task is ready, the
  executor advances the clock to the earliest deadline and wakes the timers
  due then, so time passes only when everything is waiting.
- **Seeded scheduling.** The next task is drawn from the ready set with a
  SplitMix64 seeded per runtime, separate from the `Sim`'s `FillRandom`
  stream. Same seed, same interleaving; sweeping seeds explores others.
- **Deadlocks.** No task ready and no timer set means nothing can ever
  happen: `try_block_on` returns `Deadlock`, `block_on` panics.
- **Tasks.** `Spawn`, `SpawnSet` and `SpawnWith` follow the same scope rules
  as Tokio. A task's panic is caught and reported as `Panicked`; dropping a
  task before it finishes reports `Cancelled`. Aborts are deferred to the
  executor so a task is never dropped mid-poll. When `block_on` returns,
  remaining tasks are dropped.

Handles outlive the borrow of the `Sim` that made them, so the world state
they reach (filesystem, network, wall clock) is shared behind an
`Arc<Mutex<_>>`. That, the runtimes' own bookkeeping, and the simulated
cells and channels are the crate's only shared state; it is linted with
`templates/clippy.toml`, with an allow on each of those modules alone.

### Deterministic threads

`seam_test::threads::Threads` is the same idea for sync cores that spawn
threads. Each simulated thread runs on a real OS thread, so a core's
closures, blocking calls and stack all work as written, but a scheduler
lets only one run at a time, handing over through a condvar.

- **Scheduling points.** Every operation on `ThreadSim`, and on its
  channels, cells, sockets and thread handles, first hands control to the
  scheduler, which draws the next runnable thread from its seed. Cores
  share state only through those operations, so switching at each one
  reaches every interleaving that can change what a core observes, and
  the same seed replays the same one.
- **Blocking.** An operation that would block a real thread polls the
  simulated world with a waker that marks its thread runnable, and leaves
  the runnable set until woken: channel receives and sends to a full
  channel, joins, reads, accepts, receives, and `CtrlC`. `Sleep` registers
  a timer; with no thread runnable, the clock jumps to the earliest one.
  With no timer either, the run halts as a deadlock.
- **Cells behave as mutexes.** An `Update` holds its cell for the whole
  closure, even if the closure performs effects (and so lets other threads
  run); other threads touching the cell block until it is released. A cell
  updated from inside its own update is a deadlock, as with a real mutex.
- **Thread rules match `seam-std`.** No abort; a scope that fails or
  panics joins its threads, unless detached; a panic is `Panicked`.
- **Halting.** When the main closure returns, or on a deadlock, every
  other thread is unwound with a private payload (via `resume_unwind`, so
  no panic message), and drops that would block return at once instead.

### World files

`Sim::from_world` sets a world up from a KDL file, and `check_world` checks
one against expectations in the same format. KDL was chosen for its tree
shape: blocks per kind of operation (`env`, `fs`, `net`, ...), and
directories that nest the way they do on disk. Each directive is one of
`Sim`'s setup methods, so the format cannot describe a world the simulation
does not support.

Ordering follows KDL: nodes and arguments are ordered, properties are not.
So anything whose order matters is written as nodes or arguments, and
properties only carry named options (`exit=`, `stdout=`). Directives apply
in document order and the last setting wins, which is also how one file
layers over another. The simulated filesystem lists directories by name,
whatever order the files were written in.

Expectations check only what they list: the clock, variables (`var`,
`unset`), files, output and commands. `stdout` and `stderr` compare exact
bytes, and `ran` lines must be every command, in order. Everything is
checked before reporting, so one run shows every difference.

The parser is a small one for the subset of KDL v2 world files use (no type
annotations, integers only), rather than the `kdl` crate, which keeps
seam-test free of dependencies and lets errors speak in the world's terms.
Paths are quoted, since KDL does not allow `/` in a bare word; the parser
says so when it meets one. Dhall was considered for its types and imports,
but its one Rust implementation is heavy, can fetch imports over the
network, and adds programmability that tests already have in Rust.

## Resources: forced scopes

Effects that produce something lasting (a file, a socket, a task) are
`Acquisition`s. A core never owns one; it uses one for the length of a
scope:

```rust
pub trait Acquisition { type Resource; type Error; }

pub struct Opening<A>(pub A, pub Token);                       // produces the resource
pub struct Closing<A: Acquisition>(pub A::Resource, pub PhantomData<fn() -> A>); // commits it

// Names for the pair, with a blanket impl each; the methods run a scope.
pub trait Provide<A>: Perform<Opening<A>> + Perform<Closing<A>> { fn acquire(..) }
pub trait Lend<A>: Inspect<Opening<A>> + Inspect<Closing<A>> { fn lend(..) }
// AsyncProvide, AsyncLend: the same over AsyncPerform, AsyncInspect.
// SendProvide, SendLend: bounds over SendPerform, SendInspect; their scopes
// are `acquire_send` and `lend_send`, on `SendScope`.

c.acquire(a, |c, resource| -> Result<R, E> { .. })
c.lend(a, |c, resource| -> Result<R, E> { .. })                 // c: &C
c.acquire_async(a, async |c, resource| -> Result<R, E> { .. })
```

**A scope is two operations.** Opening and closing are ordinary
operations, so a scope is a collection of effects like any other, and
everything built for operations works for scopes without a second set of
traits: the async and `Send` forms, the lifts from sync, a `Root` routing
them to a part by kind, and a wrapper forwarding every `Perform<O>`. A
backend implements `Perform<Opening<A>>` and `Perform<Closing<A>>`.

The body borrows the resource, so it cannot keep, leak or drop it, and gets
the capability back, so it can perform other effects and nest scopes. `Ok`
performs `Closing` and returns any error from it (through
`E: From<A::Error>`); `Err`, a panic, or dropping the async future drops
the resource, which rolls it back.

`Opening` holds a `Token` that only `seam-core` can create, so a core
cannot open a resource outside a scope, and only a scope ever holds a
resource by value, so only a scope can close one. A provider that opens by
asking another passes its token on: `Opening(other, token)`. The remaining
escape hatch is a core implementing `Perform<Opening<_>>` itself to
intercept a token, which shows up in its source.

**Scopes that are queries.** A scope can change nothing outside itself:
reading a file opened read-only moves only a cursor that dies with the
scope. Such a scope is a query, the way a function that mutates only its
own variables is pure, so its provider implements `Inspect` for both
operations and the scope is `Lend`, which needs only `&C`. The lift from
`Inspect` to `Perform` makes every lending capability a providing one.
`View` (a regular file read at offsets, with `ReadAt`, which moves no
cursor at all) and `ReadDir` are lent; `read_file` and `read_to_string`
take `&C`. A FIFO or a device is not a regular file: reading one consumes
what it reads, so `View` refuses it, and it is opened with `Open`.

**`Send` futures.** A scope's method is defined in its trait's only
(blanket) impl rather than as a default, so a call on a concrete type sees
the future's auto traits. Generic code bounded on `SendProvide` would still
resolve `acquire_send` through its bound, which hides the future's type, so
the `Send` forms' methods live on `SendScope` instead: implemented for
every type and named in no bound, its method calls resolve to the impl,
and a generic core's future is `Send` whenever what it holds is.

**Drop means roll back.** A provider must make dropping a resource leave
things as they were; only closing commits. A future can be dropped at
any `.await`, including halfway through opening or closing, so this is what
makes scopes cancellation-safe. Tokio's `AtomicFile` creates its temporary
file and renames it inside blocking tasks that own the cleanup guard: a
blocking task runs to completion even if its waiter is dropped, and Tokio
drops its result if nobody is waiting, so every cancellation point ends in
either the rename or the deletion. A test cancels an atomic write after
every possible number of polls; a naive version fails it.

**Tasks.** Spawning is an acquisition too: a successful scope waits for its
task, a failed or cancelled one aborts it, and `Detach` opts out. Because a
core cannot move a resource, `SpawnWith` has the provider acquire one on a
task's behalf (a listener accepting a connection into a new task that owns
it), and `SpawnSet` opens a set to spawn a batch into, since handles cannot
be collected outside their scopes. Neither allocates in seam; storing tasks
is the backend's business, so a backend for an executor with static task
storage could implement them without `alloc`.

**Threads.** On `seam-std` a task is a closure on its own OS thread, so
the same `Spawn`, `SpawnSet` and `SpawnWith` acquisitions work for sync
cores. A running thread cannot be stopped from outside, so thread handles do
not implement `Abort`, and dropping one (a failed or panicking scope)
joins it instead, as `std::thread::scope` does: no thread outlives its
scope unless detached. Stopping a thread early is cooperative, through a
`Shared<bool>` or a channel. Threads cannot borrow from their spawner:
`std::thread::scope` hands out its scope only inside a callback, which does
not fit an open-then-close provider without `unsafe`.

**Leftovers.** A process killed outright runs no `Drop`. Temporary files are
named `.<name>.seam-<suffix>.tmp`, and `sweep_atomic(c, dir, older_than)`
removes ones old enough to be abandoned.

**Cost.** A body's error type is generic, so a body whose only error path is
a `?` sometimes needs its `Ok` annotated (`io::Result::Ok(())`).

For long-lived state in hand-written state machines, the plan is raw IDs
with generation counters (runtime detection of stale handles).

## Async

- Sync lifts to async for free: blanket impls give every `Inspect`/`Perform`
  an `AsyncInspect`/`AsyncPerform`, and a `SendInspect`/`SendPerform` when
  it is thread-safe.
- Async lowers to sync with `seam_core::future::block_on`, for cores whose
  capabilities are all sync (`Std`, `Sim`, fakes); their futures are ready on
  the first poll. With `std`, a pending future parks the thread until woken,
  so a misuse with an externally woken future sleeps instead of spinning.
  Without `std` it re-polls in a spin loop. Futures from a real runtime need
  that runtime: `seam_tokio::block_on` starts a Tokio one and runs a future on it.
- Because the sync lift is a blanket impl, there is no second blanket from
  `AsyncInspect` to `AsyncPerform`. Native async query implementations
  implement both (`seam-tokio` does this with a macro).
- `seam-tokio` futures are `Send` and are tested on Tokio's multithreaded
  runtime.
- Raising does not make a backend non-blocking: a raised `Std` operation
  blocks the runtime's thread. That is documented, not prevented. Blocking
  only some operations from being raised was considered (a marker per
  backend and operation that the async lifts require) and dropped: it is
  slow rather than wrong, is sometimes what a program wants (a CLI under
  `block_on`, a test, an instant operation like `Now`), and would cost two
  traits and a list of marks per backend.

## Shared state

Shared state is an effect, so channels and cells come from the backend and
every send, receive, load and update is an operation. On the deterministic
runtime that makes them schedulable: a receive waits until a message
arrives, a send waits for room, and a receiver waiting on an idle channel is
reported as a deadlock.

Unlike files and sockets, endpoints and cells are **owned values**, not
scoped resources: a sender must be able to reach another task, and a task
cannot borrow from its spawner. They hold nothing that needs committing, so
dropping one is always correct. The cost is unbounded lifetimes: a core can
keep a sender alive forever, so a channel never closes (the simulation
reports the resulting deadlock rather than hanging).

Cancellation safety holds at the level of each operation: every one is
atomic. A receive cancelled while waiting takes no message, a send cancelled
while waiting enqueues nothing, and `Update` cannot be interrupted. The
runtime tests cancel sends and receives after every number of polls and
check that every message sent arrives exactly once; a receive that yields
between taking and returning a message fails that test.

Considered and deferred: scoped `Claim` (a received message redelivered
unless its processing succeeds) and `Modify` (an optimistic transaction on a
cell, written back only if the scope succeeds and nobody else changed it).
Both are acquisitions on top of `acquire`, and can be added without changing
anything here.

## Timeouts

`Timeout<O>` wraps an operation (or an acquisition) that waits. Rather than
a new outcome type such as `Result<O::Outcome, Elapsed>`, it reuses `O`'s
own: each waiting operation implements `TimesOut`, which builds the outcome
that reports giving up. That keeps one error path per operation, and it is
what lets a timed-out send hand its value back (`SendError::Full(value)`),
which a generic `Elapsed` could not. A timeout of zero is a non-blocking
attempt, so the `Try*` operations are `Timeout` with zero, kept for clarity.

Expiry has no effect, which each backend gets differently:

- **seam-std** sets the socket's own read or write timeout around the call
  (zero: nonblocking), and waits on condvars with a deadline for channels and
  joins. The channel is seam's own queue and condvars, because
  `std::sync::mpsc` cannot time out a send. std has no timed accept, so a
  timed accept polls the listener, sleeping up to 10ms between tries.
- **seam-tokio** wraps the call in `tokio::time::timeout`, choosing calls
  that are cancel-safe: a timed send reserves a slot first, so the value
  stays in hand until there is room. The inner future is polled before the
  deadline is checked, so a timeout of zero is a non-blocking attempt.
- **The runtime** races the operation against a timer. Every operation that
  waits there is already atomic under cancellation, so the race needs nothing
  per operation, except the send, which polls by hand to keep its value.
- **Simulated threads** wait with a deadline in the scheduler.
- **The plain `Sim`** never waits, so a timed operation that cannot finish
  at once expires at once, and the clock does not move.

On the runtime and the thread scheduler, a timer that loses its race is
removed. Otherwise it would still move the clock when nothing else is left
to run, and a deadlock would be reported later than it happened; the tests
check for exactly that.

## Roots

A `Root<A>` is one capability made of parts: `A` is a struct of them, and
each part serves one or more kinds of operation. Every operation that goes
through a root names its kind (`InKind`); operations on handles (`Read`,
`RecvMsg`, `Join`) never do, since the handle a part hands out implements
them itself. A kind is a marker type, so a program's own kinds are as cheap
to define as seam's (`kinds::Fs`, `kinds::Time`, ...).

The root implements each capability trait once, generically: find the part
serving the operation's kind (`<O::Kind as Pick<A>>::Part`) and forward to
it in the same form. Two things make that legal in Rust:

- **Routing by kind, through `Pick`.** Rust cannot express "forward to
  whichever part implements this operation": two parts might, and
  coherence rejects the rule outright, not just the duplicate. `Pick<A>` is
  implemented on a kind, per root, naming the field that serves it. A kind
  listed twice is then a plain "conflicting implementations" error, and a
  missing one is "the root `App` has no part for `kinds::Fs`".
- **Opt-in lifts.** seam's blanket impls (an `Inspect` is a `Perform`, a
  sync operation is an async one) would collide with the root's own
  forwarding impls: both would claim, say, `AsyncPerform<Sleep>` for the
  root. So the lifts apply only to types that implement `Capability`, a
  marker every backend type carries and `Root` does not. The root then
  forwards each form itself, which is what lets a sync `Std` part and an
  async `Tokio` part sit in one root. The cost is one line on each
  capability type a program writes: `impl Capability for Memory {}`.

The `Send` forms hand back the part's own future, so parts behind them are
`'static`. A part can serve several kinds, which is how a whole backend
(`Std`, `Tokio`, `Sim`, `ThreadSim`) is a part as it stands; nothing had to
be split. `root!` is sugar: it evaluates to the root, defining a struct
generic over its parts' types (so they are inferred from the values) with
its `Pick` impls; a named form defines a plain struct instead.

Each backend covers only what it is for. `seam-tokio` has no environment
or randomness, since Tokio has nothing to add to them; a Tokio program
takes `kinds::Env` and `kinds::Random` from `Std` in the same root. A root
has no singleton or lock: roots are built at the edge, and the clippy template
is what keeps cores from reaching for effects directly.

Alternatives explored and set aside are fixed per-domain slots
(`Root<Env, Time, Fs, ...>`: no room for a program's own kinds), a tuple
with type-level keys (two root types and bad errors), macro-generated roots
that list every operation, and accessors (`c.part::<Fs>()`), which would
lose plain `c.perform(op)` calls.

## Prelude

`seam_core::prelude` (re-exported as `seam::prelude`) holds the traits and
`block_on`, but no operation types: those are imported one by one, so that a
module's imports, like its bounds, show what it can do.

## Future work

Deferred by design:

- **Allocation as a capability.** The plan was an explicit `A: Allocator`
  generic in the core, fallible APIs only, and clippy bans on the `Global`
  escape hatches. `allocator_api` is still unstable (checked on 1.99.1), so
  this waits.
- **`Pin<Box<_>>` sink variant** for writers that are not `Unpin`.
- **Interactive simulated children.** A simulated child is a filter that
  runs once on the input it was given. A child that answers line by line
  would run as its own task or thread on the runtime or the scheduler,
  written as a core against its stdin and stdout.

Not implemented yet:

- **Signals.** `CtrlC` on `seam-std`, which needs a dependency
  (`signal-hook` or `ctrlc`). Other signals (`SIGTERM`, `SIGHUP`, or any by
  number) on every backend, for graceful shutdown.
- **Time.** `SleepUntil(Instant)` and an interval timer, so a periodic loop
  does not drift.
- **Network.** TCP options (`nodelay`, keepalive, TTL); connected UDP
  (`connect`, `send`, `recv`), broadcast and multicast; Unix domain
  sockets. Tokio streams shut down only their write half; shutting down
  the read half reports `Unsupported`.
- **Filesystem.** Symlinks and hard links (create, read, stat without
  following), `Canonicalize`, setting permissions and modification times,
  and recursive copy and listing. The simulation has no symlinks or
  permissions, so its `FileInfo::readonly` is always false.
- **Processes.** A gentler `Kill` (`SIGTERM`), the signal that ended a
  child in `ExitStatus`, and the process's own ID.
- **Environment.** `SetCurrentDir` (safe in std, unlike `set_var`) and
  listing every variable.
- **A lint for blocking under async.** A `dylint` lint, in its own crate,
  warning where a concrete backend meets an async bound for an operation it
  does not mark as instant (`block_on(core(&mut Std::new()))` with a core
  that reads files). It needs a per-backend `Instant<O>` marker for the
  operations that never wait, and checks after monomorphization; users
  would run it with `cargo dylint`.
- **Polish.** An optional run-time message on `io::Error` behind `alloc`;
  `Sim::list_dir` for tests; buffer-filling forms of the std-only
  primitives that still return owned data (`Args`, `Var`, `NextEntry`).

## Open questions

- How capability requirements are bundled in signatures. Long `where`
  clauses get unwieldy; a single requirements trait with a blanket impl, plus
  `impl Trait`, is one option.
- Whether a root's kinds should be finer than one per ops module. Today a
  root cannot take `Now` from one part and `Sleep` from another (both are
  `kinds::Time`) without a small part combining the two.
- Whether to add per-resource identity for conflict detection, or leave
  ordering to the caller.
