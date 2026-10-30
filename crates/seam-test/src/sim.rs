//! The simulated world, and how a handler's state is written in a world
//! file.
//!
//! [`Sim`] is a handler like any other: it implements seam's `Perform` and
//! `Inspect` for every effect it simulates, exactly as `Std` and `Tokio`
//! do, so a core takes `&mut sim` wherever it takes a real handler, and a
//! root lists it under the effects it should serve.
//!
//! Its world is made of states, one per area:
//!
//! | State | Blocks | Effects |
//! |---|---|---|
//! | `Clock` | `clock` | `Now`, `Sleep`, `WallClock` |
//! | `Random` | `seed` | `FillRandom` |
//! | `Env` | `env` | `Var`, `SetVar`, `RemoveVar`, `Args`, `CurrentDir`, `TempDir` |
//! | `Files` | `fs` | `Metadata`, `CreateDir`, `RemoveFile`, `RemoveDir`, `Rename`, `CopyFile`, `Open`, `CreateAtomic`, `View`, `ReadDir` |
//! | `Net` | `net` | `ResolveInto`, `TcpBind`, `TcpConnect`, `UdpBind` |
//! | `Stdio` | `stdio` | `OpenStdin`, `OpenStdout`, `OpenStderr` |
//! | `Processes` | `process` | `RunCommand`, `SpawnChild`, `Exit` |
//! | `Signals` | `signal` | `CtrlC` |
//! | `Tasks` | `parallelism` | `AvailableParallelism` |
//!
//! Each needs its seam-test feature.
//!
//! # World files
//!
//! [`World`] is the one trait the simulation adds, and it is not about
//! handling effects: it describes a handler's state in a world file. Any
//! handler with state implements it (`Sim`, and a fake of a program's own),
//! so one world file sets up, checks and snapshots a whole root; see
//! [`load_world`](crate::load_world).
#![allow(clippy::disallowed_types)] // the states are shared with every handle on the world

use core::any::{Any, TypeId};
use core::cell::RefCell;
use std::boxed::Box;
use std::format;
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use crate::world_file::{self, Failure, Node, WorldError, WorldMismatch, check_subset};

#[cfg(feature = "env")]
pub use crate::env::Env;
#[cfg(feature = "fs")]
pub use crate::fs::Files;
#[cfg(feature = "net")]
pub use crate::net::Net;
#[cfg(feature = "process")]
pub use crate::process::Processes;
#[cfg(feature = "random")]
pub use crate::random::Random;
#[cfg(feature = "signal")]
pub use crate::signal::Signals;
#[cfg(feature = "io-std")]
pub use crate::stdio::Stdio;
#[cfg(feature = "rt")]
pub use crate::tasks::Tasks;
#[cfg(any(feature = "time", feature = "fs"))]
pub use crate::time::Clock;

/// A handler's state, as written in a world file: the blocks it reads, how
/// it is set up from one, and how it is written out.
///
/// [`load`](Self::load) sets it up from one of its blocks, and
/// [`to_world`](Self::to_world) writes it out; expectations are checked
/// against that, as a subset, unless the state checks a block itself.
pub trait World {
    /// The top-level blocks it reads and writes, by name.
    const BLOCKS: &'static [&'static str];

    /// Sets it up from one of its blocks, on top of what is already set.
    fn load(&mut self, block: &Node) -> world_file::Result<()>;

    /// Its contents, as top-level world-file nodes.
    fn to_world(&self) -> Vec<Node>;

    /// Checks it against one of its blocks, adding a [`Failure`] for each
    /// expectation that does not hold. By default, every expected node must
    /// be in [`to_world`](Self::to_world) (see [`check_subset`]); a state
    /// with expectations of another shape (something that must be absent,
    /// a list that must be exact) checks those itself.
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        check_subset(&self.to_world(), block, failures);
        Ok(())
    }
}

/// The simulated world's states, one per type, in the order they were
/// added: what the world's lock guards.
#[doc(hidden)]
pub struct Inner {
    list: Vec<Entry>,
}

struct Entry {
    #[allow(dead_code)] // read by whichever areas are on
    id: TypeId,
    blocks: &'static [&'static str],
    state: Box<dyn DynState>,
}

/// A state behind a pointer: what the world needs of it.
trait DynState: Send {
    fn load(&mut self, block: &Node) -> world_file::Result<()>;
    fn to_world(&self) -> Vec<Node>;
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()>;
    #[allow(dead_code)] // used by whichever areas are on
    fn as_any(&mut self) -> &mut dyn Any;
}

impl<S: World + Any + Send> DynState for S {
    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        World::load(self, block)
    }

    fn to_world(&self) -> Vec<Node> {
        World::to_world(self)
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        World::check(self, block, failures)
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

impl core::fmt::Debug for Inner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Inner")
            .field("count", &self.list.len())
            .finish_non_exhaustive()
    }
}

impl Inner {
    /// The state of type `S`, created fresh if the world has none yet.
    #[allow(dead_code)] // used by whichever areas are on
    pub(crate) fn state<S: World + Default + Any + Send>(&mut self) -> &mut S {
        let id = TypeId::of::<S>();
        let i = match self.list.iter().position(|e| e.id == id) {
            Some(i) => i,
            None => {
                self.list.push(Entry {
                    id,
                    blocks: S::BLOCKS,
                    state: Box::new(S::default()),
                });
                self.list.len() - 1
            }
        };
        self.list[i]
            .state
            .as_any()
            .downcast_mut()
            .unwrap_or_else(|| unreachable!("entries are found by their type"))
    }

    /// The state reading the block `node`, as an index.
    fn owner(&self, node: &Node) -> world_file::Result<usize> {
        match self
            .list
            .iter()
            .position(|e| e.blocks.contains(&node.name()))
        {
            Some(i) => Ok(i),
            None => Err(world_file::unknown_block(node, Sim::BLOCKS)),
        }
    }
}

/// The states, as an operation of the simulation sees them: locked the
/// first time a state is reached, and unlocked when the operation returns,
/// so an operation sees and changes the world as one step.
#[allow(dead_code)] // used by whichever areas are on
pub(crate) struct States<'a> {
    shared: &'a Shared,
    guard: RefCell<Option<MutexGuard<'a, Inner>>>,
}

#[allow(dead_code)] // used by whichever areas are on
impl<'a> States<'a> {
    pub(crate) fn new(shared: &'a Shared) -> Self {
        Self {
            shared,
            guard: RefCell::new(None),
        }
    }

    /// The state of type `S`.
    pub(crate) fn state<S: World + Default + Any + Send>(&mut self) -> &mut S {
        self.inner().state()
    }

    /// The states, locked.
    pub(crate) fn inner(&mut self) -> &mut Inner {
        let shared = self.shared;
        self.guard.get_mut().get_or_insert_with(|| shared.lock())
    }

    /// Unlocks the world: for an operation that runs code of the test's own
    /// (a scripted command's handler).
    #[cfg_attr(not(feature = "process"), allow(dead_code))]
    pub(crate) fn unlock(&self) {
        drop(self.guard.borrow_mut().take());
    }

    /// A handle on the world, for a handle the operation hands out (an open
    /// file, a socket) to reach it later.
    #[cfg_attr(
        not(any(feature = "fs", feature = "net", feature = "io-std")),
        allow(dead_code)
    )]
    pub(crate) fn shared(&self) -> Shared {
        self.shared.clone()
    }
}

/// A handle on the world's states, kept by `Sim` and by the handles it
/// hands out.
#[derive(Clone)]
pub(crate) struct Shared(Arc<Mutex<Inner>>);

impl Shared {
    /// Locks the states. A panic in another holder (a failed assertion in a
    /// test thread) does not poison them for the rest of the test.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl core::fmt::Debug for Shared {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Shared(..)")
    }
}

/// A simulated world: seam-test's handler.
///
/// It implements seam's effects as `Std` does, with a virtual clock,
/// seeded random numbers, an in-memory filesystem and network, scripted
/// commands and captured stdio. Create one per test, set up the world it
/// starts in, hand it to the core in place of a real handler, then assert on
/// what the core left behind.
///
/// Every clone is the same world, so a part of a root that works over the
/// simulation is given a clone, and the test asserts through any of them:
///
/// ```
/// # #[cfg(feature = "fs")] {
/// let sim = seam_test::Sim::new();
/// let mut other = sim.clone();
/// other.add_file("/tmp/a", "hi");
/// assert_eq!(sim.file("/tmp/a").unwrap(), b"hi");
/// # }
/// ```
#[derive(Clone)]
pub struct Sim {
    pub(crate) shared: Shared,
}

impl core::fmt::Debug for Sim {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sim").finish_non_exhaustive()
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for Sim {}

impl Sim {
    /// A fresh world with seed 0.
    ///
    /// The clock reads `Instant::EPOCH`, the wall clock reads the Unix
    /// epoch, the current directory is `/`, and the filesystem holds an
    /// empty `/` and `/tmp`.
    pub fn new() -> Self {
        Self::with_seed(0)
    }

    /// A fresh world whose random numbers come from `seed`.
    pub fn with_seed(seed: u64) -> Self {
        #[cfg(not(feature = "random"))]
        let _ = seed;
        let sim = Self {
            shared: Shared(Arc::new(Mutex::new(Inner { list: Vec::new() }))),
        };
        {
            #[allow(unused_variables, unused_mut)]
            let mut states = sim.lock();
            #[cfg(feature = "random")]
            states.state::<Random>().reseed(seed);
            #[cfg(feature = "rt")]
            states.state::<Tasks>();
            #[cfg(any(feature = "time", feature = "fs"))]
            states.state::<Clock>();
            #[cfg(feature = "env")]
            states.state::<Env>();
            #[cfg(feature = "fs")]
            states.state::<Files>();
            #[cfg(feature = "net")]
            states.state::<Net>();
            #[cfg(feature = "process")]
            states.state::<Processes>();
            #[cfg(feature = "io-std")]
            states.state::<Stdio>();
            #[cfg(feature = "signal")]
            states.state::<Signals>();
        }
        sim
    }

    /// A fresh world, set up by a world file. See
    /// [the format](crate::world_file).
    ///
    /// ```
    /// # #[cfg(all(feature = "fs", feature = "env"))] {
    /// use seam_test::Sim;
    ///
    /// let sim = Sim::from_world(r#"
    ///     env { var USER ada }
    ///     fs {
    ///         dir "/home/ada" { file notes.txt "buy milk" }
    ///     }
    /// "#).unwrap();
    /// assert_eq!(sim.file("/home/ada/notes.txt").unwrap(), b"buy milk");
    /// # }
    /// ```
    pub fn from_world(text: &str) -> Result<Self, WorldError> {
        let mut sim = Self::new();
        sim.load_world(text)?;
        Ok(sim)
    }

    /// Locks the world's states.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.shared.lock()
    }

    /// Runs `core` against this world and returns the world, for chaining
    /// assertions.
    pub fn run(mut self, core: impl FnOnce(&mut Self)) -> Self {
        core(&mut self);
        self
    }

    /// Sets the world up further from a world file: a base world, then a
    /// test's own changes. On an error, the blocks before it have been
    /// applied.
    pub fn load_world(&mut self, text: &str) -> Result<&mut Self, WorldError> {
        world_file::load(self, text)?;
        Ok(self)
    }

    /// Checks the world against expectations written as a world file.
    /// Every failing expectation is reported, with its line. See
    /// [the format](crate::world_file#expectations).
    pub fn check_world(&self, text: &str) -> Result<(), WorldMismatch> {
        world_file::check(self, text)
    }

    /// Like [`check_world`](Self::check_world), but panics with the
    /// failures, for use in tests.
    ///
    /// # Panics
    ///
    /// If the world does not match, or the expectations cannot be read.
    #[track_caller]
    pub fn assert_world(&self, text: &str) {
        if let Err(mismatch) = self.check_world(text) {
            panic!("{mismatch}");
        }
    }

    /// The whole world, written out as a world file. Loading it into a
    /// fresh `Sim` gives the same world.
    pub fn to_world(&self) -> String {
        World::to_world(self)
            .iter()
            .map(|node| format!("{node}"))
            .collect()
    }
}

/// Every block of every state the simulation has; each goes to its state.
impl World for Sim {
    const BLOCKS: &'static [&'static str] = &[
        #[cfg(feature = "random")]
        "seed",
        #[cfg(feature = "rt")]
        "parallelism",
        #[cfg(any(feature = "time", feature = "fs"))]
        "clock",
        #[cfg(feature = "env")]
        "env",
        #[cfg(feature = "fs")]
        "fs",
        #[cfg(feature = "net")]
        "net",
        #[cfg(feature = "process")]
        "process",
        #[cfg(feature = "io-std")]
        "stdio",
        #[cfg(feature = "signal")]
        "signal",
    ];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        let mut states = self.lock();
        let i = states.owner(block)?;
        states.list[i].state.load(block)
    }

    fn to_world(&self) -> Vec<Node> {
        let states = self.lock();
        states
            .list
            .iter()
            .flat_map(|e| e.state.to_world())
            .collect()
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        let states = self.lock();
        let i = states.owner(block)?;
        states.list[i].state.check(block, failures)
    }
}
