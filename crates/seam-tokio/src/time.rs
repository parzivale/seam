use std::time::{Instant, SystemTime};

use seam_core::Inspect;
use seam_core::ops::time::{Now, Sleep, WallClock};

use crate::Tokio;

// Tokio's clock, so that paused time (`test-util`) is respected.
impl Inspect<Now> for Tokio {
    fn inspect(&self, _: Now) -> Instant {
        tokio::time::Instant::now().into_std()
    }
}

impl Inspect<WallClock> for Tokio {
    fn inspect(&self, _: WallClock) -> SystemTime {
        SystemTime::now()
    }
}

async_op!(perform [] Tokio, Sleep, |_s, op| tokio::time::sleep(op.0).await);
