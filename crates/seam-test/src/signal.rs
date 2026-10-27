use seam_core::io::{self, ErrorKind};
use std::vec::Vec;

use seam_core::effects as fx;
use seam_core::ops::signal::CtrlC;

use crate::error;
use crate::sim::{SimState, Simulate, Simulated, States, World};
use crate::world_file::{self, Node};

/// The simulated signals: Ctrl-Cs raised and not yet waited for.
///
/// Its block is `signal { ctrl-c [N] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    /// Ctrl-Cs raised and not yet taken by a `CtrlC` wait.
    pub ctrl_c: u32,
}

impl Signals {
    /// Takes one raised Ctrl-C, if there is one.
    pub fn take_ctrl_c(&mut self) -> bool {
        let raised = self.ctrl_c > 0;
        self.ctrl_c -= u32::from(raised);
        raised
    }
}

impl<G> World<G> {
    /// Delivers a Ctrl-C. Each one satisfies one `CtrlC` wait.
    pub fn raise_ctrl_c(&mut self) -> &mut Self {
        self.lock().state::<Signals>().ctrl_c += 1;
        self
    }

    /// Takes one raised Ctrl-C, if there is one.
    #[cfg(feature = "rt")]
    pub(crate) fn take_ctrl_c(&mut self) -> bool {
        self.lock().state::<Signals>().take_ctrl_c()
    }
}

impl SimState for Signals {
    const BLOCKS: &'static [&'static str] = &["signal"];

    /// `ctrl-c [N]` raises `N` Ctrl-Cs, 1 by default.
    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for child in block.block()? {
            if child.name() != "ctrl-c" {
                return Err(child.unknown("`signal`", &["ctrl-c"]));
            }
            child.no_children()?;
            child.props_only(&[])?;
            child.arity(0, Some(1), "an optional count")?;
            self.ctrl_c += match child.args().first() {
                Some(n) => n.uint::<u32>("the count")?,
                None => 1,
            };
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("signal");
        if self.ctrl_c > 0 {
            block = block.with_child(Node::new("ctrl-c").with_arg(self.ctrl_c));
        }
        std::vec![block]
    }
}

impl Simulated for fx::CtrlC {
    type State = Signals;
}

/// Succeeds if a Ctrl-C was raised, and fails with `WouldBlock` otherwise.
impl Simulate<CtrlC> for fx::CtrlC {
    fn run(states: &mut States<'_>, _: CtrlC) -> io::Result<()> {
        if states.state::<Signals>().take_ctrl_c() {
            Ok(())
        } else {
            Err(error(
                ErrorKind::WouldBlock,
                "no Ctrl-C raised, and nothing in the simulation waits",
            ))
        }
    }
}

/// Nothing in a world waits, so a timed wait with no Ctrl-C raised expires
/// at once.
#[cfg(feature = "time")]
impl Simulate<seam_core::ops::time::Timeout<CtrlC>> for fx::CtrlC {
    fn run(states: &mut States<'_>, op: seam_core::ops::time::Timeout<CtrlC>) -> io::Result<()> {
        crate::timeouts::expired(<fx::CtrlC as Simulate<CtrlC>>::run(states, op.op))
    }
}
