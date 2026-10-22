use std::sync::OnceLock;
use std::time::SystemTime;

use seam_core::ops::time::{Instant, Now, Sleep, WallClock};
use seam_core::{Inspect, Perform};

use crate::Std;

/// The clock's epoch: the first time anything in this process read it.
static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

impl Inspect<Now> for Std {
    fn inspect(&self, _: Now) -> Instant {
        Instant::from_epoch(EPOCH.get_or_init(std::time::Instant::now).elapsed())
    }
}

impl Inspect<WallClock> for Std {
    fn inspect(&self, _: WallClock) -> SystemTime {
        SystemTime::now()
    }
}

impl Perform<Sleep> for Std {
    fn perform(&mut self, op: Sleep) {
        std::thread::sleep(op.0);
    }
}
