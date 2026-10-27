//! Simulated effects, each defined on its effect's own key.
//!
//! An effect is simulated by implementing two sister traits on its key, the
//! type a root routes it by (`effects::Sleep` for `Sleep`, or your own
//! effect, which is usually its own key):
//!
//! - [`Simulated`] names the state the effect works on: `Clock` for
//!   `Now` and `Sleep`, `Mounts` for a program's `BindMount`.
//! - [`Simulate<O>`] runs an operation `O` against the world's states.
//!
//! The state itself implements [`SimState`]: the world-file blocks it
//! reads, and its contents as a world file. A [`World`] holds one state per
//! state type, so effects that share a state (`Now` and `Sleep`) share one
//! value, and a handler can read other states too (writing a file stamps it
//! with the clock).
//!
//! [`world!`](crate::world!) builds a world that grants the effects it
//! lists:
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
//! world.load("clock now=10").unwrap();
//! assert_eq!(nap(&mut world), Duration::from_secs(100));
//! world.check("clock now=100").unwrap();
//! assert_eq!(world.to_world(), "clock now=100 wall=90\n");
//! # }
//! ```
#![allow(clippy::disallowed_types)] // the states are shared with every clone of the world

use core::any::{Any, TypeId};
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
    fn run(states: &mut States, op: O) -> O::Outcome;
}

/// The effects a world grants: `world!` implements it for each key it
/// lists.
#[diagnostic::on_unimplemented(
    message = "the world does not grant `{K}`",
    label = "`{K}` is not listed",
    note = "list `{K}` in `world![..]`"
)]
pub trait Grants<K> {}

/// The world's states, one per type, in the order they were added.
pub struct States {
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

impl States {
    /// The state of type `S`, created fresh if the world has none yet.
    pub fn state<S: SimState>(&mut self) -> &mut S {
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

    /// The state reading the block `name`, as an index; two states reading
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

/// A simulated world: the states of the effects it grants, `G`. Build one
/// with [`world!`](crate::world!).
///
/// A world is a capability: a core takes `&mut world` wherever it takes a
/// backend or a root. Every clone is a handle to the same states.
pub struct World<G> {
    states: Arc<Mutex<States>>,
    grants: PhantomData<fn() -> G>,
}

impl<G> Clone for World<G> {
    fn clone(&self) -> Self {
        Self {
            states: Arc::clone(&self.states),
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
        Self {
            states: Arc::new(Mutex::new(States { list: Vec::new() })),
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

    fn lock(&self) -> MutexGuard<'_, States> {
        self.states.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` on the state `S`: for setting the world up, or checking
    /// it, from a test.
    pub fn state<S: SimState, R>(&self, f: impl FnOnce(&mut S) -> R) -> R {
        f(self.lock().state::<S>())
    }

    /// Sets the world up further from a world file: each block goes to the
    /// state that reads it. On an error, the blocks before it have been
    /// applied.
    pub fn load(&mut self, text: &str) -> Result<&mut Self, WorldError> {
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
    /// Every failing expectation is reported, with its line.
    pub fn check(&self, text: &str) -> Result<(), WorldMismatch> {
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

    /// The whole world, as a world file.
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
        <Q::Key as Simulate<Q>>::run(&mut self.lock(), query)
    }
}

impl<G, O> Perform<O> for World<G>
where
    O: Operation + Effect,
    O::Key: Simulate<O>,
    G: Grants<O::Key>,
{
    fn perform(&mut self, op: O) -> O::Outcome {
        <O::Key as Simulate<O>>::run(&mut self.lock(), op)
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

// ---- the clock ----

/// The simulated clocks: a monotonic one, from `Instant::EPOCH`, and a wall
/// clock, from the Unix epoch. `Sleep` advances both.
///
/// Its block is `clock now=S wall=S`, in seconds; a time that is not a
/// whole number of seconds is written in nanoseconds, `now-ns=N`.
#[cfg(feature = "time")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clock {
    /// Time since `Instant::EPOCH`.
    pub now: core::time::Duration,
    /// Time since the Unix epoch, on the wall clock.
    pub wall: core::time::Duration,
}

#[cfg(feature = "time")]
impl Default for Clock {
    fn default() -> Self {
        Self {
            now: core::time::Duration::ZERO,
            wall: core::time::Duration::ZERO,
        }
    }
}

#[cfg(feature = "time")]
impl Clock {
    /// Moves both clocks forward, as if `d` passed.
    pub fn advance(&mut self, d: core::time::Duration) {
        self.now += d;
        self.wall += d;
    }
}

#[cfg(feature = "time")]
fn duration_prop(node: Node, key: &str, d: core::time::Duration) -> Node {
    if d.subsec_nanos() == 0 {
        node.with_prop(key, d.as_secs())
    } else {
        node.with_prop(format!("{key}-ns"), d.as_nanos() as i128)
    }
}

#[cfg(feature = "time")]
fn read_duration(node: &Node, key: &str) -> world_file::Result<Option<core::time::Duration>> {
    use core::time::Duration;
    if let Some(v) = node.prop(key) {
        return Ok(Some(Duration::from_secs(v.uint(&format!("`{key}`"))?)));
    }
    match node.prop(&format!("{key}-ns")) {
        Some(v) => Ok(Some(Duration::from_nanos(v.uint(&format!("`{key}-ns`"))?))),
        None => Ok(None),
    }
}

#[cfg(feature = "time")]
impl SimState for Clock {
    const BLOCKS: &'static [&'static str] = &["clock"];

    /// `clock now=S wall=S`: advances the clock by `now`, then sets the
    /// wall clock to `wall` seconds after the Unix epoch.
    fn load(&mut self, node: &Node) -> world_file::Result<()> {
        node.no_children()?;
        node.arity(0, Some(0), "only `now=` and `wall=`")?;
        node.props_only(&["now", "now-ns", "wall", "wall-ns"])?;
        if let Some(d) = read_duration(node, "now")? {
            self.now += d;
        }
        if let Some(d) = read_duration(node, "wall")? {
            self.wall = d;
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let node = duration_prop(Node::new("clock"), "now", self.now);
        std::vec![duration_prop(node, "wall", self.wall)]
    }
}

#[cfg(feature = "time")]
mod time_effects {
    use seam_core::effects as fx;
    use seam_core::ops::time::{Instant, Now, Sleep};

    use super::{Clock, Simulate, Simulated, States};

    impl Simulated for fx::Now {
        type State = Clock;
    }

    impl Simulate<Now> for fx::Now {
        fn run(states: &mut States, _: Now) -> Instant {
            Instant::from_epoch(states.state::<Clock>().now)
        }
    }

    impl Simulated for fx::Sleep {
        type State = Clock;
    }

    impl Simulate<Sleep> for fx::Sleep {
        fn run(states: &mut States, op: Sleep) {
            states.state::<Clock>().advance(op.0);
        }
    }

    #[cfg(feature = "std")]
    impl Simulated for fx::WallClock {
        type State = Clock;
    }

    #[cfg(feature = "std")]
    impl Simulate<seam_core::ops::time::WallClock> for fx::WallClock {
        fn run(states: &mut States, _: seam_core::ops::time::WallClock) -> std::time::SystemTime {
            std::time::UNIX_EPOCH + states.state::<Clock>().wall
        }
    }
}
