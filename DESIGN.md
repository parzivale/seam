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
feature, so a core written against them runs on any backend:

- `io`: `Read`, `ReadToEnd`, `Write`, `WriteAll`, `Flush` (on handles)
- `fs`: `ReadFile`, `ReadToString`, `Metadata`, `ReadDir` (queries);
  `WriteFile`, `CreateDir`, `RemoveFile`, `RemoveDir`, `Rename`, `CopyFile`,
  `SweepAtomic`; acquisitions `Open<F>` and `CreateAtomic<F>`
- `net`: `Resolve` (DNS is an effect), `SendTo`, `RecvFrom`, `Shutdown(How)`;
  `LocalAddr` and `PeerAddr` queries; acquisitions `TcpConnect<S>`,
  `TcpBind<L>`, `Accept<S>` (from a listener) and `UdpBind<U>`
- `time`: `Sleep`, `Now` (no_std); `WallClock` (std)
- `env`: `Var`, `Args`, `CurrentDir`, `TempDir` (all queries)
- `process`: `RunCommand`, `Exit` (outcome `Infallible`), and `SpawnChild`,
  an acquisition lending a running child to a scope: written to and read
  like a stream (stdin and stdout), with `ReadErr`, `CloseStdin`, `Wait`,
  `Kill` and `ChildId`
- `random`: `RandomU64`, `FillRandom` (no_std)
- `stdio`: `Print`, `EPrint`, `ReadLine`
- `signal`: `CtrlC`
- `sync` (feature `sync`, no_std, no alloc): `Channel<S, R>` creating owned
  sender and receiver endpoints, with `SendMsg`/`TrySendMsg` and
  `RecvMsg`/`TryRecvMsg`; `Share<T, C>` creating a cloneable cell, with
  `Load`, `Store` and `Update` (an atomic read-modify-write that borrows its
  closure, so bounds can name it). See **Shared state** below.
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
for the same reason. The operations returning owned data (`ReadToEnd`,
`Resolve`, `ReadLine`) need `alloc`; `fs`, `process` and `env` need std for
`Path`, `Command` and `OsString`.

Acquisitions are generic over the backend's handle type (`Open<F>`), since
each backend has its own. Bounds name the handle as a type parameter and
inference fills it in.

`Push<T>` (outcome `Result<(), Rejected<T>>`) lets tests count or cap the
items a core emits, to check that output scales with input size.

## Backends

Each backend has a zero-sized capability (`Std`, `Tokio`), used alone or as
a part of a `Root`, constructed at the edge, and newtype handles that expose no std or Tokio I/O traits and no
way back to the inner value. Every effect on them goes through
`Perform`/`Inspect`.

In the `seam` umbrella crate, capability-group features (`fs`, `time`, ...)
never enable a backend; backends (`std`, `tokio`, `test`) are only ever
enabled by the end user. Tokio's runtime options (`rt`, ...) do nothing
without `tokio`.

Feature flags: `seam-std` has `fs`, `net`, `time`, `env`, `process`,
`random`, `io-std`. `seam-tokio` copies Tokio's names (`rt`,
`rt-multi-thread`, `fs`, `net`, `time`, `process`, `io-std`, `signal`,
`io-util`, `test-util`, `full`) plus `env` and `random`, which run
synchronously. Tokio's `sync`, `macros` and `parking_lot` are not mirrored:
there is nothing for seam to wrap yet, and `#[tokio::main]` cannot be
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
  renames, as on Unix. No symlinks or permissions.
- **net**: in-memory TCP (pipe pairs, with shutdown and `BrokenPipe`) and UDP
  (truncation, drops to unbound addresses), plus a hosts table for
  `Resolve`.
- **env**, **io-std**: configured variables, arguments and directories;
  scripted stdin, captured stdout and stderr.
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
  SplitMix64 seeded per runtime, separate from the `Sim`'s `RandomU64`
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

Expectations check only what they list. `stdout` and `stderr` compare
exact bytes, and `ran` lines must be every command, in order. Everything is
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
`Acquisition`s, not operations. A core never owns one:

```rust
pub trait Acquisition { type Resource; type Error; }

pub trait Provide<A: Acquisition> {          // AsyncProvide: the same, async
    fn provide(&mut self, a: A, token: Token) -> Result<A::Resource, A::Error>;
    fn close(&mut self, r: A::Resource) -> Result<(), A::Error>;  // commit
    fn abandon(r: A::Resource) {}                                  // roll back
}

c.acquire(a, |c, resource| -> Result<R, E> { .. })                 // Acquire
c.acquire_async(a, async |c, resource| -> Result<R, E> { .. })     // AsyncAcquire
```

Cores bound on `Provide`/`AsyncProvide` and call `acquire`/`acquire_async`,
which come from a single blanket impl each. The body borrows the resource,
so it cannot keep, leak or drop it, and gets the capability back, so it can
perform other effects and nest acquisitions. `Ok` closes the resource and
returns any close error (through `E: From<A::Error>`); `Err`, a panic, or
dropping the async future abandons it.

`provide` takes a `Token` that only `seam-core` can create, so a core cannot
call it to get a resource outside a scope. A provider that wraps another can
pass its token on. The remaining escape hatch is a core implementing
`Provide` itself to intercept a token, which shows up in its source as an
`impl Provide`.

The commit logic lives with the provider, not the resource, so a resource
type needs no trait of its own. Every `Send` `Provide` is an `AsyncProvide`.
A generic core bounding only `AsyncProvide` (plus `Send`) gets a `Send`
`acquire_async` future, because the blanket impl's preconditions are exactly
its bounds; this was checked before building it.

**Drop means abandon.** A provider must make dropping a resource leave
things as `abandon` would; only `close` commits. A future can be dropped at
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
not implement `Abort`, and abandoning one (a failed or panicking scope, or a
drop) joins it instead, as `std::thread::scope` does: no thread outlives its
scope unless detached. Stopping a thread early is cooperative, through a
`Shared<bool>` or a channel. Threads cannot borrow from their spawner:
`std::thread::scope` hands out its scope only inside a callback, which does
not fit an open-then-close provider without `unsafe`.

**Leftovers.** A process killed outright runs no `Drop`. Temporary files are
named `.<name>.seam-<suffix>.tmp`, and `SweepAtomic { dir, older_than }`
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
  stays in hand until there is room; `ReadToEnd` reads chunk by chunk, so
  data read before the deadline stays in the buffer.
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
takes `kinds::Env` and `kinds::Random` from `Std` in the same root. A root has
no singleton or lock: roots are built at the edge, and the clippy template
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

## Deferred

- **Allocation as a capability.** The plan was an explicit `A: Allocator`
  generic in the core, fallible APIs only, and clippy bans on the `Global`
  escape hatches. `allocator_api` is still unstable (checked on 1.99.1), so
  this waits.
- **`Pin<Box<_>>` sink variant** for writers that are not `Unpin`.
- **Interactive simulated children.** A simulated child is a filter that
  runs once on the input it was given. A child that answers line by line
  would run as its own task or thread on the runtime or the scheduler,
  written as a core against its stdin and stdout.

## Open questions

- How capability requirements are bundled in signatures. Long `where`
  clauses get unwieldy; a single requirements trait with a blanket impl, plus
  `impl Trait`, is one option.
- Whether a root's kinds should be finer than one per ops module. Today a
  root cannot take `Now` from one part and `Sleep` from another (both are
  `kinds::Time`) without a small part combining the two.
- Whether to add per-resource identity for conflict detection, or leave
  ordering to the caller.
