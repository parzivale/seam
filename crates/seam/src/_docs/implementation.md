# How seam works

This page walks through seam's machinery, layer by layer: what each piece
is, and why it is built the way it is. The repository's `DESIGN.md` keeps
the longer record of decisions and the alternatives that were set aside.

## The crates

| Crate | Published as | Holds | Depends on |
|---|---|---|---|
| `seam-core` | `seam-rs-core` | the traits, the operation types and their helpers, roots | nothing; `no_std` |
| `seam-std` | `seam-rs-std` | `Std`: every operation, through std | seam-core |
| `seam-tokio` | `seam-rs-tokio` | `Tokio`: the async operations, through Tokio | seam-core, Tokio |
| `seam-test` | `seam-rs-test` | `Sim`, the deterministic runtime and threads, world files | seam-core; nothing else |
| `seam` | `seam-rs` | re-exports, behind features | the above, optionally |

Features come in two independent kinds. **Capability groups** (`fs`, `net`,
`time`, ...) decide which operation types exist, and in whichever backends
are on, their implementations. **Backends** (`std`, `tokio`, `test`) decide
where effects really happen. No group turns on a backend, so a library
depends on the groups its core needs and only the binary picks a backend.

## Operations

An operation is a type, and names its result:

```rust
pub trait Operation {
    type Outcome;
}
pub trait Query: Operation {} // changes nothing outside itself
```

One type per operation, rather than one trait per subsystem, is what lets
everything else be generic: a root forwards *any* operation, a lift lifts
*any* operation, a helper names exactly the operations it uses. Operations
borrow their inputs (`Var<'a>` holds `&'a OsStr`, `Read<'a>` a
`&'a mut [u8]`), so describing one allocates nothing; the cost is
higher-ranked bounds, `for<'a> Perform<Read<'a>>`.

Outcomes are types a test backend can build: seam's own `io::Error` (a
kind and a `&'static str`, carrying the original `std::io::Error` when
there is one), `time::Instant` (a duration since a backend-chosen epoch),
`fs::FileInfo`, `process::Output`. std's equivalents cannot be constructed
without the OS.

## Capabilities

```rust,ignore
#[diagnostic::on_unimplemented(message = "`{Self}` does not have the capability to perform `{O}`", ..)]
pub trait Perform<O: Operation> {
    fn perform(&mut self, op: O) -> O::Outcome;
}

pub trait Inspect<Q: Query> {
    fn inspect(&self, query: Q) -> Q::Outcome;
}
```

A capability is any value implementing these. A handler is an impl:
`impl Perform<Sleep> for Std`. Dispatch is static: a core generic over
`C: Perform<Sleep>` is monomorphized per backend, and `c.perform(op)` is a
direct call.

### Lifts, and why they are opt-in

Blanket impls make every `Inspect` a `Perform` (the way `&mut T` reborrows
as `&T`), every sync capability an async one, and every thread-safe sync
capability a `Send` async one:

```rust,ignore
impl<T: Inspect<Q> + Capability + ?Sized, Q: Query> Perform<Q> for T { .. }
impl<T: Perform<O> + Capability + ?Sized, O: Operation> AsyncPerform<O> for T { .. }
// ... and the Inspect and Send forms
```

They require `Capability`, a marker trait. Without it, the lifts would
collide with types that implement the async forms themselves. `Root` is the
case that matters: it forwards each form to its parts, so a lift that also
claimed `AsyncPerform<Sleep>` for the root would conflict. Every backend
type carries the marker, and a handler of your own adds one line,
`impl Capability for MyType {}`.

### Three forms

Rust cannot be generic over sync and async, nor over whether a future is
`Send`. So the capability traits come in three forms (`Perform`,
`AsyncPerform`, `SendPerform`, and the same for `Inspect`), and so does
every helper (`write_all`, `write_all_async`, `write_all_send`). A
crate-private macro, `three_ways!`, writes all three from one body, in
which `run!`, `ask!`, `acquire!`, `lend!` and `call!` stand for performing
an operation, running a query, opening or lending a scope, and calling
another helper, in whichever form is being generated.

`block_on` lowers async to sync for cores whose capabilities are all sync:
their futures are ready on the first poll. With std, a pending future parks
the thread; without, it spins.

## Primitives and helpers

Backends implement **primitives**, the smallest effects: `Read` and
`Write` on a handle, `Open`, `View` and `ReadAt`, `FillRandom`, ...
Everything that can be built from primitives is a plain function in
seam-core beside them (`read_exact`, `read_file`, `write_file`,
`create_dir_all`, `print`, `BufReader`), so a backend implements less and
every helper works on every backend and every handle.

Four effects that could be built from others stay primitives, because the
built version would be worse: `CreateAtomic` (its cleanup must run in
`Drop`, where a core cannot perform effects), recursive `RemoveDir` (a walk
that follows a symlink deletes outside the tree), `CopyFile` (platforms copy
faster, and copy permissions) and `RunCommand` (collecting both pipes needs
reading two at once).

## Scopes

An acquisition names what a scope produces:

```rust,ignore
pub trait Acquisition {
    type Resource;
    type Error;
}

pub struct Opening<A>(pub A, pub Token);                                   // produces the resource
pub struct Closing<A: Acquisition>(pub A::Resource, pub PhantomData<fn() -> A>); // commits it
```

Opening and closing are ordinary operations, so a scope is just two
effects, and everything built for operations (the three forms, the lifts,
roots, wrappers) works for scopes with no second set of traits.
`Provide<A>` and `Lend<A>` are names for "performs (inspects) both", with
blanket impls that carry the scope methods:

```rust,ignore
c.acquire(op, |c, resource| { .. })   // Perform<Opening<A>> + Perform<Closing<A>>
c.lend(op, |c, resource| { .. })      // Inspect<..> for both: from &C
```

`acquire` performs `Opening`, lends the resource to the body, and on `Ok`
performs `Closing`, returning its error through `E: From<A::Error>`. On
`Err`, a panic or a dropped future, the resource is dropped instead, and
every provider makes dropping mean rolling back. That is what makes scopes
cancellation-safe: a future can be dropped at any `.await`, even halfway
through opening or closing, and the result is either the commit or the
rollback. Tokio's atomic file does its create and rename inside blocking
tasks that own the cleanup guard, and a test cancels an atomic write after
every possible number of polls.

`Token` can only be created inside seam-core, so a core cannot open a
resource outside a scope; a provider that opens by asking another passes
its token on.

**`Send` futures.** A scope method defined as a trait default would hide
its future's type behind the bound, so generic code could not prove the
future `Send`. The methods live in each trait's single blanket impl, and the
`Send` forms' methods (`acquire_send`, `lend_send`) on `SendScope`, a trait
implemented for every type and named in no bound, so calls resolve to the
impl and the future's auto traits show through.

## Roots

A `Root<A>` wraps a struct `A` of parts. Each operation names its effect
by a key (`impl Effect for Sleep { type Key = effects::Sleep; }`), and the
root implements each capability trait once, generically, by finding the
part that serves the key:

```rust,ignore
impl<A, O> Perform<O> for Root<A>
where
    O: Operation + Effect,
    O::Key: Pick<A>,                              // which field serves this key
    <O::Key as Pick<A>>::Part: Perform<O>,        // and that it can
{
    fn perform(&mut self, op: O) -> O::Outcome {
        <O::Key as Pick<A>>::pick_mut(&mut self.0).perform(op)
    }
}
// ... and Inspect, the async and the Send forms
```

`Pick<A>` is implemented on a key, per root, and `root!` writes those
impls. `root![Std<fx::Var>, Sim<fx::Now>::new()]` expands to about:

```rust,ignore
{
    #[derive(Clone, Copy, Debug, Default)]
    struct Parts<P0, P1> { __seam0: P0, __seam1: P1 }

    impl<P0, P1> Pick<Parts<P0, P1>> for fx::Var {
        type Part = P0;
        fn pick(parts: &Parts<P0, P1>) -> &P0 { &parts.__seam0 }
        fn pick_mut(parts: &mut Parts<P0, P1>) -> &mut P0 { &mut parts.__seam0 }
    }
    impl<P0, P1> Pick<Parts<P0, P1>> for fx::Now { /* the same, for __seam1 */ }

    Root::new(Parts { __seam0: Std, __seam1: Sim::new() })
}
```

The struct is generic over its parts, so their types are inferred from the
values (`FileStore<Get, Put>::new(Std, path)` is a `FileStore<Std>`) and
the derives apply whenever the parts allow. A key listed twice is two
`Pick` impls for one type, a plain "conflicting implementations" error; a
missing one is `Pick`'s own diagnostic, "the root `Parts<..>` has no part
for `fx::Open<WriteOnly>`".

Why the keys are written out in brackets, rather than looked up from the
part's type:

- Routing "to whichever part implements this operation" is not expressible:
  two parts might, and coherence rejects the rule outright.
- Routing by `<H as Effect>::Key` works only for a handler defined in the
  root's own crate: Rust's overlap check does not normalize a projection
  through another crate's impls, so two upstream handlers would "conflict".
- Type-level lists (an HList, `Select<A, B, ..>`) need either key
  inequality, which cannot be stated, or an index parameter that only a
  method call can infer, and the root forwards through trait impls.

## Backends

`Std` and `Tokio` are zero-sized and `Copy`; a root of them is too. They
hand out handles as newtypes (`File<M>`, `TcpStream`, ...) that expose no
std or Tokio I/O traits and no way back to the inner value, so every effect
on a handle also goes through `Perform`.

A file's mode is a type (`File<ReadOnly>`), and `Read` is implemented only
for readable modes, `Write`, `SetLen` and the syncs only for writable ones.

Changing the environment while another thread reads it is undefined
behaviour on most platforms, so `Std` only reads it.
`seam_std::env::WritableEnv` changes it; it is made by an `unsafe fn new()`
that carries `set_var`'s contract, and is `!Send`. It is the only `unsafe`
in seam.

## The simulation

### One shared world

`Sim` is a handler like `Std`: it implements `Perform` and `Inspect` for
every effect it simulates, and nothing about it is special to seam's
traits. Its state is split by area (a clock, random numbers, the
environment, files, the network, stdio, processes, signals), each area a
struct that reads and writes its own world-file block, all behind one
`Arc<Mutex<_>>`, and a `Sim` is a handle to it. Handles the `Sim` hands out
(an open file, a socket) hold the same `Arc`, which is why they can outlive
the borrow that made them, and why `Sim` is `Clone` with every clone the
same world. Without std there is no mutex, and the `Sim` holds its clock
and random generator itself.

That mutex, the runtimes' bookkeeping and the simulated channels and cells
are seam-test's only shared state. The crate is linted with seam's own
`clippy.toml`, which bans std's effect APIs and shared-state types, with an
allow on each of those modules alone.

### The deterministic runtime

`seam::test::rt::Runtime` owns a `Sim` and runs async code on a
single-threaded executor. Operations that never wait delegate to the
`Sim`; those that would wait are native async there:

- The in-memory network wakes exactly the waiters of the pipe, listener or
  socket that changed.
- `Sleep` registers a timer. When no task is ready, the executor advances
  the clock to the earliest deadline and wakes the timers due then.
- The next task is drawn from the ready set by a SplitMix64 seeded per
  runtime, separate from the `Sim`'s own random numbers.
- No task ready and no timer set is a deadlock, reported rather than hung.
- A timer that loses a race against the operation it times out is removed,
  so it cannot move the clock later and delay a deadlock report.

### Deterministic threads

`seam::test::threads::Threads` runs each simulated thread on a real OS
thread, so a core's closures and stack work as written, but only one runs
at a time, handing over through a condvar. Every operation on a
`ThreadSim`, its channels, cells, sockets and thread handles first hands
control to the scheduler, which picks the next runnable thread from its
seed. Cores share state only through those operations, so switching at
each reaches every interleaving that can change what a core observes.

A blocking operation polls the world with a waker that marks its thread
runnable, and leaves the runnable set until woken. When the main closure
returns, or on a deadlock, every other thread is unwound with a private
payload.

### World files

The parser handles the subset of KDL v2 world files use, with positions on
every node and value, and a writer prints nodes back. `World` is the one
trait the simulation adds, and it is not about handling effects: it
describes a handler's state, with the blocks it reads, `load`, `to_world`
and `check`, which by default is a subset test against `to_world`. `load_world::<K, _>` and `check_world::<K, _>` take a
tuple of up to 32 keys (the impls are written by a macro); for each block,
they ask each key's part for its `BLOCKS`, find the one that reads it, and
compare parts by address and type so a part reached through several keys
is used once.

`Sim`'s own `World` impl sends each block to the area that reads it, and
writes out every area in turn. A fake of a program's own implements the
same trait, so extending the simulation is as capable for other crates as
for seam itself.

## Keeping cores honest

The type system cannot stop a core calling `std::fs::read`.
`templates/clippy.toml` bans the std and Tokio effect APIs and shared-state
types. seam-test, which must perform no real effects, is linted with it;
a program copies it into each crate that should stay sans-effect.

## Support policy

- **Rust version:** 1.99 or later (`rust-version` in the workspace), on
  stable: the latest stable release, raised as new ones come out.
- **Semver:** each release is checked with `cargo-semver-checks` against
  the last one published. It does not see changes to what a `macro_rules!`
  macro accepts, or most return-type changes, so those are reviewed by
  hand.
- **`unsafe`:** forbidden in every crate but seam-std, which allows it on
  `WritableEnv` alone.
