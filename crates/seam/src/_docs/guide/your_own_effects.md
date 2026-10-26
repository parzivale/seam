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

## 4. A simulated handler

For tests, a fake that records what is mounted, and can be told to refuse
one target:

```rust
# use std::path::PathBuf;
# use seam::{Effect, Operation, io};
# #[derive(Debug, Clone, PartialEq, Eq)]
# pub struct BindMount { pub source: PathBuf, pub target: PathBuf, pub read_only: bool }
# impl Operation for BindMount { type Outcome = io::Result<()>; }
# pub struct Unmount { pub target: PathBuf }
# impl Operation for Unmount { type Outcome = io::Result<()>; }
use seam::{Capability, Perform};

#[derive(Debug, Default)]
pub struct Recorded {
    pub mounts: Vec<BindMount>,
    pub fail_on: Option<PathBuf>,
}
impl Capability for Recorded {}

impl Perform<BindMount> for Recorded {
    fn perform(&mut self, op: BindMount) -> io::Result<()> {
        if self.fail_on.as_ref() == Some(&op.target) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "mount refused"));
        }
        self.mounts.push(op);
        Ok(())
    }
}

impl Perform<Unmount> for Recorded {
    fn perform(&mut self, op: Unmount) -> io::Result<()> {
        match self.mounts.iter().rposition(|m| m.target == op.target) {
            Some(i) => {
                self.mounts.remove(i);
                Ok(())
            }
            None => Err(io::Error::new(io::ErrorKind::InvalidInput, "not mounted")),
        }
    }
}
```

A fake can be as faithful as the tests need. If it must touch the rest of
the simulated world (keep its state in a simulated file, print through the
simulated stdout), give it a clone of the test's `Sim` and perform through
that: every clone of a `Sim` is the same world. See
[roots](crate::_docs::guide::roots#parts-built-on-other-parts).

## 5. World-file blocks for it

A simulated part can read its own blocks of a
[world file](crate::_docs::world_files), so one file sets up and checks the
whole simulated world, `Sim` and fakes alike. Implement `World`: the block
names it reads, `load` to set the part up from one, and `check` to compare
the part against one after a run. Read the nodes with the public `Node`
and `Value`, whose checks report the line and column of a mistake:

```rust
# #[cfg(all(feature = "test", feature = "std"))] {
# use std::path::PathBuf;
# use seam::{Capability, Effect, Operation, Perform, io};
# #[derive(Debug, Clone, PartialEq, Eq)]
# pub struct BindMount { pub source: PathBuf, pub target: PathBuf, pub read_only: bool }
# impl Operation for BindMount { type Outcome = io::Result<()>; }
# #[derive(Debug, Default)]
# pub struct Recorded { pub mounts: Vec<BindMount>, pub fail_on: Option<PathBuf> }
use seam::test::world_file::{self, Failure, Node, World};

/// `mounts { bind "/srv/a" "/mnt/a" read-only=#true }`: mounts in place
/// before a run, or expected after one.
impl World for Recorded {
    const BLOCKS: &'static [&'static str] = &["mounts"];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for node in block.block()? {
            self.mounts.push(entry(node)?);
        }
        Ok(())
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        for node in block.block()? {
            let want = entry(node)?;
            if !self.mounts.contains(&want) {
                failures.push(node.failure(format!(
                    "{} is not bound to {}",
                    want.target.display(),
                    want.source.display()
                )));
            }
        }
        Ok(())
    }
}

/// One `bind SOURCE TARGET [read-only=#true]` line.
fn entry(node: &Node) -> world_file::Result<BindMount> {
    if node.name() != "bind" {
        return Err(node.unknown("`mounts`", &["bind"]));
    }
    node.arity(2, Some(2), "a source and a target")?;
    node.props_only(&["read-only"])?;
    node.no_children()?;
    Ok(BindMount {
        source: node.args()[0].string("the source")?.into(),
        target: node.args()[1].string("the target")?.into(),
        read_only: match node.prop("read-only") {
            Some(v) => v.bool("`read-only`")?,
            None => false,
        },
    })
}

// On its own, outside a root:
let mut fake = Recorded::default();
world_file::load(&mut fake, r#"mounts { bind "/srv/a" "/mnt/a" }"#).unwrap();
assert_eq!(fake.mounts.len(), 1);
world_file::check(&fake, r#"mounts { bind "/srv/a" "/mnt/a" }"#).unwrap();
# }
```

This is exactly how `Sim` reads its own blocks: its `World` impl uses
the same public API, and seam's tests compile it outside seam-test to keep
it that way. So anything `Sim`'s world files can do, a part of yours can
do too.

## 6. Testing it in a root

Put the fake in a root beside a `Sim`, and drive both from one world file.
`load_world` and `check_world` take the effects whose parts take part, as
a tuple of keys; each block goes to the part that reads it:

```rust
# #[cfg(all(feature = "test", feature = "std"))] {
# use std::path::PathBuf;
# use seam::{Capability, Effect, Operation, Perform, io};
# use seam::test::world_file::{self, Failure, Node, World};
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
# #[derive(Debug, Default)]
# pub struct Recorded { pub mounts: Vec<BindMount>, pub fail_on: Option<PathBuf> }
# impl Capability for Recorded {}
# impl Perform<BindMount> for Recorded {
#     fn perform(&mut self, op: BindMount) -> io::Result<()> {
#         if self.fail_on.as_ref() == Some(&op.target) {
#             return Err(io::Error::new(io::ErrorKind::PermissionDenied, "mount refused"));
#         }
#         self.mounts.push(op);
#         Ok(())
#     }
# }
# impl Perform<Unmount> for Recorded {
#     fn perform(&mut self, op: Unmount) -> io::Result<()> {
#         let i = self.mounts.iter().rposition(|m| m.target == op.target).unwrap();
#         self.mounts.remove(i);
#         Ok(())
#     }
# }
# impl World for Recorded {
#     const BLOCKS: &'static [&'static str] = &["mounts"];
#     fn load(&mut self, block: &Node) -> world_file::Result<()> {
#         for node in block.block()? { self.mounts.push(entry(node)?); }
#         Ok(())
#     }
#     fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
#         for node in block.block()? {
#             if !self.mounts.contains(&entry(node)?) { failures.push(node.failure("not bound")); }
#         }
#         Ok(())
#     }
# }
# fn entry(node: &Node) -> world_file::Result<BindMount> {
#     node.arity(2, Some(2), "a source and a target")?;
#     Ok(BindMount {
#         source: node.args()[0].string("the source")?.into(),
#         target: node.args()[1].string("the target")?.into(),
#         read_only: false,
#     })
# }
use seam::effects as fx;
use seam::test::{Sim, check_world, load_world};

let mut root = seam::root![
    Sim<fx::Var>::new(),
    Recorded<BindMount, Unmount> { mounts: Vec::new(), fail_on: Some("/mnt/c".into()) },
];
load_world::<(fx::Var, BindMount), _>(&mut root, r#"
    env { var MODE test }
    mounts { bind "/srv/base" "/mnt/base" }
"#)
.unwrap();

let wanted = [
    BindMount { source: "/srv/a".into(), target: "/mnt/a".into(), read_only: false },
    BindMount { source: "/srv/c".into(), target: "/mnt/c".into(), read_only: false },
];
assert!(bind_all(&mut root, &wanted).is_err());

// The refused mount undid `/mnt/a`; the base mount is untouched.
check_world::<(BindMount,), _>(&root, r#"mounts { bind "/srv/base" "/mnt/base" }"#).unwrap();
assert_eq!(root.part::<BindMount>().mounts.len(), 1);
# }
```

A block that no listed part reads is an error ("no part of the root reads
`mount` (did you mean `mounts`?)"), and so is a block two different parts
claim.

## Variations

- **Async handlers** implement `AsyncInspect` / `AsyncPerform`, and the
  `Send` forms for multithreaded runtimes, returning
  `impl Future<Output = ..> + Send`. A sync handler gets all three through
  the lifts.
- **Effects built from other effects.** A handler can hold another
  capability and perform through it: a store kept in files is a
  `FileStore<C>` whose `Put` writes through `C`'s `CreateAtomic`. It then
  runs on every backend, the simulation included, and on the simulated
  runtime and threads its interleavings are explored like any other. See
  the `custom_ops` example.
- **Another implementation of seam's own effects** (a different
  filesystem, a remote clock) implements seam's operations and is listed in
  a root under seam's keys (`fx::View`, `fx::Now`, ...) in place of a
  backend.
- **Scoped effects** of your own implement
  [`Acquisition`](crate::Acquisition), and the handler implements
  `Perform<Opening<A>>` and `Perform<Closing<A>>` (or `Inspect`, for a
  scope that changes nothing outside itself). Dropping the resource must
  roll it back; only closing commits.
