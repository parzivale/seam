use std::hash::{BuildHasher, Hasher, RandomState};

use seam_core::Perform;
use seam_core::ops::random::{FillRandom, RandomU64};

use crate::Std;

/// Draws 64 bits from std's randomly keyed SipHash.
///
/// std has no public random number generator; `RandomState` is seeded from
/// the OS once per thread and re-keyed on every `new`. This is fine for
/// jitter, sampling and hash seeds. It is not a cryptographic RNG.
fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

impl Perform<RandomU64> for Std {
    fn perform(&mut self, _: RandomU64) -> u64 {
        random_u64()
    }
}

impl Perform<FillRandom<'_>> for Std {
    fn perform(&mut self, op: FillRandom<'_>) {
        for chunk in op.0.chunks_mut(8) {
            let bytes = random_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}
