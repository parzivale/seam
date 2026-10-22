use std::time::{Instant, SystemTime};

use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{Inspect, Perform};

use crate::Std;

impl Inspect<Now> for Std {
    fn inspect(&self, _: Now) -> Instant {
        Instant::now()
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
