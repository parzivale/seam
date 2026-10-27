use std::sync::OnceLock;
use std::time::SystemTime;

use seam_core::Inspect;
use seam_core::ops::time::{Instant, Now, Sleep, WallClock};

use crate::Smol;

/// The clock's epoch: the first time anything in this process read it.
static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();

impl Inspect<Now> for Smol {
    fn inspect(&self, _: Now) -> Instant {
        Instant::from_epoch(EPOCH.get_or_init(std::time::Instant::now).elapsed())
    }
}

impl Inspect<WallClock> for Smol {
    fn inspect(&self, _: WallClock) -> SystemTime {
        SystemTime::now()
    }
}

async_op!(perform [] Smol, Sleep, |_s, op| {
    smol::Timer::after(op.0).await;
});
