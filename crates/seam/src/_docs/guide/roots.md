# Roots

A core takes one capability. When that capability must come from several
places (environment from std and time from Tokio, a simulated clock with a
real filesystem, a store of the program's own), combine them in a
[`Root`](crate::Root).

## Building a root

[`root!`](crate::root!) takes a list of *parts*. Each part names the effects
it serves, by their keys from [`effects`](crate::effects), in brackets
after its type's name:

```rust
# #[cfg(all(feature = "std", feature = "test", feature = "env", feature = "time"))] {
use seam::effects as fx;
use seam::prelude::*;
use seam::ops::env::Var;
use seam::ops::time::Now;
use seam::stdlib::Std;
use seam::test::Sim;

let root = seam::root![
    Std<fx::Var>,          // the real environment
    Sim<fx::Now>::new(),   // a simulated clock
];
let _home = root.inspect(Var::new("HOME"));
assert_eq!(root.inspect(Now).since_epoch().as_secs(), 0);
# }
```

The root is a capability like any other: pass `&mut root` (or `&root`) to
a core. It forwards each operation to the part listed for its effect, and
grants the core exactly the listed effects, whatever else a part could do.
Above, `Std` could also open files, but the root does not let a core do so.

## The four forms

A part is written the way Rust builds a value of its type, and in no other
way:

| Form | Example | For |
|---|---|---|
| unit struct | `Std<fx::Var>` | a type with no fields |
| tuple struct | `Pool<fx::Query>(4)` | a tuple struct, built from its fields |
| struct literal | `Pool<fx::Query> { size: 4 }` | a struct, built from named fields |
| constructor, with or without `?` | `Sim<fx::Now>::from_world(W)?` | any associated function returning the type |

```rust
# #[cfg(all(feature = "std", feature = "test", feature = "env", feature = "time"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use seam::effects as fx;
use seam::prelude::*;
use seam::ops::time::{Instant, Now};
use seam::stdlib::Std;
use seam::test::Sim;

/// A clock stopped at one instant.
struct Frozen {
    at: Instant,
}
impl Capability for Frozen {}
impl Inspect<Now> for Frozen {
    fn inspect(&self, _: Now) -> Instant {
        self.at
    }
}

let root = seam::root![
    Std<fx::Var>,                                // a unit struct
    Sim<fx::Sleep>::from_world("seed 1")?,       // a constructor, with `?`
    Frozen<fx::Now> { at: Instant::EPOCH },      // a struct literal
];
assert_eq!(root.inspect(Now), Instant::EPOCH);
# Ok(())
# }
# #[cfg(not(all(feature = "std", feature = "test", feature = "env", feature = "time")))]
# fn main() {}
```

A generic type's parameters are inferred from the value, as in
`FileStore<Get, Put>::new(Std, path)`, which builds a `FileStore<Std>`.
Anything else is a compile error that lists the four forms.

## Keys

A key is a marker type naming one effect. seam's are all in
[`effects`](crate::effects), one per operation and named after it:
`effects::Var`, `effects::Now`, `effects::TcpBind`, `effects::View`. Two
details:

- `Open` is keyed by its mode, so reading and writing a file are separate
  grants: `effects::Open<ReadOnly>`, `effects::Open<WriteOnly>`.
- A scope routes by its acquisition: listing `effects::View` serves both
  opening and closing a `View`. A timed operation, `Timeout<O>`, routes as
  `O` does.
- Operations on handles (`Read`, `Write`, `RecvMsg`, `Join`) have no key:
  they go to the handle itself, which the part handed out.

A program's own effect names its key with [`Effect`](crate::Effect),
usually itself:

```rust
use seam::{Effect, Operation};

pub struct Get(pub String);
impl Operation for Get {
    type Outcome = Option<String>;
}
impl Effect for Get {
    type Key = Get;
}
```

## One part, several effects

A part can serve any number of effects, and a whole backend is one part.
Each effect is served by exactly one part:

```rust
# #[cfg(all(feature = "std", feature = "fs", feature = "env", feature = "io-std"))] {
use seam::effects as fx;
use seam::stdlib::Std;

let _root = seam::root![Std<fx::Args, fx::View, fx::OpenStdout>];
# }
```

Listing one effect under two parts is a compile error ("conflicting
implementations"), and so is a core asking for an effect the root does not
list:

```text
error[E0277]: the root `Parts<..>` has no part for `fx::Open<WriteOnly>`
```

## Reaching a part

`root.part::<K>()` returns the part serving the key `K`, and `part_mut`
lends it mutably: for setting a simulated part up before a run, or checking
it afterwards.

```rust
# #[cfg(all(feature = "test", feature = "env", feature = "time"))] {
use seam::effects as fx;
use seam::prelude::*;
use seam::ops::env::Var;
use seam::test::Sim;

let mut root = seam::root![Sim<fx::Var, fx::Now>::new()];
root.part_mut::<fx::Var>().set_var("USER", "ada");
assert!(root.inspect(Var::new("USER")).is_some());
# }
```

## Parts built on other parts

A part can hold another capability and perform through it: a store that
keeps its data in files, a cache in front of a network client. Give it the
capability it needs when building it. A `Sim` is a handle to a shared world
(every clone is the same world), so a simulated part built over a clone
sees what the root's own `Sim` part sees:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::effects as fx;
use seam::prelude::*;
use seam::{Effect, Operation};
use seam::ops::fs::{read_to_string, write_file};
use seam::test::Sim;

pub struct Put(pub String, pub String);
impl Operation for Put {
    type Outcome = seam::io::Result<()>;
}
impl Effect for Put {
    type Key = Put;
}

/// Keeps each value in a file under `/kv`, through the `Sim` it holds.
struct Disk(Sim);
impl Capability for Disk {}
impl Perform<Put> for Disk {
    fn perform(&mut self, op: Put) -> seam::io::Result<()> {
        write_file(&mut self.0, &format!("/kv/{}", op.0), &op.1)
    }
}

let mut sim = Sim::new();
sim.add_dir("/kv");
let mut root = seam::root![
    Sim<fx::View>::clone(&sim),   // a constructor: `Clone::clone`
    Disk<Put>(sim.clone()),       // a tuple struct
];
root.perform(Put("k".into(), "v".into())).unwrap();
assert_eq!(read_to_string(&root, "/kv/k").unwrap(), "v");
# }
```

In production, the same part would hold `Std`, or a root of its own.

[`seam::jiff`](crate::jiff)'s `TzDb` is a part of this kind: it answers
time zone lookups by reading the tz database through the filesystem of the
capability it holds, so `TzDb::new(Std)` reads the real database and
`TzDb::new(sim.clone())` reads whatever files the test put in the `Sim`.

## Naming the root's type

`root![..]` evaluates to a `Root` of a generated struct, whose type cannot
be written. To store a root in a field, or name it in a signature, define
the struct with the named form, and wrap a value of it in
[`Root::new`](crate::Root::new):

```rust
# #[cfg(all(feature = "std", feature = "env", feature = "time"))] {
use seam::effects as fx;
use seam::Root;
use seam::stdlib::Std;

seam::root! {
    #[derive(Clone, Copy, Default)]
    pub struct App {
        pub env: Std<fx::Var>,
        pub clock: Std<fx::Now, fx::Sleep>,
    }
}

struct Service {
    root: Root<App>,
}
let _service = Service { root: Root::new(App::default()) };
# }
```

The generated struct of `root![..]` derives `Clone`, `Copy`, `Debug` and
`Default` whenever its parts do, so a root of `Std` and `Tokio` parts is
`Copy`, and a root of clonable parts can be cloned into a task or thread.

## Lifts and `Capability`

seam's blanket impls make every `Inspect` a `Perform` and every sync
operation usable from async code. They apply to types that opt in with
[`Capability`](crate::Capability), which every backend type does. A type of
your own that implements operations adds one line:

```rust
# struct Memory;
impl seam::Capability for Memory {}
```

`Root` itself does not, because it forwards each form (sync, async,
`Send`) to its parts directly; that is what lets a sync `Std` part and an
async `Tokio` part sit in one root.
