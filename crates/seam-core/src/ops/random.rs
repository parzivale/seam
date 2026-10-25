//! Randomness.
//!
//! The primitive is [`FillRandom`]; [`random_u64`] is built on it.
//!
//! ```
//! use seam_core::{Capability, Perform};
//! use seam_core::ops::random::{FillRandom, random_u64};
//!
//! /// Picks one item at random.
//! fn pick<'a, T, C>(c: &mut C, items: &'a [T]) -> Option<&'a T>
//! where
//!     C: for<'b> Perform<FillRandom<'b>>,
//! {
//!     if items.is_empty() {
//!         return None;
//!     }
//!     let i = random_u64(c) % items.len() as u64;
//!     items.get(i as usize)
//! }
//!
//! /// "Random" in a test means chosen by the test.
//! struct Always(u8);
//! impl Capability for Always {}
//! impl Perform<FillRandom<'_>> for Always {
//!     fn perform(&mut self, op: FillRandom<'_>) {
//!         op.0.fill(self.0);
//!     }
//! }
//!
//! assert_eq!(pick(&mut Always(0), &["a", "b", "c"]), Some(&"a"));
//! ```

use crate::three_ways::three_ways;

/// Fill the buffer with random bytes.
#[derive(Debug)]
pub struct FillRandom<'a>(pub &'a mut [u8]);

operations! {
    FillRandom<'_> => ();
}

crate::root::keys!(
    ['a] FillRandom<'a> => FillRandom,
);

three_ways! {
    /// A random `u64`: eight random bytes, little-endian.
    pub fn random_u64, random_u64_async, random_u64_send
    [C] (c: &mut C) -> u64
    where
        sync { C: for<'a> crate::Perform<FillRandom<'a>> }
        async { C: for<'a> crate::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> crate::future::SendPerform<FillRandom<'a>> + Send }
    {
        let mut bytes = [0; 8];
        run!(c, FillRandom(&mut bytes));
        u64::from_le_bytes(bytes)
    }
}
