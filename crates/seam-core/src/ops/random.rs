//! Randomness.
//!
//! The primitive is [`FillRandom`]. Built on it: [`random_u64`],
//! [`random_range`] (without the bias of `%`), [`random_bool`], [`shuffle`]
//! and [`choose`].
//!
//! ```
//! use seam_core::{Capability, Perform};
//! use seam_core::ops::random::{FillRandom, random_range};
//!
//! /// Picks one item at random.
//! fn pick<'a, T, C>(c: &mut C, items: &'a [T]) -> Option<&'a T>
//! where
//!     C: for<'b> Perform<FillRandom<'b>>,
//! {
//!     if items.is_empty() {
//!         return None;
//!     }
//!     let i = random_range(c, 0..items.len() as u64);
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

    /// A random number in `range`, every value equally likely.
    ///
    /// # Panics
    ///
    /// If `range` is empty.
    pub fn random_range, random_range_async, random_range_send
    [C] (c: &mut C, range: core::ops::Range<u64>) -> u64
    where
        sync { C: for<'a> crate::Perform<FillRandom<'a>> }
        async { C: for<'a> crate::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> crate::future::SendPerform<FillRandom<'a>> + Send }
    {
        assert!(range.start < range.end, "`random_range` needs a non-empty range");
        let span = range.end - range.start;
        // Lemire's method: scale a random u64 into the span by multiplying,
        // and reject the few draws that would make some values likelier.
        let reject_below = span.wrapping_neg() % span;
        // A real source is rejected at most half the time, so 64 rejections
        // in a row only happen with a fake that repeats itself (one that
        // always gives zero); it gets a slightly biased answer rather than
        // a loop that never ends.
        let mut draw = 0;
        for _ in 0..64 {
            draw = call!(random_u64, random_u64_async, random_u64_send; (c));
            let product = u128::from(draw) * u128::from(span);
            if (product as u64) >= reject_below {
                return range.start + (product >> 64) as u64;
            }
        }
        range.start + draw % span
    }

    /// `true` or `false`, each half the time.
    pub fn random_bool, random_bool_async, random_bool_send
    [C] (c: &mut C) -> bool
    where
        sync { C: for<'a> crate::Perform<FillRandom<'a>> }
        async { C: for<'a> crate::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> crate::future::SendPerform<FillRandom<'a>> + Send }
    {
        call!(random_u64, random_u64_async, random_u64_send; (c)) & 1 == 1
    }

    /// Puts `items` in a random order, every order equally likely
    /// (Fisher-Yates).
    pub fn shuffle, shuffle_async, shuffle_send
    [C, T] (c: &mut C, items: &mut [T]) -> ()
    where
        sync { C: for<'a> crate::Perform<FillRandom<'a>> }
        async { C: for<'a> crate::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> crate::future::SendPerform<FillRandom<'a>> + Send, T: Send }
    {
        for i in (1..items.len()).rev() {
            let j = call!(random_range, random_range_async, random_range_send; (c, 0..i as u64 + 1));
            items.swap(i, j as usize);
        }
    }

    /// One of `items`, each equally likely, or `None` if there are none.
    pub fn choose, choose_async, choose_send
    ['s, C, T] (c: &mut C, items: &'s [T]) -> Option<&'s T>
    where
        sync { C: for<'a> crate::Perform<FillRandom<'a>> }
        async { C: for<'a> crate::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> crate::future::SendPerform<FillRandom<'a>> + Send, T: Sync }
    {
        if items.is_empty() {
            return None;
        }
        let i = call!(random_range, random_range_async, random_range_send; (c, 0..items.len() as u64));
        items.get(i as usize)
    }
}
