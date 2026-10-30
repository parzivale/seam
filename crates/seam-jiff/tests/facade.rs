//! jiff's pure API through the facade, and its effectful parts as effects.

use seam_core::effects as fx;
use seam_jiff::civil::{self, Date, Time, Weekday};
use seam_jiff::fmt::{rfc2822, strtime, temporal};
use seam_jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use seam_jiff::{Span, ToSpan, TzDb, Unit, Zoned, ZonedRound, effects as tz};
use seam_test::Sim;

fn sim() -> Sim {
    let mut sim = Sim::new();
    for name in ["Europe/Paris", "America/New_York"] {
        let (_, data) = jiff_tzdb::get(name).unwrap();
        sim.add_file(format!("/usr/share/zoneinfo/{name}"), data);
    }
    sim
}

#[test]
fn civil_arithmetic_and_spans() {
    let d = civil::date(2024, 2, 28);
    assert_eq!(d.tomorrow().unwrap(), civil::date(2024, 2, 29));
    assert_eq!(d.weekday(), Weekday::Wednesday);
    let later: Date = d + 1.month();
    assert_eq!(later, civil::date(2024, 3, 28));
    let span: Span = d.until(civil::date(2024, 12, 25)).unwrap();
    assert_eq!(span.get_days(), 301);
    let dt = d.to_datetime(Time::midnight()) + 90.minutes();
    assert_eq!(dt.to_string(), "2024-02-28T01:30:00");
    let fmt = strtime::format("%A, %B %-d", d).unwrap();
    assert_eq!(fmt, "Wednesday, February 28");
    let parsed: Date = strtime::parse("%Y/%m/%d", "2024/07/14")
        .unwrap()
        .to_date()
        .unwrap();
    assert_eq!(parsed, civil::date(2024, 7, 14));
    assert_eq!(
        temporal::DateTimeParser::new()
            .parse_date("2024-07-14")
            .unwrap(),
        parsed
    );
}

#[test]
fn zones_through_the_capability() {
    let sim = sim();
    let root = seam_core::root![TzDb<tz::TimeZoneFor>::new(sim.clone())];

    let paris = TimeZone::get(&root, "Europe/Paris").unwrap();
    let z = civil::date(2024, 6, 1)
        .at(12, 0, 0, 0)
        .in_tz(&root, "Europe/Paris")
        .unwrap();
    assert_eq!(z.offset(), Offset::constant(2));
    assert_eq!(z.time_zone(), &paris);
    // Arithmetic across the DST change, rounding.
    let winter = (&z + 6.months())
        .round(ZonedRound::new().smallest(Unit::Day))
        .unwrap();
    assert_eq!(winter.offset(), Offset::constant(1));
    // Parsing an annotated datetime looks the zone up in the root.
    let parsed = Zoned::parse(&root, "2024-06-01T12:00:00+02:00[Europe/Paris]").unwrap();
    assert_eq!(parsed, z);
    let ny = parsed.in_tz(&root, "America/New_York").unwrap();
    assert_eq!(ny.hour(), 6);
    // A zone the root does not have is an error, not a silent UTC.
    assert!(Zoned::parse(&root, "2024-06-01T12:00[Mars/Base]").is_err());
    // strtime's %Q.
    let bdt = strtime::parse("%Y-%m-%d %H:%M %Q", "2024-06-01 12:00 Europe/Paris").unwrap();
    assert_eq!(bdt.to_zoned(&root).unwrap(), z);
    // Gaps and folds.
    let gap = paris.to_ambiguous_zoned(civil::date(2024, 3, 31).at(2, 30, 0, 0));
    assert!(matches!(gap.offset(), AmbiguousOffset::Gap { .. }));
    // RFC 2822 needs no database.
    let mail = rfc2822::parse("Sat, 1 Jun 2024 12:00:00 +0200").unwrap();
    assert_eq!(mail.timestamp(), z.timestamp());
    // Transitions iterate in the facade's types.
    let next = paris.following(z.timestamp()).next().unwrap();
    assert_eq!(next.offset(), Offset::constant(1));
}

#[test]
fn now_is_the_capabilitys_clock() {
    let mut sim = sim();
    sim.set_wall_clock(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000));
    sim.set_var("TZ", "Europe/Paris");
    let root = seam_core::root! {
        Sim<fx::WallClock>::clone(&sim),
        TzDb<tz::TimeZoneFor, tz::SystemTimeZone>::new(sim.clone()),
    };
    assert_eq!(
        seam_jiff::Timestamp::now(&root).unwrap().as_second(),
        1_700_000_000
    );
    let now = Zoned::now(&root).unwrap();
    assert_eq!(now.to_string(), "2023-11-14T23:13:20+01:00[Europe/Paris]");
}
