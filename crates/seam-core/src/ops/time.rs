//! Clocks and waiting.
//!
//! [`Sleep`] needs no std. [`Now`] and [`WallClock`] produce std types and
//! need the `std` feature.

use core::time::Duration;

/// Wait for a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sleep(pub Duration);

/// Read the monotonic clock.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Now;

/// Read the system (wall-clock) time.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

operations! {
    Sleep => ();
}

#[cfg(feature = "std")]
operations! {
    #[query] Now => std::time::Instant;
    #[query] WallClock => std::time::SystemTime;
}
