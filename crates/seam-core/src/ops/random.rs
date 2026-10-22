//! Randomness.
//!
//! ```
//! use seam_core::Perform;
//! use seam_core::ops::random::RandomU64;
//!
//! /// Picks one item at random.
//! fn pick<'a, T, C: Perform<RandomU64>>(c: &mut C, items: &'a [T]) -> Option<&'a T> {
//!     if items.is_empty() {
//!         return None;
//!     }
//!     let i = c.perform(RandomU64) % items.len() as u64;
//!     items.get(i as usize)
//! }
//!
//! /// "Random" in a test means chosen by the test.
//! struct Always(u64);
//! impl Perform<RandomU64> for Always {
//!     fn perform(&mut self, _: RandomU64) -> u64 {
//!         self.0
//!     }
//! }
//!
//! assert_eq!(pick(&mut Always(4), &["a", "b", "c"]), Some(&"b"));
//! ```

/// Produce a random `u64`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomU64;

/// Fill the buffer with random bytes.
#[derive(Debug)]
pub struct FillRandom<'a>(pub &'a mut [u8]);

operations! {
    RandomU64 => u64;
    FillRandom<'_> => ();
}

crate::root::in_kind!(crate::kinds::Random: [] RandomU64, ['a] FillRandom<'a>);
