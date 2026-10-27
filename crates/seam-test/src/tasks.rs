//! What the simulated runtime and threads report about the machine.

use core::num::NonZeroUsize;
use std::vec::Vec;

use seam_core::effects as fx;
use seam_core::ops::task::AvailableParallelism;

use crate::sim::{SimState, Simulate, Simulated, States, World};
use crate::world_file::{self, Node};

/// The machine tasks run on: how many threads it reports.
///
/// Its block is `parallelism N`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tasks {
    /// What `AvailableParallelism` reports. The default is 1.
    pub parallelism: NonZeroUsize,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            parallelism: NonZeroUsize::MIN,
        }
    }
}

impl<G> World<G> {
    /// Sets what `AvailableParallelism` reports. The default is 1.
    pub fn set_parallelism(&mut self, n: NonZeroUsize) -> &mut Self {
        self.lock().state::<Tasks>().parallelism = n;
        self
    }
}

impl SimState for Tasks {
    const BLOCKS: &'static [&'static str] = &["parallelism"];

    /// `parallelism N`, at least 1.
    fn load(&mut self, node: &Node) -> world_file::Result<()> {
        node.no_children()?;
        node.props_only(&[])?;
        node.arity(1, Some(1), "a number of threads")?;
        let n = node.args()[0].uint::<usize>("parallelism")?;
        self.parallelism = NonZeroUsize::new(n)
            .ok_or_else(|| node.args()[0].error("parallelism must be at least 1"))?;
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        std::vec![Node::new("parallelism").with_arg(self.parallelism.get())]
    }
}

impl Simulated for fx::AvailableParallelism {
    type State = Tasks;
}

impl Simulate<AvailableParallelism> for fx::AvailableParallelism {
    fn run(states: &mut States<'_>, _: AvailableParallelism) -> NonZeroUsize {
        states.state::<Tasks>().parallelism
    }
}
