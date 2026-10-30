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
//! # Where the randomness comes from
//!
//! Anything that performs `FillRandom`: `Std` (the operating system's
//! randomness) in a program, a seeded `Sim` in tests, or a root with either
//! as a part. Tokio and smol have no randomness of their own; a program on
//! them puts `Std` in its root for it.
//!
//! `FillRandom` never waits, so the RNG is sync even in async code: an
//! async core bounds on `Perform<FillRandom>` (not its async form) and
//! calls [`rng`] as here. The root a Tokio or smol program builds with
//! `Std` in it performs both.
//!
//! # In tests
//! ```
//! use seam_rand::RngExt;
//! use seam_rand::seq::SliceRandom;
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
//! # In a program
//!
//! ```no_run
//! use seam_rand::RngExt;
//! use seam_core::effects as fx;
//! use seam_std::Std;
//!
//! let mut root = seam_core::root![Std<fx::FillRandom>];
//! let roll: u8 = seam_rand::rng(&mut root).random_range(1..=6);
//! // Or one generator, seeded once, for many draws.
//! let mut fast: seam_rand::rngs::StdRng = seam_rand::seeded(&mut root);
//! let coin: bool = fast.random();
//! println!("{roll} {coin}");
//! ```
//!
//! Through the `seam` crate, this crate is `seam::rand` (feature `rand`).
//!
//! # Notes
//! The RNG is not marked `CryptoRng`: whether seam's randomness is fit for
//! cryptography depends on the backend (a `Sim`'s is not).
//!
//! Cores should not call rand's `rng()`, `random()` or `make_rng()`, which
//! read the system directly; seam's `templates/clippy.toml` bans them.
#![forbid(unsafe_code)]
#![no_std]

/// rand's API, all of it pure: built without `thread_rng` and `sys_rng`,
/// rand has nothing here that reads the system. Draw from [`rng`] or
/// [`seeded`] instead of `rand::rng()`.
pub use rand::{
    CryptoRng, Fill, Rng, RngExt, SeedableRng, TryCryptoRng, TryRng, distr, prelude, rand_core,
    rngs, seq,
};

use core::convert::Infallible;

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
