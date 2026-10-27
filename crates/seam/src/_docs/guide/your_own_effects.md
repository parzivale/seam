# Effects of your own

A program's own effects (bind mounts, a key-value store, a payment API, a
queue) are operations like seam's, with a real handler for production and
a simulated one for tests. This guide builds one end to end: bind mounts,
applied all or nothing.

## 1. The operations

Each operation is a type. [`Operation`](crate::Operation) names its
result; [`Effect`](crate::Effect) names its key, the marker a root routes
it by, which for an effect of your own is usually the type itself.
Read-only operations are also [`Query`](crate::Query)s, which lets them be
run through `Inspect`, from `&self`.

```rust
use std::path::PathBuf;
use seam::{Effect, Operation, io};

/// Bind `source` onto `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindMount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub read_only: bool,
}
impl Operation for BindMount {
    type Outcome = io::Result<()>;
}
impl Effect for BindMount {
    type Key = BindMount;
}

/// Unmount whatever is on `target`.
pub struct Unmount {
    pub target: PathBuf,
}
impl Operation for Unmount {
    type Outcome = io::Result<()>;
}
impl Effect for Unmount {
    type Key = Unmount;
}
```

## 2. Code that uses them

A core asks for them like any capability. This one applies a list of
mounts and, if one fails, undoes the ones it made:

```rust
# use std::path::PathBuf;
# use seam::{Effect, Operation, io};
# #[derive(Debug, Clone, PartialEq, Eq)]
# pub struct BindMount { pub source: PathBuf, pub target: PathBuf, pub read_only: bool }
# impl Operation for BindMount { type Outcome = io::Result<()>; }
# pub struct Unmount { pub target: PathBuf }
# impl Operation for Unmount { type Outcome = io::Result<()>; }
use seam::Perform;

pub fn bind_all<C>(c: &mut C, mounts: &[BindMount]) -> io::Result<()>
where
    C: Perform<BindMount> + Perform<Unmount>,
{
    for (done, mount) in mounts.iter().enumerate() {
        if let Err(e) = c.perform(mount.clone()) {
            for made in mounts[..done].iter().rev() {
                let _ = c.perform(Unmount { target: made.target.clone() });
            }
            return Err(e);
        }
    }
    Ok(())
}
```

## 3. The real handler

A handler is a type that implements the operations, and
[`Capability`](crate::Capability) to opt in to seam's lifts (so it also
works from async code, and anything it can `Inspect` it can also
`Perform`). The real one calls the OS; with `rustix`:

```rust,ignore
pub struct Linux;
impl Capability for Linux {}

impl Perform<BindMount> for Linux {
    fn perform(&mut self, op: BindMount) -> io::Result<()> {
        rustix::mount::mount_bind(&op.source, &op.target).map_err(std::io::Error::from)?;
        // ... remount read-only if asked
        Ok(())
    }
}
impl Perform<Unmount> for Linux { /* rustix::mount::unmount */ }
```

At the edge, put it in a root next to the backend:

```rust,ignore
let mut root = seam::root![
    Std<fx::Args, fx::View, fx::OpenStdout>,
    Linux<BindMount, Unmount>,
];
run(&mut root)?;
```

This is the one place the real effect lives, so it is the one place that
is not unit tested; keep it thin.

## 4. The simulated handler

A simulated effect is defined on its key, with two sister traits, and
works on a *state* of the simulated world:

- `Simulated` names the state the effect works on. Effects that share a
  state (`BindMount` and `Unmount` both work on the mount table) name the
  same one, and a world holds one value of each state type, so they share
  it.
- `Simulate<O>` runs an operation against the world's states.

The state is an ordinary struct, implementing `SimState`: its `Default`
is a fresh world, `load` sets it up from its world-file block, and
`to_world` writes it out.

```rust
# #[cfg(all(feature = "test", feature = "std"))] {
# use std::path::PathBuf;
# use seam::{Effect, Operation, Perform, io};
# #[derive(Debug, Clone, PartialEq, Eq)]
# pub struct BindMount { pub source: PathBuf, pub target: PathBuf, pub read_only: bool }
# impl Operation for BindMount { type Outcome = io::Result<()>; }
# impl Effect for BindMount { type Key = BindMount; }
# pub struct Unmount { pub target: PathBuf }
# impl Operation for Unmount { type Outcome = io::Result<()>; }
# impl Effect for Unmount { type Key = Unmount; }
# pub fn bind_all<C>(c: &mut C, mounts: &[BindMount]) -> io::Result<()>
# where C: Perform<BindMount> + Perform<Unmount> {
#     for (done, mount) in mounts.iter().enumerate() {
#         if let Err(e) = c.perform(mount.clone()) {
#             for made in mounts[..done].iter().rev() {
#                 let _ = c.perform(Unmount { target: made.target.clone() });
#             }
#             return Err(e);
#         }
#     }
#     Ok(())
# }
use seam::test::sim::{SimState, Simulate, Simulated, States};
use seam::test::world_file::{self, Node};

/// The mount table: what is mounted, and a target to refuse, for testing
/// failures.
#[derive(Debug, Default)]
pub struct Mounts {
    pub mounts: Vec<BindMount>,
    pub refuse: Option<PathBuf>,
}

/// `mounts { bind SOURCE TARGET read-only=#true; refuse TARGET }`
impl SimState for Mounts {
    const BLOCKS: &'static [&'static str] = &["mounts"];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for node in block.block()? {
            if node.name() == "refuse" {
                node.arity(1, Some(1), "a target")?;
                self.refuse = Some(node.args()[0].string("the target")?.into());
                continue;
            }
            if node.name() != "bind" {
                return Err(node.unknown("`mounts`", &["bind", "refuse"]));
            }
            node.arity(2, Some(2), "a source and a target")?;
            node.props_only(&["read-only"])?;
            let read_only = match node.prop("read-only") {
                Some(v) => v.bool("`read-only`")?,
                None => false,
            };
            self.mounts.push(BindMount {
                source: node.args()[0].string("the source")?.into(),
                target: node.args()[1].string("the target")?.into(),
                read_only,
            });
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("mounts");
        for m in &self.mounts {
            block = block.with_child(
                Node::new("bind")
                    .with_arg(m.source.to_string_lossy().into_owned())
                    .with_arg(m.target.to_string_lossy().into_owned())
                    .with_prop("read-only", m.read_only),
            );
        }
        vec![block]
    }
}

impl Simulated for BindMount {
    type State = Mounts;
}

impl Simulate<BindMount> for BindMount {
    fn run(states: &mut States, op: BindMount) -> io::Result<()> {
        let mounts = states.state::<Mounts>();
        if mounts.refuse.as_ref() == Some(&op.target) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "mount refused"));
        }
        mounts.mounts.push(op);
        Ok(())
    }
}

impl Simulated for Unmount {
    type State = Mounts;
}

impl Simulate<Unmount> for Unmount {
    fn run(states: &mut States, op: Unmount) -> io::Result<()> {
        let mounts = &mut states.state::<Mounts>().mounts;
        match mounts.iter().rposition(|m| m.target == op.target) {
            Some(i) => {
                mounts.remove(i);
                Ok(())
            }
            None => Err(io::Error::new(io::ErrorKind::InvalidInput, "not mounted")),
        }
    }
}
# }
```

`Mounts` has no `check`: the default is that an expected block holds in
`to_world`, as a subset (each expected `bind`, with the same arguments and
at least the expected properties). A state whose expectations have
another shape (something that must be absent, a list that must be exact)
overrides `check`.

This is exactly how seam's own simulated effects are written: `Sleep` is
simulated on `effects::Sleep`, against a `Clock` state; the filesystem's
operations against `Files`. Nothing about the built-in ones is special.

## 5. Testing it in a world

[`world!`](crate::test::world) builds a simulated world that grants
exactly the effects it lists, each with its state. A core takes it like
any capability, and the world reads and writes world files through its
states:

```rust
# #[cfg(all(feature = "test", feature = "std", feature = "env"))] {
# use std::path::PathBuf;
# use seam::{Effect, Operation, Perform, io};
# #[derive(Debug, Clone, PartialEq, Eq)]
# pub struct BindMount { pub source: PathBuf, pub target: PathBuf, pub read_only: bool }
# impl Operation for BindMount { type Outcome = io::Result<()>; }
# impl Effect for BindMount { type Key = BindMount; }
# pub struct Unmount { pub target: PathBuf }
# impl Operation for Unmount { type Outcome = io::Result<()>; }
# impl Effect for Unmount { type Key = Unmount; }
# pub fn bind_all<C>(c: &mut C, mounts: &[BindMount]) -> io::Result<()>
# where C: Perform<BindMount> + Perform<Unmount> {
#     for (done, mount) in mounts.iter().enumerate() {
#         if let Err(e) = c.perform(mount.clone()) {
#             for made in mounts[..done].iter().rev() {
#                 let _ = c.perform(Unmount { target: made.target.clone() });
#             }
#             return Err(e);
#         }
#     }
#     Ok(())
# }
# use seam::test::sim::{SimState, Simulate, Simulated, States};
# use seam::test::world_file::{self, Node};
# #[derive(Debug, Default)]
# pub struct Mounts { pub mounts: Vec<BindMount>, pub refuse: Option<PathBuf> }
# impl SimState for Mounts {
#     const BLOCKS: &'static [&'static str] = &["mounts"];
#     fn load(&mut self, block: &Node) -> world_file::Result<()> {
#         for node in block.block()? {
#             if node.name() == "refuse" {
#                 node.arity(1, Some(1), "a target")?;
#                 self.refuse = Some(node.args()[0].string("the target")?.into());
#                 continue;
#             }
#             node.arity(2, Some(2), "a source and a target")?;
#             let read_only = match node.prop("read-only") { Some(v) => v.bool("`read-only`")?, None => false };
#             self.mounts.push(BindMount {
#                 source: node.args()[0].string("the source")?.into(),
#                 target: node.args()[1].string("the target")?.into(),
#                 read_only,
#             });
#         }
#         Ok(())
#     }
#     fn to_world(&self) -> Vec<Node> {
#         let mut block = Node::new("mounts");
#         for m in &self.mounts {
#             block = block.with_child(
#                 Node::new("bind")
#                     .with_arg(m.source.to_string_lossy().into_owned())
#                     .with_arg(m.target.to_string_lossy().into_owned())
#                     .with_prop("read-only", m.read_only),
#             );
#         }
#         vec![block]
#     }
# }
# impl Simulated for BindMount { type State = Mounts; }
# impl Simulate<BindMount> for BindMount {
#     fn run(states: &mut States, op: BindMount) -> io::Result<()> {
#         let mounts = states.state::<Mounts>();
#         if mounts.refuse.as_ref() == Some(&op.target) {
#             return Err(io::Error::new(io::ErrorKind::PermissionDenied, "mount refused"));
#         }
#         mounts.mounts.push(op);
#         Ok(())
#     }
# }
# impl Simulated for Unmount { type State = Mounts; }
# impl Simulate<Unmount> for Unmount {
#     fn run(states: &mut States, op: Unmount) -> io::Result<()> {
#         let mounts = &mut states.state::<Mounts>().mounts;
#         match mounts.iter().rposition(|m| m.target == op.target) {
#             Some(i) => { mounts.remove(i); Ok(()) }
#             None => Err(io::Error::new(io::ErrorKind::InvalidInput, "not mounted")),
#         }
#     }
# }
use seam::effects as fx;

let mut world = seam::test::world![fx::Var, BindMount, Unmount];
world.load_world(r#"
    env { var MODE test }
    mounts { bind "/srv/base" "/mnt/base"; refuse "/mnt/c" }
"#).unwrap();

let wanted = [
    BindMount { source: "/srv/a".into(), target: "/mnt/a".into(), read_only: false },
    BindMount { source: "/srv/c".into(), target: "/mnt/c".into(), read_only: false },
];
assert!(bind_all(&mut world, &wanted).is_err());

// The refused mount undid `/mnt/a`; the base mount is untouched.
world.check_world(r#"mounts { bind "/srv/base" "/mnt/base" }"#).unwrap();
let mismatch = world.check_world(r#"mounts { bind "/srv/a" "/mnt/a" }"#).unwrap_err();
assert_eq!(
    mismatch.failures[0].message,
    r#"expected `bind "/srv/a" "/mnt/a"`, found nothing"#
);

// The whole world, written out:
assert!(world.to_world().contains(r#"bind "/srv/base" "/mnt/base" read-only=#false"#));
# }
```

- A core performing an effect the world does not list fails to compile:
  "the world does not grant `fx::Sleep`".
- A block no state of the world reads is an error, with a suggestion
  ("unknown `mount` in the world (did you mean `mounts`?)"), and so is a
  block two states claim.
- `world.state(|m: &mut Mounts| ..)` reaches a state directly, to set it
  up or check it in Rust rather than KDL.
- [`Sim`](crate::test::Sim) is the world of every built-in effect. A world
  of your own can list any mix of built-in effects and yours.
- A world is a capability, so it can also be one part of a root, beside
  real parts: `World<fx::Now, fx::Sleep>::clone(&world)`. Every clone is
  the same world.

## 6. Simulated effects built from others

A handler's `States` is itself a capability for every simulated effect,
so a simulated effect can be built from others, the way a real one is.
A key-value store that keeps each value in a file of the simulated
filesystem has no state of its own; its handlers perform the
filesystem's effects:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::{Effect, Operation, Query, io};
use seam::effects as fx;
use seam::ops::fs::{read_to_string, write_file};
use seam::prelude::*;
use seam::test::sim::{Simulate, Simulated, Stateless, States};

pub struct Put(pub String, pub String);
impl Operation for Put { type Outcome = io::Result<()>; }
impl Effect for Put { type Key = Put; }

pub struct Get(pub String);
impl Operation for Get { type Outcome = io::Result<String>; }
impl Query for Get {}
impl Effect for Get { type Key = Get; }

impl Simulated for Put { type State = Stateless; }
impl Simulate<Put> for Put {
    fn run(states: &mut States, op: Put) -> io::Result<()> {
        write_file(states, &format!("/kv/{}", op.0), &op.1)
    }
}

impl Simulated for Get { type State = Stateless; }
impl Simulate<Get> for Get {
    fn run(states: &mut States, op: Get) -> io::Result<String> {
        read_to_string(&*states, &format!("/kv/{}", op.0))
    }
}

let mut world = seam::test::world![fx::View, Put, Get];
world.load_world(r#"fs { dir "/kv" }"#).unwrap();
world.perform(Put("k".into(), "v".into())).unwrap();
assert_eq!(world.inspect(Get("k".into())).unwrap(), "v");
world.check_world(r#"fs { file "/kv/k" "v" }"#).unwrap();
# }
```

Each operation a handler performs this way runs as a step of its own: the
world is unlocked around it, so the handles it opens can reach the world.

## Variations

- **A plain fake, without a world.** For a quick test of one effect, a
  struct that implements `Perform<BindMount>` itself, listed in a root
  next to a `Sim`, works too. It has no world-file blocks.
- **Async handlers** implement `AsyncInspect` / `AsyncPerform`, and the
  `Send` forms for multithreaded runtimes, returning
  `impl Future<Output = ..> + Send`. A world implements every form of
  every effect it grants.
- **Effects built from other effects, for real.** A handler can hold
  another capability and perform through it: a store kept in files is a
  `FileStore<C>` whose `Put` writes through `C`'s `CreateAtomic`. It then
  runs on every backend. See the `custom_ops` example.
- **Another implementation of seam's own effects** (a different
  filesystem, a remote clock) implements seam's operations and is listed in
  a root under seam's keys (`fx::View`, `fx::Now`, ...) in place of a
  backend.
- **Scoped effects** of your own implement
  [`Acquisition`](crate::Acquisition), and the handler implements
  `Perform<Opening<A>>` and `Perform<Closing<A>>` (or `Simulate` for both,
  in a world). Dropping the resource must roll it back; only closing
  commits.
