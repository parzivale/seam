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
  `Open<F>`
- `net`: `Resolve` (DNS is an effect), `TcpConnect<S>`, `TcpBind<L>`,
  `Accept<S>`, `UdpBind<U>`, `SendTo`, `RecvFrom`, `Shutdown`; `LocalAddr`
  and `PeerAddr` queries
- `time`: `Sleep`, `Now` (no_std); `WallClock` (std)
- `env`: `Var`, `Args`, `CurrentDir`, `TempDir` (all queries)
- `process`: `RunCommand`, `Exit` (outcome `Infallible`)
- `random`: `RandomU64`, `FillRandom` (no_std)
- `stdio`: `Print`, `EPrint`, `ReadLine`
- `signal`: `CtrlC`

Outcome types must be constructible without the OS, or a test backend could
not produce them. So `Now` returns seam's own `time::Instant` (a duration
since a backend-chosen epoch) instead of `std::time::Instant`, `Metadata`
returns `fs::FileInfo` instead of `std::fs::Metadata`, and `RunCommand`
returns `process::Output` instead of `std::process::Output`.

Operations that create a resource are generic over the backend's handle
type (`Open<F>`), since each backend has its own. Bounds name the handle as a
type parameter and inference fills it in.

`Push<T>` (outcome `Result<(), Rejected<T>>`) lets tests count or cap the
items a core emits, to check that output scales with input size.

## Backends

Each backend has a zero-sized root capability (`Std`, `Tokio`) constructed at
the edge, and newtype handles that expose no std or Tokio I/O traits and no
way back to the inner value. Every effect on them goes through
`Perform`/`Inspect`.

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

## Resources and finalization

```rust
pub trait Finalize: Sized {
    fn close(self);      // normal completion: flush, sync, commit
    fn abandon(self) {}  // unwinding: discard, do not sync
}
pub trait Scoped: Finalize { fn scope<R>(self, f: impl FnOnce(&mut Self) -> R) -> R; }
impl<T: Finalize> Scoped for T { /* drop guard: abandon on panic, close on return */ }
```

`Scoped` has a single blanket impl, so implementors cannot override it and
skip cleanup. `close` has no error channel; a core that must see sync errors
performs `Flush` itself before the scope ends.

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

- **Spawning** as an operation. Without it a core cannot start concurrent
  tasks, so the `echo_server` example serves one connection at a time.

- **Allocation as a capability.** The plan was an explicit `A: Allocator`
  generic in the core, fallible APIs only, and clippy bans on the `Global`
  escape hatches. `allocator_api` is still unstable (checked on 1.99.1), so
  this waits.
- **`Pin<Box<_>>` sink variant** for writers that are not `Unpin`.
- **A `Resource` trait** for code generic over resources, and Tokio-side
  `Finalize` (closing a Tokio file needs an async close).
- **Child process handles** (only run-to-completion today).

## Open questions

- How capability requirements are bundled in signatures. Long `where`
  clauses get unwieldy; a single requirements trait with a blanket impl, plus
  `impl Trait`, is one option.
- Whether to add per-resource identity for conflict detection, or leave
  ordering to the caller.
