//! A world clock: now, in several time zones (`seam::jiff`), and a
//! meeting time converted between them.
//!
//! The core reads the wall clock and looks zones up through effects, so
//! the program uses the system's clock and tz database while the tests
//! fix the time and use the tz database compiled in (`jiff-bundled`).
//!
//! ```text
//! cargo run -p seam-rs --example world_clock --features std,test,jiff-bundled
//! ```

use seam::io;
use seam::jiff::civil::DateTime;
use seam::jiff::{TimeZoneFor, Zoned};
use seam::ops::time::WallClock;
use seam::prelude::*;

/// Now in each zone, as `zone: time`.
fn now_in_each<C>(c: &C, zones: &[&str]) -> io::Result<Vec<String>>
where
    C: Inspect<WallClock> + for<'a> Inspect<TimeZoneFor<'a>>,
{
    let now = seam::jiff::Timestamp::now(c)?;
    zones
        .iter()
        .map(|zone| {
            let there = now.in_tz(c, zone)?;
            Ok(format!("{zone}: {}", there.strftime("%a %H:%M")))
        })
        .collect()
}

/// A meeting at `local` civil time in `from`, as seen in `to`.
fn convert<C>(c: &C, local: DateTime, from: &str, to: &str) -> io::Result<Zoned>
where
    C: for<'a> Inspect<TimeZoneFor<'a>>,
{
    local.in_tz(c, from)?.in_tz(c, to)
}

/// The edge: the system's clock, and the zones bundled in.
fn main() -> io::Result<()> {
    use seam::effects as fx;
    let root = seam::root![
        seam::stdlib::Std<fx::WallClock>,
        seam::jiff::Bundled<seam::jiff::effects::TimeZoneFor>,
    ];
    for line in now_in_each(&root, &["America/New_York", "Europe/London", "Asia/Tokyo"])? {
        println!("{line}");
    }
    let standup = seam::jiff::civil::date(2026, 11, 2).at(9, 30, 0, 0);
    let there = convert(&root, standup, "Europe/Berlin", "America/Los_Angeles")?;
    println!(
        "09:30 in Berlin is {} in Los Angeles",
        there.strftime("%H:%M on %a")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use seam::effects as fx;
    use seam::test::Sim;

    use super::*;

    #[test]
    fn the_same_instant_everywhere() {
        let mut sim = Sim::new();
        // 2023-11-14 22:13:20 UTC.
        sim.set_wall_clock(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let root = seam::root![
            Sim<fx::WallClock>::clone(&sim),
            seam::jiff::Bundled<seam::jiff::effects::TimeZoneFor>,
        ];
        assert_eq!(
            now_in_each(&root, &["UTC", "Asia/Tokyo", "America/New_York"]).unwrap(),
            [
                "UTC: Tue 22:13",
                "Asia/Tokyo: Wed 07:13",
                "America/New_York: Tue 17:13"
            ]
        );
    }

    #[test]
    fn meetings_cross_daylight_saving() {
        let zones = seam::jiff::Bundled;
        let at_nine = |day| {
            let meet = seam::jiff::civil::date(2026, 3, day).at(9, 0, 0, 0);
            let nyc = convert(&zones, meet, "Europe/London", "America/New_York").unwrap();
            nyc.strftime("%H:%M").to_string()
        };
        // The US moves its clocks on 8 March, the UK on 29 March: for the
        // three weeks between, they are four hours apart, not five.
        assert_eq!(at_nine(2), "04:00");
        assert_eq!(at_nine(16), "05:00");
        assert_eq!(at_nine(30), "04:00");
        let meet = seam::jiff::civil::date(2026, 3, 16).at(9, 0, 0, 0);
        assert!(convert(&zones, meet, "Europe/London", "Mars/Olympus").is_err());
    }
}
