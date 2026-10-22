use seam_core::Perform;
use seam_core::ops::random::{FillRandom, RandomU64};

use crate::Sim;

/// SplitMix64: small, fast, and the same everywhere. Not cryptographic.
#[derive(Debug, Clone)]
pub(crate) struct SplitMix64(pub(crate) u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl Perform<RandomU64> for Sim {
    fn perform(&mut self, _: RandomU64) -> u64 {
        self.rng.next()
    }
}

impl Perform<FillRandom<'_>> for Sim {
    fn perform(&mut self, op: FillRandom<'_>) {
        for chunk in op.0.chunks_mut(8) {
            let bytes = self.rng.next().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}
