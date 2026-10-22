# seam

Sans-effect programming for Rust.

Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every** effect:
I/O, time, randomness, shared state. The core of your program is written
against capability bounds and can be unit tested, or run, with no effects at
all. Real implementations or test fakes are plugged in at the edge, the seam.

A function's bounds are an audit of what it can do:

```rust
use seam::Perform;
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
| [`seam-core`](crates/seam-core) | The traits (`Operation`, `Query`, `Inspect`, `Perform`, `Finalize`, `Scoped`, async variants, `block_on`) and the operation types. `no_std`. |
| [`seam-std`](crates/seam-std) | The std backend: a `Std` root capability and newtype handles (`File`, `TcpStream`, ...) that expose no std I/O traits. |
| [`seam-tokio`](crates/seam-tokio) | The Tokio backend: a `Tokio` root capability implementing the async and `Send` capability traits. |
| [`seam`](crates/seam) | Re-exports everything, behind feature flags. `no_std` by default. |

## Features

`seam` has no default features: you get the traits and `Push`, with no std.

| Feature | Gives you |
|---|---|
| `time`, `random` | `Sleep`, `RandomU64`, `FillRandom`. No std needed. |
| `fs`, `net`, `process`, `env`, `io-std` | Those operation types. Pulls in std and the std backend. |
| `std` | The std backend, `seam::stdlib`. |
| `tokio` | The Tokio backend, `seam::tokio`. |
| `signal`, `rt`, `rt-multi-thread`, `test-util` | Tokio-only, named as in Tokio. |
| `full` | Everything. |

A backend implements exactly the capability groups that are enabled, so
`features = ["tokio", "net", "time"]` gives you Tokio-backed networking and
clocks and nothing else.

## Keeping the core honest

The newtype handles stop effects leaking out of the backends, but nothing in
the type system stops a core crate from calling `std::fs::read` directly.
[`templates/clippy.toml`](templates/clippy.toml) bans the std and Tokio
effect APIs (and shared-state types); copy it into each crate that should stay
sans-effect.

## Design

See [DESIGN.md](DESIGN.md).
