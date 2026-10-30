//! The facade's effectful functions, over effects: what jiff does by
//! reading the clock, the system's time zone or its tz database, done by
//! asking a capability instead.

use seam_core::Inspect;
use seam_core::io;
use seam_core::ops::time::WallClock;

use crate::civil::{Date, DateTime, Time};
use crate::fmt::strtime::BrokenDownTime;
use crate::fmt::temporal::{Pieces, PiecesOffset, TimeZoneAnnotation, TimeZoneAnnotationKind};
use crate::tz::{Disambiguation, Offset, OffsetConflict, TimeZone};
use crate::{SystemTimeZone, TimeZoneFor, Timestamp, Zoned, invalid};

impl Timestamp {
    /// Now, by `c`'s wall clock: jiff's `Timestamp::now`, as an effect.
    pub fn now<C: Inspect<WallClock>>(c: &C) -> io::Result<Self> {
        crate::timestamp(c)
    }

    /// This instant in the zone `name`, which `c` looks up: jiff's
    /// `Timestamp::in_tz`, as an effect.
    pub fn in_tz<C>(self, c: &C, name: &str) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        Ok(self.to_zoned(TimeZone::get(c, name)?))
    }
}

impl Zoned {
    /// Now, by `c`'s wall clock, in its system time zone: jiff's
    /// `Zoned::now`, as an effect.
    pub fn now<C>(c: &C) -> io::Result<Self>
    where
        C: Inspect<WallClock> + Inspect<SystemTimeZone>,
    {
        crate::now_local(c)
    }

    /// The same instant in the zone `name`, which `c` looks up: jiff's
    /// `Zoned::in_tz`, as an effect.
    pub fn in_tz<C>(&self, c: &C, name: &str) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        Ok(self.with_time_zone(TimeZone::get(c, name)?))
    }

    /// Parses a Temporal datetime with a time zone annotation
    /// (`2024-06-01T12:00[Europe/Paris]`), looking the zone up in `c`:
    /// jiff's `str::parse::<Zoned>`, as an effect.
    pub fn parse<C>(c: &C, input: impl AsRef<[u8]>) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        DateTimeParser::new().parse_zoned(c, input)
    }
}

impl Date {
    /// Midnight-or-later on this date in the zone `name`, which `c` looks
    /// up: jiff's `Date::in_tz`, as an effect.
    pub fn in_tz<C>(self, c: &C, name: &str) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        TimeZone::get(c, name)?
            .to_zoned(self.to_datetime(Time::midnight()))
            .map_err(invalid)
    }
}

impl DateTime {
    /// This civil time in the zone `name`, which `c` looks up: jiff's
    /// `DateTime::in_tz`, as an effect.
    pub fn in_tz<C>(self, c: &C, name: &str) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        TimeZone::get(c, name)?.to_zoned(self).map_err(invalid)
    }
}

impl TimeZone {
    /// The zone `name` (`"Europe/Paris"`), which `c` looks up: jiff's
    /// `TimeZone::get`, as an effect.
    pub fn get<C>(c: &C, name: &str) -> io::Result<TimeZone>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        c.inspect(TimeZoneFor::new(name))
    }

    /// `c`'s system time zone: jiff's `TimeZone::system`, as an effect.
    pub fn system<C: Inspect<SystemTimeZone> + ?Sized>(c: &C) -> io::Result<TimeZone> {
        c.inspect(SystemTimeZone)
    }
}

impl TimeZoneAnnotation<'_> {
    /// The zone it names, which `c` looks up (or the offset it gives):
    /// jiff's `TimeZoneAnnotation::to_time_zone`, as an effect.
    pub fn to_time_zone<C>(&self, c: &C) -> io::Result<TimeZone>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        self.kind().to_time_zone(c)
    }
}

impl TimeZoneAnnotationKind<'_> {
    /// The zone it names, which `c` looks up (or the offset it gives):
    /// jiff's `TimeZoneAnnotationKind::to_time_zone`, as an effect.
    pub fn to_time_zone<C>(&self, c: &C) -> io::Result<TimeZone>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        match self {
            TimeZoneAnnotationKind::Named(name) => TimeZone::get(c, name.as_str()),
            TimeZoneAnnotationKind::Offset(offset) => Ok(TimeZone::fixed(*offset)),
        }
    }
}

impl BrokenDownTime {
    /// A zoned datetime from the parsed fields, its zone (`%Q`) looked up
    /// in `c`, or fixed at its offset (`%z`): jiff's
    /// `BrokenDownTime::to_zoned`, as an effect.
    pub fn to_zoned<C>(&self, c: &C) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        let dt = self.to_datetime().map_err(invalid)?;
        match (self.iana_time_zone(), self.offset()) {
            (Some(name), offset) => {
                let tz = TimeZone::get(c, name)?;
                let ambiguous = match offset {
                    Some(offset) => OffsetConflict::Reject.resolve(dt, offset, tz),
                    None => Ok(tz.to_ambiguous_zoned(dt)),
                };
                ambiguous.and_then(|z| z.compatible()).map_err(invalid)
            }
            (None, Some(offset)) => TimeZone::fixed(offset).to_zoned(dt).map_err(invalid),
            (None, None) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the parsed time has no time zone or offset",
            )),
        }
    }
}

/// jiff's [`DateTimeParser`](jiff::fmt::temporal::DateTimeParser) for
/// Temporal (ISO 8601 / RFC 9557) datetimes, without its effectful
/// functions: [`parse_zoned`](Self::parse_zoned) and
/// [`parse_time_zone`](Self::parse_time_zone) look zones up through a
/// capability.
#[derive(Debug)]
pub struct DateTimeParser {
    inner: jiff::fmt::temporal::DateTimeParser,
    conflict: OffsetConflict,
    disambiguation: Disambiguation,
}

impl Default for DateTimeParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DateTimeParser {
    /// jiff's [`DateTimeParser::new`](jiff::fmt::temporal::DateTimeParser::new).
    pub const fn new() -> Self {
        Self {
            inner: jiff::fmt::temporal::DateTimeParser::new(),
            conflict: OffsetConflict::Reject,
            disambiguation: Disambiguation::Compatible,
        }
    }

    /// jiff's [`DateTimeParser::offset_conflict`](jiff::fmt::temporal::DateTimeParser::offset_conflict).
    #[must_use]
    pub fn offset_conflict(self, strategy: OffsetConflict) -> Self {
        Self {
            inner: self.inner.offset_conflict(strategy.into_jiff()),
            conflict: strategy,
            ..self
        }
    }

    /// jiff's [`DateTimeParser::disambiguation`](jiff::fmt::temporal::DateTimeParser::disambiguation).
    #[must_use]
    pub const fn disambiguation(self, strategy: Disambiguation) -> Self {
        Self {
            inner: self.inner.disambiguation(strategy),
            disambiguation: strategy,
            ..self
        }
    }

    /// Parses a datetime with a time zone annotation, looking the zone up
    /// in `c`, and resolving an offset that disagrees with it, and a time
    /// the zone skips or repeats, as configured: jiff's
    /// `DateTimeParser::parse_zoned`, as an effect.
    pub fn parse_zoned<C>(&self, c: &C, input: impl AsRef<[u8]>) -> io::Result<Zoned>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        let pieces = self.parse_pieces(input.as_ref()).map_err(invalid)?;
        let Some(annotation) = pieces.time_zone_annotation() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a zoned datetime needs a time zone annotation, as in `[Europe/Paris]`",
            ));
        };
        let tz = annotation.to_time_zone(c)?;
        let dt = pieces
            .date()
            .to_datetime(pieces.time().unwrap_or(Time::midnight()));
        let offset = match pieces.offset() {
            None => None,
            Some(PiecesOffset::Zulu) => Some(Offset::UTC),
            Some(PiecesOffset::Numeric(n)) => Some(n.offset()),
        };
        let ambiguous = match offset {
            Some(offset) => self.conflict.resolve(dt, offset, tz),
            None => Ok(tz.to_ambiguous_zoned(dt)),
        };
        ambiguous
            .and_then(|z| z.disambiguate(self.disambiguation))
            .map_err(invalid)
    }

    /// Parses a time zone (an IANA name, an offset or a POSIX TZ string),
    /// looking names up in `c`: jiff's `DateTimeParser::parse_time_zone`,
    /// as an effect.
    pub fn parse_time_zone<C>(&self, c: &C, input: impl AsRef<[u8]>) -> io::Result<TimeZone>
    where
        C: for<'a> Inspect<TimeZoneFor<'a>> + ?Sized,
    {
        let input = input.as_ref();
        let text = core::str::from_utf8(input)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))?;
        if let Ok(offset) = self
            .inner
            .parse_time_zone_with(&jiff::tz::TimeZoneDatabase::none(), input)
        {
            // Offsets and POSIX strings need no database.
            return Ok(TimeZone::from_jiff(offset));
        }
        TimeZone::get(c, text)
    }

    /// jiff's [`DateTimeParser::parse_timestamp`](jiff::fmt::temporal::DateTimeParser::parse_timestamp).
    pub fn parse_timestamp<I: AsRef<[u8]>>(&self, input: I) -> Result<Timestamp, jiff::Error> {
        self.inner.parse_timestamp(input).map(Timestamp::from_jiff)
    }

    /// jiff's [`DateTimeParser::parse_datetime`](jiff::fmt::temporal::DateTimeParser::parse_datetime).
    pub fn parse_datetime<I: AsRef<[u8]>>(&self, input: I) -> Result<DateTime, jiff::Error> {
        self.inner.parse_datetime(input).map(DateTime::from_jiff)
    }

    /// jiff's [`DateTimeParser::parse_date`](jiff::fmt::temporal::DateTimeParser::parse_date).
    pub fn parse_date<I: AsRef<[u8]>>(&self, input: I) -> Result<Date, jiff::Error> {
        self.inner.parse_date(input).map(Date::from_jiff)
    }

    /// jiff's [`DateTimeParser::parse_time`](jiff::fmt::temporal::DateTimeParser::parse_time).
    pub fn parse_time<I: AsRef<[u8]>>(&self, input: I) -> Result<Time, jiff::Error> {
        self.inner.parse_time(input).map(Time::from_jiff)
    }

    /// jiff's [`DateTimeParser::parse_pieces`](jiff::fmt::temporal::DateTimeParser::parse_pieces).
    pub fn parse_pieces<'i, I: ?Sized + AsRef<[u8]> + 'i>(
        &self,
        input: &'i I,
    ) -> Result<Pieces<'i>, jiff::Error> {
        self.inner.parse_pieces(input).map(Pieces::from_jiff)
    }
}
