use std::sync::OnceLock;
use std::time::SystemTime;

use seam_core::Inspect;
use seam_core::ops::time::{Instant, Now, Sleep, WallClock};

use crate::Tokio;

/// The clock's epoch: the first time anything in this process read it.
static EPOCH: OnceLock<tokio::time::Instant> = OnceLock::new();

// Tokio's clock, so that paused time (`test-util`) is respected.
impl Inspect<Now> for Tokio {
    fn inspect(&self, _: Now) -> Instant {
        Instant::from_epoch(EPOCH.get_or_init(tokio::time::Instant::now).elapsed())
    }
}

impl Inspect<WallClock> for Tokio {
    fn inspect(&self, _: WallClock) -> SystemTime {
        SystemTime::now()
    }
}

async_op!(perform [] Tokio, Sleep, |_s, op| tokio::time::sleep(op.0).await);
