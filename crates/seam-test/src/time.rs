use core::time::Duration;

use seam_core::ops::time::{Instant, Now, Sleep};
use seam_core::{Inspect, Perform};

use crate::Sim;

impl Sim {
    /// Moves both clocks forward, as if `d` passed outside the core.
    pub fn advance(&mut self, d: Duration) {
        #[allow(unused_mut)] // a guard with std, a reference without
        let mut world = self.state_mut();
        world.clock += d;
        #[cfg(feature = "std")]
        {
            world.wall += d;
        }
    }

    /// The monotonic clock, since `Instant::EPOCH`.
    #[cfg(feature = "rt")]
    pub(crate) fn clock(&self) -> Duration {
        self.state().clock
    }

    /// Sets the wall clock. The monotonic clock is unaffected.
    #[cfg(feature = "std")]
    pub fn set_wall_clock(&mut self, time: std::time::SystemTime) {
        self.world.lock().wall = time;
    }
}

impl Inspect<Now> for Sim {
    fn inspect(&self, _: Now) -> Instant {
        Instant::from_epoch(self.state().clock)
    }
}

#[cfg(feature = "std")]
impl Inspect<seam_core::ops::time::WallClock> for Sim {
    fn inspect(&self, _: seam_core::ops::time::WallClock) -> std::time::SystemTime {
        self.world.lock().wall
    }
}

/// Advances the virtual clocks and returns at once.
impl Perform<Sleep> for Sim {
    fn perform(&mut self, op: Sleep) {
        self.advance(op.0);
    }
}
