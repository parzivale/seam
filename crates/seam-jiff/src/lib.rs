//! jiff for seam: time zones as effects.
//!
//! Turning an instant into a civil time needs a time zone, and a time zone
//! comes from the outside world: the tz database (on Unix, files under
//! `/usr/share/zoneinfo`) and the system's own zone (`TZ`, or
//! `/etc/localtime`). jiff reads them itself; this crate makes them two
//! queries instead, [`TimeZoneFor`] and [`SystemTimeZone`], so a core that
//! uses time zones says so in its bounds, and a test decides what they are.
//!
//! - [`TzDb`] answers them by reading the database through the filesystem
//!   (and the environment, for `TZ`) of the capability it is given: `Std`
//!   in production, a `Sim` in tests, whose world-file files then are the
//!   tz database.
//! - [`Bundled`] (feature `bundled`) answers `TimeZoneFor` from the
//!   database compiled into the program, performing no effect at all.
//!
//! The helpers read the wall clock through seam too: [`timestamp`],
//! [`now_zoned`], [`now_in`] and [`now_local`], each in sync, `_async` and
//! `_send` forms.
//!
//! ```
//! use seam_core::effects as fx;
//! use seam_jiff::{TzDb, now_in};
//! use seam_test::Sim;
//! use std::time::{Duration, UNIX_EPOCH};
//!
//! // The simulated tz database: one zone, as its TZif file.
//! let (_, paris) = jiff_tzdb::get("Europe/Paris").unwrap();
//! let mut sim = Sim::new();
//! sim.add_file("/usr/share/zoneinfo/Europe/Paris", paris);
//! sim.set_wall_clock(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
//!
//! let root = seam_core::root![
//!     Sim<fx::WallClock>::clone(&sim),
//!     TzDb<seam_jiff::effects::TimeZoneFor>::new(sim.clone()),
//! ];
//! let now = now_in(&root, "Europe/Paris").unwrap();
//! assert_eq!(now.to_string(), "2023-11-14T23:13:20+01:00[Europe/Paris]");
//! ```
//!
//! Cores should not call jiff's own `Zoned::now`, `Timestamp::now`,
//! `TimeZone::system` or `TimeZone::get`, which reach the system directly;
//! seam's `templates/clippy.toml` bans them. Built without jiff's default
//! features, as here, jiff has no database of its own to find.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::string::String;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use seam_core::io;
use seam_core::ops::env::Var;
use seam_core::ops::fs::{ReadAt, View, read_file};
use seam_core::ops::time::WallClock;
use seam_core::{Effect, Inspect, Lend, Operation, Query};

/// Look up the time zone named `name` in the tz database: an IANA name
/// such as `Europe/Paris` or `UTC`.
///
/// Fails with `io::ErrorKind::NotFound` for a zone the database lacks,
/// `InvalidInput` for a name that is not one (empty, absolute, or with
/// `..`), and `InvalidData` for a database entry jiff cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeZoneFor<'a> {
    /// The zone's name.
    pub name: &'a str,
}

impl<'a> TimeZoneFor<'a> {
    /// Describes looking up `name`.
    pub const fn new(name: &'a str) -> Self {
        Self { name }
    }
}

/// The system's own time zone: on Unix, from `TZ` (an IANA name, `:name`,
/// a path to a TZif file, or a POSIX TZ string), then `/etc/timezone`,
/// then `/etc/localtime`, and UTC if none is there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemTimeZone;

impl Operation for TimeZoneFor<'_> {
    type Outcome = io::Result<TimeZone>;
}
impl Query for TimeZoneFor<'_> {}

impl Operation for SystemTimeZone {
    type Outcome = io::Result<TimeZone>;
}
impl Query for SystemTimeZone {}

/// The keys that route this crate's effects in a root, as
/// `seam_core::effects` does for seam's own.
pub mod effects {
    /// Routes [`TimeZoneFor`](crate::TimeZoneFor).
    #[derive(Debug, Clone, Copy)]
    pub struct TimeZoneFor;

    /// Routes [`SystemTimeZone`](crate::SystemTimeZone).
    #[derive(Debug, Clone, Copy)]
    pub struct SystemTimeZone;
}

impl Effect for TimeZoneFor<'_> {
    type Key = effects::TimeZoneFor;
}

impl Effect for SystemTimeZone {
    type Key = effects::SystemTimeZone;
}

/// Where Unix keeps the tz database.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

/// Answers [`TimeZoneFor`] and [`SystemTimeZone`] by reading the tz
/// database, as TZif files under a directory ([`ZONEINFO`] unless built
/// with [`at`](Self::at)), through the filesystem of `C`. `SystemTimeZone`
/// also reads `TZ` through `C`'s environment.
///
/// `C` is any capability that lends `View` (and, for `SystemTimeZone`,
/// inspects `Var`): `Std`, a `Sim` (cloned from the one in the root, so
/// they share a world), or a root of its own. `F` is its file handle,
/// worked out from `C`. Tokio has no sync filesystem; under Tokio, give it
/// `Std`, which reads the few small files it needs.
#[derive(Debug, Clone)]
pub struct TzDb<C, F> {
    fs: C,
    dir: PathBuf,
    view: core::marker::PhantomData<fn() -> F>,
}

impl<C, F> TzDb<C, F>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    /// The database under [`ZONEINFO`], read through `fs`.
    pub fn new(fs: C) -> Self {
        Self::at(fs, ZONEINFO)
    }

    /// The database under `dir`, read through `fs`.
    pub fn at(fs: C, dir: impl Into<PathBuf>) -> Self {
        Self {
            fs,
            dir: dir.into(),
            view: core::marker::PhantomData,
        }
    }

    /// The zone `name`, from the TZif file `path`.
    fn read(&self, name: &str, path: &Path) -> io::Result<TimeZone> {
        let data = read_file(&self.fs, path)?;
        TimeZone::tzif(name, &data).map_err(invalid)
    }

    fn lookup(&self, name: &str) -> io::Result<TimeZone> {
        check_name(name)?;
        self.read(name, &self.dir.join(name))
    }
}

/// A jiff error, as `InvalidData`. Its message is built at run time, so it
/// travels as a `std::io::Error`.
fn invalid(e: jiff::Error) -> io::Error {
    io::Error::from(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Refuses names that would leave the database's directory.
fn check_name(name: &str) -> io::Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('/')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-+".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a time zone name",
        ))
    }
}

impl<C, F> Inspect<TimeZoneFor<'_>> for TzDb<C, F>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    fn inspect(&self, op: TimeZoneFor<'_>) -> io::Result<TimeZone> {
        match self.lookup(op.name) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such time zone in the tz database",
            )),
            found => found,
        }
    }
}

impl<C, F> Inspect<SystemTimeZone> for TzDb<C, F>
where
    C: for<'a> Lend<View<'a, F>> + for<'a> Inspect<Var<'a>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    fn inspect(&self, _: SystemTimeZone) -> io::Result<TimeZone> {
        if let Some(tz) = self.fs.inspect(Var::new("TZ")) {
            let tz = tz.into_string().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "`TZ` is not valid UTF-8")
            })?;
            return self.zone_of_tz(&tz);
        }
        match read_file(&self.fs, "/etc/timezone") {
            Ok(name) => {
                let name = String::from_utf8_lossy(&name);
                if let Ok(tz) = self.lookup(name.trim()) {
                    return Ok(tz);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match self.read("Local", Path::new("/etc/localtime")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(TimeZone::UTC),
            found => found,
        }
    }
}

impl<C, F> TzDb<C, F>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    /// The zone `TZ` names, as POSIX and glibc read it.
    fn zone_of_tz(&self, tz: &str) -> io::Result<TimeZone> {
        let tz = tz.strip_prefix(':').unwrap_or(tz);
        if tz.is_empty() {
            return Ok(TimeZone::UTC);
        }
        if tz.starts_with('/') {
            return self.read(tz, Path::new(tz));
        }
        match self.lookup(tz) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                // Not in the database: a POSIX TZ string, such as `EST5EDT`.
                TimeZone::posix(tz).map_err(|_| {
                    io::Error::new(io::ErrorKind::NotFound, "`TZ` names no known time zone")
                })
            }
            found => found,
        }
    }
}

// The lifts let async cores ask it too.
impl<C, F> seam_core::Capability for TzDb<C, F> {}

/// Answers [`TimeZoneFor`] from the tz database compiled into the program
/// (jiff-tzdb), performing no effect. It has no system zone to report.
#[cfg(feature = "bundled")]
#[derive(Debug, Default, Clone, Copy)]
pub struct Bundled;

#[cfg(feature = "bundled")]
impl Inspect<TimeZoneFor<'_>> for Bundled {
    fn inspect(&self, op: TimeZoneFor<'_>) -> io::Result<TimeZone> {
        let (name, data) = jiff_tzdb::get(op.name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no such time zone in the bundled tz database",
            )
        })?;
        TimeZone::tzif(name, data).map_err(invalid)
    }
}

#[cfg(feature = "bundled")]
impl seam_core::Capability for Bundled {}

/// The wall clock's time as a [`Timestamp`].
fn to_timestamp(now: std::time::SystemTime) -> io::Result<Timestamp> {
    Timestamp::try_from(now).map_err(invalid)
}

/// Writes each helper in its sync, async and `Send` forms, from one body.
/// `ask!(c, query)` asks in the form being generated; `call!(sync, async,
/// send; (args))` calls another helper in it.
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
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::Inspect::inspect($c, $q) };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $s $xs };
            }
            $body
        }

        #[doc = concat!("`", stringify!($name), "`, for async capabilities.")]
        pub async fn $aname<$($g)*>($($args)*) -> $ret where $($ab)* {
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::AsyncInspect::inspect_async($c, $q).await };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $as $xs.await };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($name), "`, for async capabilities, with a `Send` future: ",
            "for multithreaded runtimes."
        )]
        pub async fn $sname<$($g)*>($($args)*) -> $ret where $($nb)* {
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::SendInspect::inspect_send($c, $q).await };
            }
            #[allow(unused_macros)]
            macro_rules! call {
                ($s:path, $as:path, $ss:path; $xs:tt) => { $ss $xs.await };
            }
            $body
        }
    )*};
}

three_ways! {
    /// Now, by the wall clock, as a jiff [`Timestamp`]. Fails with
    /// `InvalidData` if the clock is outside jiff's range.
    pub fn timestamp, timestamp_async, timestamp_send
    [C] (c: &C) -> io::Result<Timestamp>
    where
        sync { C: Inspect<WallClock> }
        async { C: seam_core::future::AsyncInspect<WallClock> }
        send { C: seam_core::future::SendInspect<WallClock> + Sync }
    {
        to_timestamp(ask!(c, WallClock))
    }

    /// Now, by the wall clock, in the time zone `tz`.
    pub fn now_zoned, now_zoned_async, now_zoned_send
    [C] (c: &C, tz: TimeZone) -> io::Result<Zoned>
    where
        sync { C: Inspect<WallClock> }
        async { C: seam_core::future::AsyncInspect<WallClock> }
        send { C: seam_core::future::SendInspect<WallClock> + Sync }
    {
        Ok(call!(timestamp, timestamp_async, timestamp_send; (c))?.to_zoned(tz))
    }

    /// Now, by the wall clock, in the time zone named `name`.
    pub fn now_in, now_in_async, now_in_send
    [C] (c: &C, name: &str) -> io::Result<Zoned>
    where
        sync { C: Inspect<WallClock> + for<'a> Inspect<TimeZoneFor<'a>> }
        async {
            C: seam_core::future::AsyncInspect<WallClock>
                + for<'a> seam_core::future::AsyncInspect<TimeZoneFor<'a>>,
        }
        send {
            C: seam_core::future::SendInspect<WallClock>
                + for<'a> seam_core::future::SendInspect<TimeZoneFor<'a>>
                + Sync,
        }
    {
        let tz = ask!(c, TimeZoneFor::new(name))?;
        call!(now_zoned, now_zoned_async, now_zoned_send; (c, tz))
    }

    /// Now, by the wall clock, in the system's time zone.
    pub fn now_local, now_local_async, now_local_send
    [C] (c: &C) -> io::Result<Zoned>
    where
        sync { C: Inspect<WallClock> + Inspect<SystemTimeZone> }
        async {
            C: seam_core::future::AsyncInspect<WallClock>
                + seam_core::future::AsyncInspect<SystemTimeZone>,
        }
        send {
            C: seam_core::future::SendInspect<WallClock>
                + seam_core::future::SendInspect<SystemTimeZone>
                + Sync,
        }
    {
        let tz = ask!(c, SystemTimeZone)?;
        call!(now_zoned, now_zoned_async, now_zoned_send; (c, tz))
    }
}
