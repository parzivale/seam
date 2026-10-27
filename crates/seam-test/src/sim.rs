//! Simulated effects, each defined on its effect's own key.
//!
//! An effect is simulated by implementing two sister traits on its key, the
//! type a root routes it by (`effects::Sleep` for `Sleep`, or your own
//! effect, which is usually its own key):
//!
//! - [`Simulated`] names the state the effect works on: `Clock` for
//!   `Now` and `Sleep`, a `Mounts` of yours for a program's `BindMount`.
//! - [`Simulate<O>`] runs an operation `O` against the world's states.
//!
//! The state itself implements [`SimState`]: the world-file blocks it
//! reads, and its contents written out as a world file. A [`World`] holds
//! one state per state type, so effects that share a state (`Now` and
//! `Sleep`) share one value, and a handler can use other states too
//! (writing a file stamps it with the clock).
//!
//! [`world!`](crate::world!) builds a world that grants exactly the effects
//! it lists, and [`Sim`] is the world of every built-in effect:
//!
//! ```
//! # #[cfg(feature = "time")] {
//! use core::time::Duration;
//! use seam_core::effects as fx;
//! use seam_core::ops::time::{Now, Sleep};
//! use seam_core::{Inspect, Perform};
//!
//! /// The core: generic, as always.
//! fn nap<C: Perform<Sleep> + Inspect<Now>>(c: &mut C) -> Duration {
//!     c.perform(Sleep(Duration::from_secs(90)));
//!     c.inspect(Now).since_epoch()
//! }
//!
//! let mut world = seam_test::world![fx::Now, fx::Sleep];
//! world.load_world("clock now=10").unwrap();
//! assert_eq!(nap(&mut world), Duration::from_secs(100));
//! world.check_world("clock now=100").unwrap();
//! assert_eq!(world.to_world(), "clock now=100 wall=100\n");
//! # }
//! ```
//!
//! # The built-in states
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
//! | `Stateless` | none | `Channel`, `Share` |
//!
//! Each needs its seam-test feature.
#![allow(clippy::disallowed_types)] // the states are shared with every handle on the world

use core::any::{Any, TypeId};
use core::cell::RefCell;
use core::future::Future;
use core::marker::PhantomData;
use std::boxed::Box;
use std::format;
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform};
use seam_core::{Effect, Inspect, Operation, Perform, Query};

use crate::world_file::{
    self, Failure, Node, WorldError, WorldMismatch, check_subset, parse_world,
};

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

/// A part of the simulated world: what effects work on, and how it is
/// written in a world file.
///
/// `Default` is the state of a fresh world. [`load`](Self::load) sets it up
/// from one of its blocks, and [`to_world`](Self::to_world) writes it out;
/// expectations are checked against that, as a subset, unless the state
/// checks a block itself.
pub trait SimState: Any + Send + Default {
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

/// The state an effect works on. Implemented on the effect's key, beside
/// [`Simulate`].
pub trait Simulated {
    /// The state.
    type State: SimState;
}

/// How an operation is simulated. Implemented on its effect's key: one
/// impl per operation the key routes.
#[diagnostic::on_unimplemented(
    message = "`{Self}` has no simulation for `{O}`",
    label = "no `Simulate<{O}>` impl",
    note = "implement `seam_test::sim::Simulate<{O}>` for the effect's key"
)]
pub trait Simulate<O: Operation>: Simulated {
    /// Runs `op` on the world's states.
    fn run(states: &mut States<'_>, op: O) -> O::Outcome;
}

/// The effects a world grants: [`world!`](crate::world!) implements it for
/// each key it lists, and [`Everything`] for every built-in key.
#[diagnostic::on_unimplemented(
    message = "the world does not grant `{K}`",
    label = "`{K}` is not listed",
    note = "list `{K}` in `world![..]`"
)]
pub trait Grants<K> {}

/// A state with nothing in it, for effects that keep no state in the world
/// (a channel's state lives in its endpoints). It reads no blocks.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stateless;

impl SimState for Stateless {
    const BLOCKS: &'static [&'static str] = &[];

    fn load(&mut self, _: &Node) -> world_file::Result<()> {
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        Vec::new()
    }
}

/// The world's states, one per type, in the order they were added: what a
/// world's lock guards. Its helpers are the crate's own.
#[doc(hidden)]
pub struct Inner {
    list: Vec<Entry>,
}

struct Entry {
    id: TypeId,
    blocks: &'static [&'static str],
    state: Box<dyn DynState>,
}

/// A [`SimState`] behind a pointer: what the world needs of it.
trait DynState: Send {
    fn load(&mut self, block: &Node) -> world_file::Result<()>;
    fn to_world(&self) -> Vec<Node>;
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()>;
    fn as_any(&mut self) -> &mut dyn Any;
}

impl<S: SimState> DynState for S {
    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        SimState::load(self, block)
    }

    fn to_world(&self) -> Vec<Node> {
        SimState::to_world(self)
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        SimState::check(self, block, failures)
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

/// The world's states, as a [`Simulate`] impl sees them. The world is
/// locked when a state is first reached, and stays locked until the
/// operation returns, so an operation sees and changes the world as one
/// step.
///
/// `States` is also a capability for every simulated effect, so a handler
/// can be built from other effects: a store's `Put` that writes a file
/// calls `write_file(states, ..)`, as a core would. Each of those runs as
/// a step of its own, so the world is unlocked around it.
pub struct States<'a> {
    shared: &'a Shared,
    guard: RefCell<Option<MutexGuard<'a, Inner>>>,
}

impl<'a> States<'a> {
    fn new(shared: &'a Shared) -> Self {
        Self {
            shared,
            guard: RefCell::new(None),
        }
    }

    /// The state of type `S`, created fresh if the world has none yet.
    pub fn state<S: SimState>(&mut self) -> &mut S {
        self.inner().state()
    }

    /// The states, locked.
    pub(crate) fn inner(&mut self) -> &mut Inner {
        let shared = self.shared;
        self.guard.get_mut().get_or_insert_with(|| shared.lock())
    }

    /// Unlocks the world: for an operation that runs code of the test's own
    /// (a scripted command's handler), or another operation.
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

impl core::fmt::Debug for States<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("States").finish_non_exhaustive()
    }
}

/// Performs another simulated effect, as a step of its own.
impl<O> Perform<O> for States<'_>
where
    O: Operation + Effect,
    O::Key: Simulate<O>,
{
    fn perform(&mut self, op: O) -> O::Outcome {
        self.unlock();
        let out = <O::Key as Simulate<O>>::run(self, op);
        self.unlock();
        out
    }
}

/// Runs another simulated query, as a step of its own.
impl<Q> Inspect<Q> for States<'_>
where
    Q: Query + Effect,
    Q::Key: Simulate<Q>,
{
    fn inspect(&self, query: Q) -> Q::Outcome {
        self.unlock();
        <Q::Key as Simulate<Q>>::run(&mut States::new(self.shared), query)
    }
}

impl Inner {
    /// The state of type `S`, created fresh if the world has none yet.
    pub(crate) fn state<S: SimState>(&mut self) -> &mut S {
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

    /// The state reading the block `node`, as an index; two states reading
    /// one block is an error.
    fn owner(&self, node: &Node) -> world_file::Result<usize> {
        let mut owners = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.blocks.contains(&node.name()));
        match (owners.next(), owners.next()) {
            (Some((i, _)), None) => Ok(i),
            (Some(_), Some(_)) => {
                Err(node.error(format!("two parts of the world read `{}`", node.name())))
            }
            (None, _) => {
                if let Some(feature) = feature_off(node.name()) {
                    return Err(node.error(format!(
                        "`{}` needs seam-test's `{feature}` feature",
                        node.name()
                    )));
                }
                let all: Vec<&str> = self
                    .list
                    .iter()
                    .flat_map(|e| e.blocks.iter().copied())
                    .collect();
                Err(node.unknown("the world", &all))
            }
        }
    }
}

/// The feature a built-in block needs, if it is off.
fn feature_off(block: &str) -> Option<&'static str> {
    let off: &[(&str, &str, bool)] = &[
        ("clock", "time", cfg!(feature = "time")),
        ("seed", "random", cfg!(feature = "random")),
        ("env", "env", cfg!(feature = "env")),
        ("fs", "fs", cfg!(feature = "fs")),
        ("net", "net", cfg!(feature = "net")),
        ("stdio", "io-std", cfg!(feature = "io-std")),
        ("process", "process", cfg!(feature = "process")),
        ("signal", "signal", cfg!(feature = "signal")),
        ("parallelism", "rt", cfg!(feature = "rt")),
    ];
    off.iter()
        .find(|(name, _, on)| *name == block && !on)
        .map(|(_, feature, _)| *feature)
}

/// A handle on a world's states, kept by the handles a world hands out.
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

/// A simulated world: the states of the effects it grants, `G`. Build one
/// with [`world!`](crate::world!), or use [`Sim`], the world of every
/// built-in effect.
///
/// A world is a capability: a core takes `&mut world` wherever it takes a
/// backend or a root. Every clone is a handle to the same states.
pub struct World<G> {
    shared: Shared,
    grants: PhantomData<fn() -> G>,
}

impl<G> Clone for World<G> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            grants: PhantomData,
        }
    }
}

impl<G> core::fmt::Debug for World<G> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("World").finish_non_exhaustive()
    }
}

impl<G> World<G> {
    /// A world with no states yet. Used by `world!`.
    #[doc(hidden)]
    pub fn empty() -> Self {
        let states = Arc::new(Mutex::new(Inner { list: Vec::new() }));
        Self {
            shared: Shared(states),
            grants: PhantomData,
        }
    }

    /// Adds the state `S`, fresh, unless the world has it. Used by `world!`.
    #[doc(hidden)]
    #[must_use]
    pub fn with_state<S: SimState>(self) -> Self {
        self.lock().state::<S>();
        self
    }

    /// Locks the world's states.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.shared.lock()
    }

    /// Runs `f` on the state `S` (created fresh if the world has none yet):
    /// for setting the world up, or checking it, from a test. The world is
    /// locked while `f` runs, so `f` must not use the world itself.
    pub fn state<S: SimState, R>(&self, f: impl FnOnce(&mut S) -> R) -> R {
        f(self.lock().state::<S>())
    }

    /// Runs `core` against this world and returns the world, for chaining
    /// assertions.
    pub fn run(mut self, core: impl FnOnce(&mut Self)) -> Self {
        core(&mut self);
        self
    }

    /// Sets the world up further from a world file: each block goes to the
    /// state that reads it, in order. A base world, then a test's own
    /// changes. On an error, the blocks before it have been applied.
    pub fn load_world(&mut self, text: &str) -> Result<&mut Self, WorldError> {
        let nodes = parse_world(text)?;
        let mut states = self.lock();
        for node in &nodes {
            let i = states.owner(node)?;
            states.list[i].state.load(node)?;
        }
        drop(states);
        Ok(self)
    }

    /// Checks the world against expectations written as a world file.
    /// Every failing expectation is reported, with its line. See
    /// [the format](crate::world_file#expectations).
    pub fn check_world(&self, text: &str) -> Result<(), WorldMismatch> {
        let nodes = parse_world(text).map_err(WorldMismatch::invalid)?;
        let states = self.lock();
        let mut failures = Vec::new();
        for node in &nodes {
            let i = states.owner(node).map_err(WorldMismatch::invalid)?;
            states.list[i]
                .state
                .check(node, &mut failures)
                .map_err(WorldMismatch::invalid)?;
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(WorldMismatch { failures })
        }
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

    /// The whole world, written out as a world file: every state's blocks,
    /// in the order the states were added. Loading it into a fresh world
    /// of the same effects gives the same world.
    pub fn to_world(&self) -> String {
        let states = self.lock();
        states
            .list
            .iter()
            .flat_map(|e| e.state.to_world())
            .map(|node| format!("{node}"))
            .collect()
    }
}

impl<G, Q> Inspect<Q> for World<G>
where
    Q: Query + Effect,
    Q::Key: Simulate<Q>,
    G: Grants<Q::Key>,
{
    fn inspect(&self, query: Q) -> Q::Outcome {
        <Q::Key as Simulate<Q>>::run(&mut States::new(&self.shared), query)
    }
}

impl<G, O> Perform<O> for World<G>
where
    O: Operation + Effect,
    O::Key: Simulate<O>,
    G: Grants<O::Key>,
{
    fn perform(&mut self, op: O) -> O::Outcome {
        <O::Key as Simulate<O>>::run(&mut States::new(&self.shared), op)
    }
}

// Nothing in a world waits, so the async forms run the operation at once.

impl<G, Q> AsyncInspect<Q> for World<G>
where
    Q: Query + Effect,
    Q::Key: Simulate<Q>,
    G: Grants<Q::Key>,
{
    async fn inspect_async(&self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}

impl<G, O> AsyncPerform<O> for World<G>
where
    O: Operation + Effect,
    O::Key: Simulate<O>,
    G: Grants<O::Key>,
{
    async fn perform_async(&mut self, op: O) -> O::Outcome {
        self.perform(op)
    }
}

impl<G, Q> SendInspect<Q> for World<G>
where
    Q: Query + Effect,
    Q::Key: Simulate<Q>,
    Q::Outcome: Send,
    G: Grants<Q::Key>,
{
    fn inspect_send(&self, query: Q) -> impl Future<Output = Q::Outcome> + Send {
        core::future::ready(self.inspect(query))
    }
}

impl<G, O> SendPerform<O> for World<G>
where
    O: Operation + Effect,
    O::Key: Simulate<O>,
    O::Outcome: Send,
    G: Grants<O::Key>,
{
    fn perform_send(&mut self, op: O) -> impl Future<Output = O::Outcome> + Send {
        core::future::ready(self.perform(op))
    }
}

/// Builds a simulated [`World`] that grants the effects listed, by their
/// keys, each with its simulated state:
///
/// ```
/// # #[cfg(feature = "time")] {
/// use seam_core::effects as fx;
/// let world = seam_test::world![fx::Now, fx::Sleep];
/// # let _ = world;
/// # }
/// ```
///
/// A core performing an effect the world does not list fails to compile
/// ("the world does not grant `fx::Sleep`"), and so does a key with no
/// [`Simulate`](crate::sim::Simulate) impl:
///
/// ```compile_fail
/// # #[cfg(feature = "time")] {
/// use seam_core::effects as fx;
/// use seam_core::Perform;
/// let mut world = seam_test::world![fx::Now];
/// world.perform(seam_core::ops::time::Sleep(core::time::Duration::ZERO));
/// # }
/// # #[cfg(not(feature = "time"))] compile_error!("E0277");
/// ```
#[macro_export]
macro_rules! world {
    ($($key:ty),+ $(,)?) => {{
        #[derive(Debug, Clone, Copy)]
        struct Grant;
        $(impl $crate::sim::Grants<$key> for Grant {})+
        $crate::sim::World::<Grant>::empty()
            $(.with_state::<<$key as $crate::sim::Simulated>::State>())+
    }};
}

// ---- `Sim`: the world of every built-in effect ----

/// What [`Sim`] grants: every built-in effect whose feature is on.
#[derive(Debug, Clone, Copy)]
pub struct Everything;

macro_rules! grants_everything {
    ($($feature:literal: $($key:ty),+;)*) => {$($(
        #[cfg(feature = $feature)]
        impl Grants<$key> for Everything {}
    )+)*};
}

grants_everything! {
    "time": seam_core::effects::Now, seam_core::effects::Sleep, seam_core::effects::WallClock;
    "random": seam_core::effects::FillRandom;
    "env": seam_core::effects::Var, seam_core::effects::SetVar, seam_core::effects::RemoveVar,
        seam_core::effects::Args, seam_core::effects::CurrentDir, seam_core::effects::TempDir;
    "fs": seam_core::effects::Metadata, seam_core::effects::CreateDir,
        seam_core::effects::RemoveFile, seam_core::effects::RemoveDir, seam_core::effects::Rename,
        seam_core::effects::CopyFile, seam_core::effects::CreateAtomic, seam_core::effects::View,
        seam_core::effects::ReadDir;
    "net": seam_core::effects::ResolveInto, seam_core::effects::TcpConnect,
        seam_core::effects::TcpBind, seam_core::effects::UdpBind;
    "process": seam_core::effects::RunCommand, seam_core::effects::SpawnChild,
        seam_core::effects::Exit;
    "signal": seam_core::effects::CtrlC;
    "io-std": seam_core::effects::OpenStdin, seam_core::effects::OpenStdout,
        seam_core::effects::OpenStderr;
    "rt": seam_core::effects::AvailableParallelism;
    "sync": seam_core::effects::Channel, seam_core::effects::Share;
}

#[cfg(feature = "fs")]
impl<M> Grants<seam_core::effects::Open<M>> for Everything {}

/// The simulated world of every built-in effect: a [`World`] that grants
/// all of them, with every built-in state. The test backend's capability.
///
/// Create one per test. Set up the world it starts in (files, variables,
/// scripted commands, stdin), hand it to the core in place of a real
/// backend, then assert on what the core left behind. Every clone is the
/// same world, so a part of a root that works over the simulation is given
/// a clone, and the test asserts through any of them:
///
/// ```
/// # #[cfg(feature = "fs")] {
/// let sim = seam_test::Sim::new();
/// let mut other = sim.clone();
/// other.add_file("/tmp/a", "hi");
/// assert_eq!(sim.file("/tmp/a").unwrap(), b"hi");
/// # }
/// ```
pub type Sim = World<Everything>;

impl World<Everything> {
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
        #[allow(unused_mut)]
        let mut sim = Self::empty();
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
}

impl Default for World<Everything> {
    fn default() -> Self {
        Self::new()
    }
}
