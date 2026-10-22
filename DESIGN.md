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
- `net`: `Resolve` (DNS is an effect), `SendTo`, `RecvFrom`, `Shutdown`;
  `LocalAddr` and `PeerAddr` queries; acquisitions `TcpConnect<S>`,
  `TcpBind<L>`, `Accept<S>` (from a listener) and `UdpBind<U>`
- `time`: `Sleep`, `Now` (no_std); `WallClock` (std)
- `env`: `Var`, `Args`, `CurrentDir`, `TempDir` (all queries)
- `process`: `RunCommand`, `Exit` (outcome `Infallible`)
- `random`: `RandomU64`, `FillRandom` (no_std)
- `stdio`: `Print`, `EPrint`, `ReadLine`
- `signal`: `CtrlC`
- `task` (feature `rt`, no_std): the acquisitions `Spawn<F, H>`,
  `SpawnAll<F, H>` and `SpawnWith<A, T, E, H>` (the last two with `alloc`),
  and `Join<T>`, `Abort`, `IsFinished` and `Detach` on the handle;
  `YieldNow`. A bound must name the spawned future's type, so a core either
  takes the task as a function argument (the type is inferred at the call
  site, no allocation) or spawns a `BoxFuture`. Tasks are `'static`, so they
  get their own clone of the root capability (`Std` and `Tokio` are
  `Clone`).

Outcome types must be constructible without the OS, or a test backend could
not produce them. So `Now` returns seam's own `time::Instant` (a duration
since a backend-chosen epoch) instead of `std::time::Instant`, `Metadata`
returns `fs::FileInfo` instead of `std::fs::Metadata`, and `RunCommand`
returns `process::Output` instead of `std::process::Output`.

Acquisitions are generic over the backend's handle type (`Open<F>`), since
each backend has its own. Bounds name the handle as a type parameter and
inference fills it in.

`Push<T>` (outcome `Result<(), Rejected<T>>`) lets tests count or cap the
items a core emits, to check that output scales with input size.

## Backends

Each backend has a zero-sized root capability (`Std`, `Tokio`) constructed at
the edge, and newtype handles that expose no std or Tokio I/O traits and no
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
- **process**: commands are answered by scripted handlers and logged.
  `Exit` unwinds with an `Exited` payload, without running the panic hook;
  `catch_exit` turns it back into a code.
- **signal**: `raise_ctrl_c` queues a Ctrl-C for `CtrlC` to consume.

Nothing blocks. Operations that would wait on a real system fail with
`WouldBlock`. `Sim` is `Send + Sync`, so `Send`-bound async cores (the kind
written for Tokio) run in it through the sync lifts and `block_on`.

Handles outlive the borrow of the `Sim` that made them, so the world state
they reach (filesystem, network, wall clock) is shared behind an
`Arc<Mutex<_>>`. That is the crate's one use of shared state; it is linted
with `templates/clippy.toml`, with an allow on that module alone.

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
it), and `SpawnAll` spawns a batch, since handles cannot be collected
outside their scopes.

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
  that runtime: `seam_tokio::block_on` starts a Tokio one.
- Because the sync lift is a blanket impl, there is no second blanket from
  `AsyncInspect` to `AsyncPerform`. Native async query implementations
  implement both (`seam-tokio` does this with a macro).
- `seam-tokio` futures are `Send` and are tested on Tokio's multithreaded
  runtime.

## Prelude

`seam_core::prelude` (re-exported as `seam::prelude`) holds the traits and
`block_on`, but no operation types: those are imported one by one, so that a
module's imports, like its bounds, show what it can do.

## Deferred

- **Spawning in the other backends.** Only Tokio implements `Spawn`. The
  test backend would need a deterministic executor; the std backend could
  spawn threads.

- **Allocation as a capability.** The plan was an explicit `A: Allocator`
  generic in the core, fallible APIs only, and clippy bans on the `Global`
  escape hatches. `allocator_api` is still unstable (checked on 1.99.1), so
  this waits.
- **`Pin<Box<_>>` sink variant** for writers that are not `Unpin`.
- **Child process handles** (only run-to-completion today).

## Open questions

- How capability requirements are bundled in signatures. Long `where`
  clauses get unwieldy; a single requirements trait with a blanket impl, plus
  `impl Trait`, is one option.
- Whether to add per-resource identity for conflict detection, or leave
  ordering to the caller.
