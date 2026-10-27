# What seam is

## The problem

Most code that is hard to test is hard because of what it touches: the
clock, the filesystem, the network, randomness, the environment, other
threads. Sans-IO libraries solve this for protocols by keeping I/O out of
the protocol logic. seam does the same for **every** effect, so a whole
program's logic (its *core*) is written without touching the outside world
at all, and the effects are supplied at the edge of the program: the seam.

```rust
# #[cfg(all(feature = "test", feature = "time", feature = "random"))] {
use core::time::Duration;
use seam::prelude::*;
use seam::ops::random::{FillRandom, random_u64};
use seam::ops::time::Sleep;

/// Can sleep and draw random numbers. Nothing else: no files, no network,
/// no environment. The bounds say so, and the compiler holds it to them.
fn backoff<C: for<'a> Perform<FillRandom<'a>> + Perform<Sleep>>(c: &mut C, attempt: u32) {
    let jitter = random_u64(c) % 100;
    c.perform(Sleep(Duration::from_millis(100 * u64::from(attempt) + jitter)));
}

// In a test, on a simulated world: no real sleeping, the same numbers
// every run.
let mut sim = seam::test::Sim::with_seed(1);
backoff(&mut sim, 3);
assert!(sim.inspect(seam::ops::time::Now).since_epoch() >= Duration::from_millis(300));
# }
```

The same `backoff` runs in production on `seam::stdlib::Std` (real time,
real randomness) or `seam::tokio::Tokio`, unchanged.

Three things follow:

- **Testable cores.** A core runs against a simulated world
  (`seam::test::Sim`, or a `world!` of just the effects it needs) with a
  virtual clock, seeded randomness, an in-memory filesystem and network,
  scripted commands and captured output. Effects of the program's own are
  simulated the same way, on their own keys. Tests are fast and
  deterministic, and concurrency bugs are reproduced from a seed.
- **Signatures are an audit.** A function's bounds list everything it can
  do. A core bounded on `Open<'a, ReadOnly, F>` cannot write a file, and
  one with no network bounds cannot reach the network.
- **Backends are interchangeable.** The same core runs on std, on Tokio,
  in the simulation, or on a mix of them through a [`Root`](crate::Root).

## seam as an effect system

In programming-language terms, seam is a **type-and-effect system with
tail-resumptive handlers**, encoded in Rust's traits:

| Effect-system idea | In seam |
|---|---|
| an operation (a request to the outside world) | a type implementing [`Operation`](crate::Operation), naming its result: `Sleep`, `Open`, a `Get` of your own |
| an effect signature (what a computation may do) | trait bounds: `C: Perform<Sleep> + Inspect<Now>` |
| performing an operation | `c.perform(Sleep(d))`, `c.inspect(Now)` |
| a handler | an `impl Perform<Sleep> for Std`, or for a fake |
| a simulated handler | `impl Simulate<Sleep> for effects::Sleep`: defined on the effect's own key, against a state of a simulated world |
| installing handlers | passing a capability value: `core(&mut Std)`, `core(&mut sim)`, `core(&mut root)` |
| composing handlers, one per effect | a [`Root`](crate::Root): each part serves the effects listed for it |
| a handler built on other effects | a part that holds another capability and performs through it |
| effect polymorphism | generics: `fn core<C: Perform<Sleep>>(c: &mut C)` works for any handler |
| scoped operations (bracket, `with_file`) | an acquisition: [`Opening`](crate::Opening) and [`Closing`](crate::Closing) operations, run as a scope by [`Provide`](crate::Provide) and [`Lend`](crate::Lend) |

Read-only operations are [`Query`](crate::Query)s, run through
[`Inspect`](crate::Inspect) from `&self`; everything else goes through
[`Perform`](crate::Perform) from `&mut self`. Every `Inspect` is also a
`Perform`, so a stronger handler is accepted where a weaker one is asked
for.

### Tail-resumptive: what that means, and what it rules out

In a full algebraic-effect system (Koka, Eff, OCaml 5, or the Rust crate
`effing-mad` on nightly coroutines), performing an operation *suspends* the
computation and hands its continuation, "the rest of the program", to the
handler. The handler may resume it, resume it later, never resume it, or
run code after it finishes.

A seam handler is an ordinary function: it computes the answer and returns
it, and the computation carries on. That is the *tail-resumptive* case,
where the handler always resumes exactly once, immediately. It covers I/O,
time, randomness, the environment, shared state and almost every effect a
real program has, and it compiles to a direct, statically dispatched call.

What it does not cover are **control effects**, which need the
continuation itself:

- aborting from a handler (exceptions as an effect). In seam, failure is a
  `Result` in the operation's outcome and the core propagates it with `?`;
- generators and `yield` as an effect;
- nondeterminism and backtracking (resuming more than once);
- a handler that wraps the rest of the computation (timing it, rolling it
  back from inside the handler).

seam leaves those to Rust's own control flow: `Result`, early return, and
`async`. Async seam gets part of the way back: a future is a one-shot
continuation, so the simulated runtime can suspend a task at an operation,
resume it when another task makes progress possible, and drop it to cancel.
But that machinery is the runtime's, not something an ordinary handler can
do.

### Laws

Algebraic effects come with equations: `put s; get` returns `s`. seam has
no way to state those in the type system. They are checked the way any Rust
property is, with tests, typically run against several handlers (`Std` in a
temporary directory and `Sim`) to confirm they agree. Where several
operations must behave as one (read, change and write back a cell without
another thread in between), seam makes that a single operation (`Update`),
so the atomicity is part of the signature rather than a law.

## What seam is not

- **Not a runtime.** seam does not schedule anything in production; it
  forwards to std or Tokio. Its runtime and thread scheduler are only for
  tests.
- **Not a sandbox.** Nothing in Rust's type system stops a core from
  calling `std::fs::read` directly. The repository's `templates/clippy.toml`
  bans the std and Tokio effect APIs, and copying it into a core's crate is
  what keeps the core honest.
- **Not dynamic.** Handlers are chosen by type, at compile time. There is
  no handler stack to push onto at run time, and no allocation or dynamic
  dispatch on the way to a handler.

## Compared to other approaches

| Approach | Effects in signatures | Swap handlers | Control effects | Cost |
|---|---|---|---|---|
| seam | trait bounds | by type, statically | no (use `Result`, `async`) | longer bounds |
| `effing-mad` | effect sets on coroutines | by handler closures | yes (one-shot) | nightly Rust; every effectful function is a coroutine |
| Koka, Eff, OCaml 5 | inferred effect rows (OCaml 5: untyped) | yes | yes | a language of its own |
| Haskell `mtl`, `polysemy`, `effectful` | type classes or effect rows | yes | partly to fully | a language of its own |
| sans-IO protocol crates | none (the protocol returns events) | the caller drives I/O | no | covers I/O only, per protocol |
| traits per dependency (`trait Clock`, `trait Store`) | yes, per project | yes | no | each project writes its own, with no shared backends or simulation |
| `cap-std` | capability values for files and sockets | no | no | security, not testing: real effects only |

The closest everyday comparison is the last-but-one row: injecting a
`Clock` trait so a test can pass a fake one. seam is that idea made
uniform: one set of operation types for every effect, backends that
implement all of them, a simulation that implements all of them
deterministically, and a way to combine handlers per effect.

## What it costs

- **Bounds get long.** A core that reads files, prints and sleeps names
  each, with a type parameter per handle type (`F` for files, `O` for
  stdout). Grouping them into one trait with a blanket impl keeps
  signatures short (the `backup` example does this).
- **Three forms.** Rust cannot be generic over sync and async, or over
  whether a future is `Send`, so traits and helpers come in three forms
  (`write_all`, `write_all_async`, `write_all_send`).
- **Discipline at the edge.** The clippy template is a convention, not a
  guarantee.

In return, a core is plain Rust, on stable, with no allocation or dynamic
dispatch added, that runs on any backend and is tested on a simulated
world.
