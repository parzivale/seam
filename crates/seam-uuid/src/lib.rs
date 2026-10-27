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
//! Cores should not call `Uuid::new_v4` or `Uuid::now_v7`; seam's
//! `templates/clippy.toml` bans them.
#![forbid(unsafe_code)]

use seam_core::ops::random::FillRandom;
use seam_core::ops::time::WallClock;
use uuid::{Builder, Uuid};

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
    /// made in the same millisecond, in no particular order).
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
