//! rand for seam: any capability that performs `FillRandom` as a rand RNG.
//!
//! rand's distributions, ranges, shuffles and samplers take an RNG. This
//! crate makes seam's randomness one, so a core keeps its randomness in its
//! bounds (`Perform<FillRandom>`) and still uses all of rand: on `Std` it
//! is the system's randomness, on a `Sim` it is seeded and the same every
//! run.
//!
//! - [`rng`] borrows a capability as an RNG: each draw is a `FillRandom`.
//! - [`seeded`] seeds one of rand's own generators from the capability,
//!   with a single `FillRandom`, and then draws without effects: faster
//!   for many draws, and still reproducible under a `Sim`.
//!
//! ```
//! use rand::RngExt;
//! use rand::seq::SliceRandom;
//! use seam_core::Perform;
//! use seam_core::ops::random::FillRandom;
//!
//! /// Deals five cards and rolls a die.
//! fn deal<C: for<'a> Perform<FillRandom<'a>>>(c: &mut C) -> (Vec<u8>, u8) {
//!     let mut rng = seam_rand::rng(c);
//!     let mut deck: Vec<u8> = (1..=52).collect();
//!     deck.shuffle(&mut rng);
//!     deck.truncate(5);
//!     (deck, rng.random_range(1..=6))
//! }
//!
//! let mut a = seam_test::Sim::with_seed(9);
//! let mut b = seam_test::Sim::with_seed(9);
//! assert_eq!(deal(&mut a), deal(&mut b)); // seeded: the same every run
//! ```
//!
//! The RNG is not marked `CryptoRng`: whether seam's randomness is fit for
//! cryptography depends on the backend (a `Sim`'s is not).
//!
//! Cores should not call rand's `rng()`, `random()` or `make_rng()`, which
//! read the system directly; seam's `templates/clippy.toml` bans them.
#![forbid(unsafe_code)]
#![no_std]

use core::convert::Infallible;

use rand::TryRng;
use seam_core::Perform;
use seam_core::ops::random::FillRandom;

/// A capability that performs `FillRandom`, borrowed as a rand RNG: it
/// implements `rand::Rng` (through `TryRng` that cannot fail), so it works
/// with `rand::RngExt` and everything that takes an RNG. Made by [`rng`].
#[derive(Debug)]
pub struct RngOf<'a, C: ?Sized>(pub &'a mut C);

/// Borrows `c` as a rand RNG. Every draw performs `FillRandom`.
pub fn rng<C>(c: &mut C) -> RngOf<'_, C>
where
    C: for<'a> Perform<FillRandom<'a>> + ?Sized,
{
    RngOf(c)
}

impl<C> TryRng for RngOf<'_, C>
where
    C: for<'a> Perform<FillRandom<'a>> + ?Sized,
{
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        let mut bytes = [0; 4];
        self.0.perform(FillRandom(&mut bytes));
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let mut bytes = [0; 8];
        self.0.perform(FillRandom(&mut bytes));
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.perform(FillRandom(dst));
        Ok(())
    }
}

/// One of rand's generators (`R`), seeded from `c` with one `FillRandom`.
/// After that it draws with no effects at all; under a `Sim` its numbers
/// follow from the `Sim`'s seed. With the `std_rng` feature,
/// `seeded::<rand::rngs::StdRng, _>(c)`.
pub fn seeded<R, C>(c: &mut C) -> R
where
    R: rand::SeedableRng,
    C: for<'a> Perform<FillRandom<'a>> + ?Sized,
{
    let mut seed = R::Seed::default();
    c.perform(FillRandom(seed.as_mut()));
    R::from_seed(seed)
}
