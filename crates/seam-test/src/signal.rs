use seam_core::io::{self, ErrorKind};

use seam_core::Perform;
use seam_core::ops::signal::CtrlC;

use crate::{Sim, error};

impl Sim {
    /// Delivers a Ctrl-C. Each one satisfies one `CtrlC` wait.
    pub fn raise_ctrl_c(&mut self) -> &mut Self {
        self.world.lock().ctrl_c += 1;
        self
    }

    /// Takes one raised Ctrl-C, if there is one.
    #[cfg(feature = "rt")]
    pub(crate) fn take_ctrl_c(&mut self) -> bool {
        let mut world = self.world.lock();
        let raised = world.ctrl_c > 0;
        world.ctrl_c -= u32::from(raised);
        raised
    }
}

/// Succeeds if a Ctrl-C was raised, and fails with `WouldBlock` otherwise.
impl Perform<CtrlC> for Sim {
    fn perform(&mut self, _: CtrlC) -> io::Result<()> {
        let mut world = self.world.lock();
        if world.ctrl_c == 0 {
            return Err(error(
                ErrorKind::WouldBlock,
                "no Ctrl-C raised, and nothing in the simulation waits",
            ));
        }
        world.ctrl_c -= 1;
        Ok(())
    }
}
