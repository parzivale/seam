# Getting started

This guide writes a small core, runs it on the real world, and tests it on
a simulated one.

## Dependencies

Pick the capability groups the core uses (`env`, `io-std`, ...), and
separately the backends: `std` for the program (with `tokio` or `smol`
beside it if the core is async), `test` for its tests. No capability group turns a backend on, so a library holding a core
depends on the groups alone.

```toml
[dependencies]
seam = { package = "seam-rs", version = "0.6", features = ["env", "io-std", "std"] }

[dev-dependencies]
seam = { package = "seam-rs", version = "0.6", features = ["test"] }
```

## A core

A core is a generic function over a capability `C`, with a bound for each
operation it needs:

```rust
# #[cfg(all(feature = "test", feature = "env", feature = "io-std"))] {
use seam::io;
use seam::prelude::*;
use seam::ops::env::Var;
use seam::ops::io::Write;
use seam::ops::stdio::{OpenStdout, print};

/// Greets `$USER`. It can read environment variables and print, and
/// nothing else.
fn greet<C, O>(c: &mut C) -> io::Result<()>
where
    C: for<'a> Inspect<Var<'a>> + Provide<OpenStdout<O>>,
    O: for<'a> Perform<Write<'a>>,
{
    let user = c
        .inspect(Var::new("USER"))
        .map_or_else(|| "stranger".to_owned(), |u| u.to_string_lossy().into_owned());
    print(c, format!("hello, {user}\n").as_bytes())
}
# let mut sim = seam::test::Sim::new();
# sim.set_var("USER", "ada");
# greet(&mut sim).unwrap();
# assert_eq!(sim.stdout(), b"hello, ada\n");
# }
```

The bounds read as:

- `for<'a> Inspect<Var<'a>>`: it may read a variable. `Var` borrows the
  name, so the bound covers every lifetime (`for<'a>`). `Inspect` is for
  queries, which change nothing, so it needs only `&self`.
- `Provide<OpenStdout<O>>`: it may open standard output, as a handle of
  the backend's type `O`, for the length of a scope. `print` opens it,
  writes and closes it.
- `O: for<'a> Perform<Write<'a>>`: that handle can be written to.

`O` is never named by the caller; Rust infers it from the backend.

Operations live in `seam::ops`, one module per capability group. Helpers
built from them (`print`, `read_to_string`, `write_file`, `random_u64`, ...)
sit beside them and work on every backend.

## Running it

In `main`, pass a real backend. `Std` is a zero-sized value that performs
each operation with std:

```rust,ignore
fn main() -> seam::io::Result<()> {
    greet(&mut seam::stdlib::Std)
}
```

For an async program, write the core with the async traits and pass
`seam::tokio::Tokio` or `seam::smol::Smol` (see [the async guide](crate::_docs::guide::async_and_concurrency)).
For a program that takes different effects from different backends, or
has effects of its own, pass a [root](crate::_docs::guide::roots).

## Testing it

In a test, pass a `Sim`: a simulated world. Set it up with its methods, run
the core, and check what it left behind:

```rust
# #[cfg(all(feature = "test", feature = "env", feature = "io-std"))] {
# use seam::io;
# use seam::prelude::*;
# use seam::ops::env::Var;
# use seam::ops::io::Write;
# use seam::ops::stdio::{OpenStdout, print};
# fn greet<C, O>(c: &mut C) -> io::Result<()>
# where
#     C: for<'a> Inspect<Var<'a>> + Provide<OpenStdout<O>>,
#     O: for<'a> Perform<Write<'a>>,
# {
#     let user = c
#         .inspect(Var::new("USER"))
#         .map_or_else(|| "stranger".to_owned(), |u| u.to_string_lossy().into_owned());
#     print(c, format!("hello, {user}\n").as_bytes())
# }
use seam::test::Sim;

let mut sim = Sim::new();
sim.set_var("USER", "ada");
greet(&mut sim).unwrap();
assert_eq!(sim.stdout(), b"hello, ada\n");

// Nothing set: the default.
let sim = Sim::new().run(|sim| greet(sim).unwrap());
assert_eq!(sim.stdout(), b"hello, stranger\n");
# }
```

The same setup can be written as a [world file](crate::_docs::world_files):

```rust
# #[cfg(all(feature = "test", feature = "env", feature = "io-std"))] {
# use seam::io;
# use seam::prelude::*;
# use seam::ops::env::Var;
# use seam::ops::io::Write;
# use seam::ops::stdio::{OpenStdout, print};
# fn greet<C, O>(c: &mut C) -> io::Result<()>
# where
#     C: for<'a> Inspect<Var<'a>> + Provide<OpenStdout<O>>,
#     O: for<'a> Perform<Write<'a>>,
# {
#     let user = c
#         .inspect(Var::new("USER"))
#         .map_or_else(|| "stranger".to_owned(), |u| u.to_string_lossy().into_owned());
#     print(c, format!("hello, {user}\n").as_bytes())
# }
let mut sim = seam::test::Sim::from_world("env { var USER ada }").unwrap();
greet(&mut sim).unwrap();
sim.assert_world(r#"stdio { stdout "hello, ada\n" }"#);
# }
```

## When a bound is missing

A core that performs an operation its bounds do not list fails to compile,
with a message naming both:

```text
error[E0277]: `C` does not have the capability to perform `Sleep`
```

The same message appears when a backend does not implement an operation,
or when a feature that provides it is off.

## Keeping the core honest

Nothing in the type system stops a core from calling `std::env::var`
directly. Copy the repository's `templates/clippy.toml` into the core's
crate: it bans the std and Tokio effect APIs, so clippy flags any effect
that does not go through a capability.

## Next

- [Roots](crate::_docs::guide::roots): several backends and your own effects in one
  capability.
- [Resources](crate::_docs::guide::resources): files, sockets and tasks.
- [Testing](crate::_docs::guide::testing): the simulated world in detail.
