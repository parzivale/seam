//! Clocks and waiting.
//!
//! [`Sleep`], [`Now`] and [`Instant`] need no std. [`WallClock`] produces a
//! `SystemTime` and needs the `std` feature.

use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

/// Wait for a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sleep(pub Duration);

/// Read the monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct Now;

/// Read the system (wall-clock) time.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

/// A point on a backend's monotonic clock.
///
/// It is a duration since an epoch the backend picks (process start for
/// `seam-std`, zero for `seam-test`), so instants are only comparable with
/// others from the same backend in the same run. Unlike
/// `std::time::Instant`, it can be constructed without reading a clock,
/// which is what lets test fakes produce one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Instant(Duration);

impl Instant {
    /// The backend's epoch.
    pub const EPOCH: Self = Self(Duration::ZERO);

    /// The instant `since` after the backend's epoch.
    pub const fn from_epoch(since: Duration) -> Self {
        Self(since)
    }

    /// How long after the backend's epoch this is.
    pub const fn since_epoch(self) -> Duration {
        self.0
    }

    /// The time from `earlier` to `self`, or zero if `earlier` is later.
    pub fn duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// The time from `earlier` to `self`, or `None` if `earlier` is later.
    pub fn checked_duration_since(self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// `self + d`, or `None` on overflow.
    pub fn checked_add(self, d: Duration) -> Option<Self> {
        self.0.checked_add(d).map(Self)
    }

    /// `self - d`, or `None` if that is before the epoch.
    pub fn checked_sub(self, d: Duration) -> Option<Self> {
        self.0.checked_sub(d).map(Self)
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    /// # Panics
    ///
    /// On overflow.
    fn add(self, d: Duration) -> Self {
        self.checked_add(d)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, d: Duration) {
        *self = *self + d;
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;

    /// # Panics
    ///
    /// If the result would be before the epoch.
    fn sub(self, d: Duration) -> Self {
        self.checked_sub(d)
            .expect("instant would be before the epoch")
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, d: Duration) {
        *self = *self - d;
    }
}

impl Sub for Instant {
    type Output = Duration;

    /// Saturates to zero, like `std::time::Instant`.
    fn sub(self, earlier: Self) -> Duration {
        self.duration_since(earlier)
    }
}

operations! {
    Sleep => ();
    #[query] Now => Instant;
}

#[cfg(feature = "std")]
operations! {
    #[query] WallClock => std::time::SystemTime;
}
