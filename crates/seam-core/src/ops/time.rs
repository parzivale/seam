//! Clocks and waiting.
//!
//! [`Sleep`], [`Now`] and [`Instant`] need no std. `WallClock` produces a
//! `SystemTime` and needs the `std` feature.
//!
//! [`Timeout`] wraps an operation that waits, such as a receive, a read or
//! a join, to give up after a while.
//!
//! Built from `Now` and `Sleep`: [`sleep_until`] a deadline, [`elapsed`]
//! since an instant, and [`tick`], which paces a loop by an [`Interval`]
//! without drifting.
//!
//! A core that takes its clock as a capability can be tested without
//! waiting:
//!
//! ```
//! use core::time::Duration;
//! use seam_core::{Inspect, Perform};
//! use seam_core::ops::time::{Instant, Now, Sleep};
//!
//! /// Polls `ready` every second, up to `timeout`. Returns whether it became
//! /// ready.
//! fn wait_until<C>(c: &mut C, timeout: Duration, mut ready: impl FnMut() -> bool) -> bool
//! where
//!     C: Inspect<Now> + Perform<Sleep>,
//! {
//!     let start = c.inspect(Now);
//!     while c.inspect(Now) - start < timeout {
//!         if ready() {
//!             return true;
//!         }
//!         c.perform(Sleep(Duration::from_secs(1)));
//!     }
//!     false
//! }
//!
//! /// A virtual clock: sleeping moves it forward instantly.
//! struct Clock(Instant);
//! impl Inspect<Now> for Clock {
//!     fn inspect(&self, _: Now) -> Instant {
//!         self.0
//!     }
//! }
//! impl Perform<Sleep> for Clock {
//!     fn perform(&mut self, op: Sleep) {
//!         self.0 += op.0;
//!     }
//! }
//!
//! let mut clock = Clock(Instant::EPOCH);
//! assert!(!wait_until(&mut clock, Duration::from_secs(30), || false));
//! assert_eq!(clock.0.since_epoch(), Duration::from_secs(30)); // in no time
//! ```

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

/// Perform `op`, but give up waiting after `after`.
///
/// Its outcome is `op`'s own: an expiry is reported the way `op` reports
/// not being able to finish, as its [`TimesOut`] impl says. A receive that
/// times out fails with `RecvError::Empty`, a send with `SendError::Full`
/// (handing the value back), and an I/O operation with
/// `io::ErrorKind::TimedOut`. An operation that expires had no effect.
///
/// `Timeout` wraps primitives only. A helper built from several (such as
/// `read_exact`) has no timed form; a core that needs one times each
/// primitive it performs, against a deadline it keeps.
///
/// `Timeout` also wraps acquisitions that wait, such as `Accept` and
/// `TcpConnect`, failing them with their own error.
///
/// A timeout of zero does not wait at all: the operation finishes only if
/// it can do so at once.
///
/// ```
/// # #[cfg(feature = "sync")] {
/// use core::time::Duration;
/// use seam_core::Perform;
/// use seam_core::ops::sync::{RecvError, RecvMsg};
/// use seam_core::ops::time::Timeout;
///
/// /// Waits up to a second for the next job, so the worker can check in
/// /// between whether it should stop.
/// fn next_job<R>(jobs: &mut R) -> Option<u32>
/// where
///     R: Perform<Timeout<RecvMsg<u32>>>,
/// {
///     match jobs.perform(Timeout::new(Duration::from_secs(1), RecvMsg::new())) {
///         Ok(job) => Some(job),
///         Err(RecvError::Empty | RecvError::Closed) => None,
///     }
/// }
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout<O> {
    /// How long to wait at most.
    pub after: Duration,
    /// The operation or acquisition.
    pub op: O,
}

impl<O> Timeout<O> {
    /// Describes performing `op`, waiting at most `after`.
    pub const fn new(after: Duration, op: O) -> Self {
        Self { after, op }
    }
}

/// An operation or acquisition that can wait, and so can be given a
/// [`Timeout`]. `timed_out` gives the outcome (for an acquisition, the
/// error) that reports giving up.
///
/// Backends call it to build what an expired `Timeout` returns; cores
/// do not need it.
pub trait TimesOut {
    /// What an expiry produces: the operation's outcome, or the
    /// acquisition's error.
    type Expired;

    /// The outcome of giving up, built from the operation, which did not
    /// run to completion.
    fn timed_out(self) -> Self::Expired;
}

impl<O> crate::Operation for Timeout<O>
where
    O: crate::Operation + TimesOut<Expired = O::Outcome>,
{
    type Outcome = O::Outcome;
}

impl<A> crate::Acquisition for Timeout<A>
where
    A: crate::Acquisition + TimesOut<Expired = A::Error>,
{
    type Resource = A::Resource;
    type Error = A::Error;
}

/// The `io::Error` an I/O operation that times out fails with.
#[allow(dead_code)] // used by whichever of io, net and signal are on
pub(crate) fn io_timed_out() -> crate::io::Error {
    crate::io::ErrorKind::TimedOut.into()
}

operations! {
    Sleep => ();
    #[query] Now => Instant;
}

#[cfg(feature = "std")]
operations! {
    #[query] WallClock => std::time::SystemTime;
}

crate::root::keys!(
    [] Sleep => Sleep,
    [] Now => Now,
);
#[cfg(feature = "std")]
crate::root::keys!(
    [] WallClock => WallClock,
);

/// A timed operation is served by whatever serves the operation.
impl<O: crate::Effect> crate::Effect for Timeout<O> {
    type Key = O::Key;
}

/// A deadline every `period`, from a start: what [`tick`] waits for. The
/// deadlines are `start + n·period`, so a loop paced by it does not drift
/// however long each turn takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    next: Instant,
    period: Duration,
}

impl Interval {
    /// Deadlines every `period`, the first at `start`. Pass `Now` for the
    /// first tick to be at once.
    ///
    /// # Panics
    ///
    /// If `period` is zero.
    pub fn new(start: Instant, period: Duration) -> Self {
        assert!(!period.is_zero(), "an `Interval`'s period must not be zero");
        Self {
            next: start,
            period,
        }
    }

    /// The next deadline.
    pub fn next(&self) -> Instant {
        self.next
    }

    /// The time between deadlines.
    pub fn period(&self) -> Duration {
        self.period
    }
}

crate::three_ways::three_ways! {
    /// Sleeps until `deadline`, by the monotonic clock. Returns at once if it
    /// has passed.
    pub fn sleep_until, sleep_until_async, sleep_until_send
    [C] (c: &mut C, deadline: Instant) -> ()
    where
        sync { C: crate::Inspect<Now> + crate::Perform<Sleep> }
        async { C: crate::future::AsyncInspect<Now> + crate::future::AsyncPerform<Sleep> }
        send { C: crate::future::SendInspect<Now> + crate::future::SendPerform<Sleep> + Send }
    {
        let now = ask!(c, Now);
        if let Some(wait) = deadline.checked_duration_since(now).filter(|d| !d.is_zero()) {
            run!(c, Sleep(wait));
        }
    }

    /// How long it has been since `since`, by the monotonic clock: zero if
    /// `since` is later.
    pub fn elapsed, elapsed_async, elapsed_send
    [C] (c: &C, since: Instant) -> Duration
    where
        sync { C: crate::Inspect<Now> }
        async { C: crate::future::AsyncInspect<Now> }
        send { C: crate::future::SendInspect<Now> + Sync }
    {
        ask!(c, Now).duration_since(since)
    }

    /// Waits for the interval's next deadline, and returns it. If deadlines
    /// were missed (the loop ran long), they are skipped rather than run in
    /// a burst: the next one is the first still to come.
    pub fn tick, tick_async, tick_send
    [C] (c: &mut C, interval: &mut Interval) -> Instant
    where
        sync { C: crate::Inspect<Now> + crate::Perform<Sleep> }
        async { C: crate::future::AsyncInspect<Now> + crate::future::AsyncPerform<Sleep> }
        send { C: crate::future::SendInspect<Now> + crate::future::SendPerform<Sleep> + Send }
    {
        let deadline = interval.next;
        call!(sleep_until, sleep_until_async, sleep_until_send; (c, deadline));
        let now = ask!(c, Now);
        let mut next = deadline + interval.period;
        if next <= now {
            let behind = now.duration_since(next);
            let skipped = behind.as_nanos() / interval.period.as_nanos() + 1;
            next += interval.period * u32::try_from(skipped).unwrap_or(u32::MAX);
        }
        interval.next = next;
        deadline
    }
}
