//! uuid for seam: UUIDs from seam's randomness and clock.
//!
//! A random UUID needs randomness and a time-ordered one also needs the
//! wall clock; both are effects. uuid's own `Uuid::new_v4` and
//! `Uuid::now_v7` read the system for them. These helpers take them through
//! seam instead, so a core that makes IDs says so in its bounds, and under
//! a `Sim` the IDs are the same every run.
//!
//! - [`new_v4`]: random, from `FillRandom`.
//! - [`new_v7`]: time-ordered (sortable by creation time), from
//!   `WallClock` and `FillRandom`.
//!
//! Each comes in sync, `_async` and `_send` forms.
//!
//! # Where the randomness and clock come from
//!
//! `FillRandom` and `WallClock`: `Std` has both; Tokio and smol have
//! `WallClock` but no randomness, so a program on them puts `Std` in its
//! root (or uses `Std` for both). In tests, a seeded `Sim` with its wall
//! clock set makes the IDs the same every run.
//!
//! # In tests
//! ```
//! use seam_test::Sim;
//! use std::time::{Duration, UNIX_EPOCH};
//!
//! let mut sim = Sim::with_seed(4);
//! sim.set_wall_clock(UNIX_EPOCH + Duration::from_millis(1_700_000_000_000));
//! let id = seam_uuid::new_v7(&mut sim);
//! assert_eq!(id.get_version_num(), 7);
//! assert_eq!(id, seam_uuid::new_v7(&mut {
//!     let mut again = Sim::with_seed(4);
//!     again.set_wall_clock(UNIX_EPOCH + Duration::from_millis(1_700_000_000_000));
//!     again
//! }));
//! ```
//!
//! # In a program
//!
//! ```no_run
//! use seam_core::effects as fx;
//! use seam_std::Std;
//!
//! let mut root = seam_core::root![Std<fx::FillRandom, fx::WallClock>];
//! println!("{}", seam_uuid::new_v4(&mut root));
//!
//! // From async code: the `_send` form, here under Tokio.
//! let id = seam_tokio::block_on(seam_uuid::new_v7_send(&mut root));
//! println!("{id}");
//! ```
//!
//! Through the `seam` crate, this crate is `seam::uuid` (feature `uuid`).
//!
//! # Notes
//! Cores should not call `Uuid::new_v4` or `Uuid::now_v7`; seam's
//! `templates/clippy.toml` bans them.
#![forbid(unsafe_code)]

/// uuid's API, all of it pure: built without `std`, `rng` and `v4`, uuid
/// has nothing here that reads the clock or randomness. Its time-based
/// versions take a `timestamp::Timestamp`; [`new_v7`] gives one from
/// seam's clock.
pub use uuid::{Builder, Bytes, Error, NonNilUuid, Uuid, Variant, Version, fmt, timestamp, uuid};

use seam_core::ops::random::FillRandom;
use seam_core::ops::time::WallClock;

/// Milliseconds since the Unix epoch; 0 for a clock set before it.
fn unix_millis(now: std::time::SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Writes each helper in its sync, async and `Send` forms, from one body.
macro_rules! three_ways {
    ($(
        $(#[$m:meta])*
        pub fn $name:ident, $aname:ident, $sname:ident
        [$($g:tt)*] ($($args:tt)*) -> $ret:ty
        where
            sync { $($sb:tt)* }
            async { $($ab:tt)* }
            send { $($nb:tt)* }
        $body:block
    )*) => {$(
        $(#[$m])*
        pub fn $name<$($g)*>($($args)*) -> $ret where $($sb)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::Perform::perform($c, $op) };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::Inspect::inspect($c, $q) };
            }
            $body
        }

        #[doc = concat!("`", stringify!($name), "`, for async capabilities.")]
        pub async fn $aname<$($g)*>($($args)*) -> $ret where $($ab)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::AsyncPerform::perform_async($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::AsyncInspect::inspect_async($c, $q).await };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($name), "`, for async capabilities, with a `Send` future: ",
            "for multithreaded runtimes."
        )]
        pub async fn $sname<$($g)*>($($args)*) -> $ret where $($nb)* {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::SendPerform::perform_send($c, $op).await };
            }
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::SendInspect::inspect_send($c, $q).await };
            }
            $body
        }
    )*};
}

three_ways! {
    /// A random (version 4) UUID: 122 random bits, from one `FillRandom`.
    pub fn new_v4, new_v4_async, new_v4_send
    [C] (c: &mut C) -> Uuid
    where
        sync { C: for<'a> seam_core::Perform<FillRandom<'a>> }
        async { C: for<'a> seam_core::future::AsyncPerform<FillRandom<'a>> }
        send { C: for<'a> seam_core::future::SendPerform<FillRandom<'a>> + Send }
    {
        let mut bytes = [0; 16];
        run!(c, FillRandom(&mut bytes));
        Builder::from_random_bytes(bytes).into_uuid()
    }

    /// A time-ordered (version 7) UUID: the wall clock's milliseconds since
    /// the Unix epoch, then random bits. UUIDs made later sort later (those
    /// made in the same millisecond, in no particular order: [`V7`] keeps
    /// them in order).
    pub fn new_v7, new_v7_async, new_v7_send
    [C] (c: &mut C) -> Uuid
    where
        sync { C: seam_core::Inspect<WallClock> + for<'a> seam_core::Perform<FillRandom<'a>> }
        async {
            C: seam_core::future::AsyncInspect<WallClock>
                + for<'a> seam_core::future::AsyncPerform<FillRandom<'a>>,
        }
        send {
            C: seam_core::future::SendInspect<WallClock>
                + for<'a> seam_core::future::SendPerform<FillRandom<'a>>
                + Send
                + Sync,
        }
    {
        let millis = unix_millis(ask!(&*c, WallClock));
        let mut random = [0; 10];
        run!(c, FillRandom(&mut random));
        Builder::from_unix_timestamp_millis(millis, &random).into_uuid()
    }
}

/// Makes version 7 UUIDs that sort in the order they were made, even
/// within one millisecond, as uuid's own `Uuid::now_v7` does.
///
/// After the timestamp comes a 42-bit counter (RFC 9562, section 6.2,
/// method 1): set to a random value with its top bit clear at each new
/// millisecond, and incremented for each UUID within one. A clock that
/// steps back keeps the last millisecond, so order holds; a counter that
/// runs out moves on to the next millisecond. The last 32 bits are random.
///
/// It is plain state, so each generator orders its own UUIDs; share one
/// (behind a lock, across threads) for one order.
///
/// ```
/// use seam_test::Sim;
///
/// let mut sim = Sim::with_seed(1);
/// let mut ids = seam_uuid::V7::new();
/// let made: Vec<_> = (0..100).map(|_| ids.next(&mut sim)).collect();
/// // The simulated clock stood still: one millisecond, still in order.
/// assert!(made.windows(2).all(|w| w[0] < w[1]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V7 {
    /// The last UUID's millisecond and counter.
    last: Option<(u64, u64)>,
}

/// The counter's width, in bits.
const COUNTER_BITS: u32 = 42;

impl V7 {
    /// A generator that has made nothing yet.
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// The next UUID, at `millis`, from 10 random bytes.
    fn stamp(&mut self, millis: u64, random: [u8; 10]) -> Uuid {
        let seed = u64::from_be_bytes([
            0, 0, random[0], random[1], random[2], random[3], random[4], random[5],
        ]) & ((1 << (COUNTER_BITS - 1)) - 1);
        let (millis, counter) = match self.last {
            Some((last, counter)) if millis <= last => {
                if counter + 1 < 1 << COUNTER_BITS {
                    (last, counter + 1)
                } else {
                    (last + 1, seed)
                }
            }
            _ => (millis, seed),
        };
        self.last = Some((millis, counter));
        let ms = millis.to_be_bytes();
        let c = counter;
        let bytes = [
            ms[2],
            ms[3],
            ms[4],
            ms[5],
            ms[6],
            ms[7],
            0x70 | ((c >> 38) & 0x0f) as u8,
            (c >> 30) as u8,
            0x80 | ((c >> 24) & 0x3f) as u8,
            (c >> 16) as u8,
            (c >> 8) as u8,
            c as u8,
            random[6],
            random[7],
            random[8],
            random[9],
        ];
        Builder::from_bytes(bytes).into_uuid()
    }

    /// The next UUID, by `c`'s wall clock and randomness.
    pub fn next<C>(&mut self, c: &mut C) -> Uuid
    where
        C: seam_core::Inspect<WallClock> + for<'a> seam_core::Perform<FillRandom<'a>>,
    {
        let millis = unix_millis(c.inspect(WallClock));
        let mut random = [0; 10];
        c.perform(FillRandom(&mut random));
        self.stamp(millis, random)
    }

    /// [`next`](Self::next), for async capabilities.
    pub async fn next_async<C>(&mut self, c: &mut C) -> Uuid
    where
        C: seam_core::future::AsyncInspect<WallClock>
            + for<'a> seam_core::future::AsyncPerform<FillRandom<'a>>,
    {
        let millis = unix_millis(c.inspect_async(WallClock).await);
        let mut random = [0; 10];
        c.perform_async(FillRandom(&mut random)).await;
        self.stamp(millis, random)
    }

    /// [`next`](Self::next), for async capabilities, with a `Send` future:
    /// for multithreaded runtimes.
    pub async fn next_send<C>(&mut self, c: &mut C) -> Uuid
    where
        C: seam_core::future::SendInspect<WallClock>
            + for<'a> seam_core::future::SendPerform<FillRandom<'a>>
            + Send
            + Sync,
    {
        let millis = unix_millis(c.inspect_send(WallClock).await);
        let mut random = [0; 10];
        c.perform_send(FillRandom(&mut random)).await;
        self.stamp(millis, random)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_is_a_v7_uuid() {
        let mut ids = V7::new();
        let id = ids.stamp(0x0123_4567_89ab, [0xff; 10]);
        assert_eq!(id.get_version_num(), 7);
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(&id.as_bytes()[..6], &[0x01, 0x23, 0x45, 0x67, 0x89, 0xab]);
        // The seed has its top bit clear: room to count.
        assert_eq!(ids.last, Some((0x0123_4567_89ab, (1 << 41) - 1)));
    }

    #[test]
    fn order_holds_through_a_clock_stepping_back_and_a_full_counter() {
        let mut ids = V7::new();
        let a = ids.stamp(1000, [0; 10]);
        let b = ids.stamp(999, [0; 10]);
        assert!(a < b);
        ids.last = Some((1000, (1 << COUNTER_BITS) - 1));
        let c = ids.stamp(1000, [0; 10]);
        assert!(b < c);
        assert_eq!(ids.last.unwrap().0, 1001);
        // A new millisecond starts a new count.
        let d = ids.stamp(5000, [0; 10]);
        assert!(c < d);
    }
}
