//! Sans-effect programming.
//!
//! Sans-IO pulls I/O out of protocol logic. `seam` pulls out **every**
//! effect: I/O, time, randomness, shared state. The core of a program is
//! written against capability bounds ([`Inspect`], [`Perform`]) and can be
//! unit tested, or run, with no effects at all. Real implementations or test
//! fakes are plugged in at the edge: the seam.
//!
//! A function's bounds list everything it is able to do:
//!
//! ```
//! use seam::{Inspect, Perform};
//! use seam::ops::random::RandomU64;
//! use seam::ops::time::Sleep;
//! use core::time::Duration;
//!
//! /// Waits a random 0-99 ms. It can sleep and draw random numbers, and
//! /// nothing else.
//! fn jitter<C: Perform<RandomU64> + Perform<Sleep>>(c: &mut C) {
//!     let ms = c.perform(RandomU64) % 100;
//!     c.perform(Sleep(Duration::from_millis(ms)));
//! }
//!
//! // A test fake: no real clock, no real randomness.
//! #[derive(Default)]
//! struct Fake { slept: Duration }
//! impl Perform<RandomU64> for Fake {
//!     fn perform(&mut self, _: RandomU64) -> u64 { 142 }
//! }
//! impl Perform<Sleep> for Fake {
//!     fn perform(&mut self, op: Sleep) { self.slept += op.0; }
//! }
//!
//! let mut fake = Fake::default();
//! jitter(&mut fake);
//! assert_eq!(fake.slept, Duration::from_millis(42));
//! ```
//!
//! # Crates and features
//!
//! This crate re-exports the others. It is `no_std` with no default
//! features.
//!
//! | Feature | Gives you |
//! |---|---|
//! | `time`, `random` | operation types; no std needed |
//! | `fs`, `net`, `process`, `env`, `io-std` | operation types (pulls in std) |
//! | `std` | the std backend, [`stdlib`] |
//! | `tokio` | the Tokio backend, [`tokio`] |
//! | `signal`, `rt`, `rt-multi-thread`, `test-util` | Tokio-only, as in Tokio |
//! | `full` | everything |
//!
//! A backend implements exactly the capability features that are enabled.
#![no_std]

pub use seam_core::*;

/// The std backend.
#[cfg(feature = "std")]
pub use seam_std as stdlib;

/// The Tokio backend.
#[cfg(feature = "tokio")]
pub use seam_tokio as tokio;
