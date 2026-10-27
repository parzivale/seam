//! Time zones through seam: a simulated tz database, the system zone's
//! rules, the bundled database, and the real one.

use std::time::{Duration, UNIX_EPOCH};

use seam_core::Inspect;
use seam_core::effects as fx;
use seam_core::io::ErrorKind;
use seam_jiff::{SystemTimeZone, TimeZoneFor, TzDb, now_in, now_in_send, now_local, timestamp};
use seam_test::Sim;

/// A simulated world with a few zones in its tz database, at a known time.
fn world() -> Sim {
    let mut sim = Sim::new();
    for name in ["Europe/Paris", "America/New_York", "Asia/Tokyo", "UTC"] {
        let (_, data) = jiff_tzdb::get(name).unwrap();
        sim.add_file(format!("/usr/share/zoneinfo/{name}"), data);
    }
    sim.set_wall_clock(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    sim
}

/// The zone the system reports: its name (`-` if it has none) and its
/// offset at the world's time.
fn system_zone(sim: &Sim) -> String {
    let tz = TzDb::new(sim.clone()).inspect(SystemTimeZone).unwrap();
    let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    format!("{} {}", tz.iana_name().unwrap_or("-"), tz.to_offset(now))
}

#[test]
fn zones_come_from_the_simulated_database() {
    let sim = world();
    let root = seam_core::root![
        Sim<fx::WallClock>::clone(&sim),
        TzDb<seam_jiff::effects::TimeZoneFor>::new(sim.clone()),
    ];
    assert_eq!(timestamp(&root).unwrap().as_second(), 1_700_000_000);
    let tokyo = now_in(&root, "Asia/Tokyo").unwrap();
    assert_eq!(tokyo.to_string(), "2023-11-15T07:13:20+09:00[Asia/Tokyo]");
    let new_york = seam_core::future::block_on(now_in_send(&root, "America/New_York")).unwrap();
    assert_eq!(
        new_york.to_string(),
        "2023-11-14T17:13:20-05:00[America/New_York]"
    );

    // Time passes on the simulated clock.
    let mut sim = sim;
    sim.advance(Duration::from_secs(3600));
    assert_eq!(now_in(&root, "UTC").unwrap().hour(), 23);
}

#[test]
fn missing_and_bad_names() {
    let db = TzDb::new(world());
    let kind = |name| db.inspect(TimeZoneFor::new(name)).unwrap_err().kind();
    assert_eq!(kind("Mars/Olympus_Mons"), ErrorKind::NotFound);
    assert_eq!(kind("../../etc/passwd"), ErrorKind::InvalidInput);
    assert_eq!(kind("/etc/passwd"), ErrorKind::InvalidInput);
    assert_eq!(kind(""), ErrorKind::InvalidInput);

    let mut sim = world();
    sim.add_file("/usr/share/zoneinfo/Broken", "not a TZif file");
    let broken = TzDb::new(sim).inspect(TimeZoneFor::new("Broken"));
    assert_eq!(broken.unwrap_err().kind(), ErrorKind::InvalidData);
}

#[test]
fn the_system_zone_follows_tz_then_the_files() {
    // Nothing set: UTC.
    let mut sim = world();
    assert_eq!(system_zone(&sim), "UTC +00");

    // `/etc/localtime` alone: its rules, under the name `Local`.
    let (_, tokyo) = jiff_tzdb::get("Asia/Tokyo").unwrap();
    sim.add_file("/etc/localtime", tokyo);
    assert_eq!(system_zone(&sim), "Local +09");

    // `/etc/timezone` names it.
    sim.add_file("/etc/timezone", "Europe/Paris\n");
    assert_eq!(system_zone(&sim), "Europe/Paris +01");

    // `TZ` wins, in each of its forms.
    sim.set_var("TZ", "America/New_York");
    assert_eq!(system_zone(&sim), "America/New_York -05");
    sim.set_var("TZ", ":Asia/Tokyo");
    assert_eq!(system_zone(&sim), "Asia/Tokyo +09");
    sim.set_var("TZ", "/usr/share/zoneinfo/Europe/Paris");
    assert_eq!(system_zone(&sim), "/usr/share/zoneinfo/Europe/Paris +01");
    sim.set_var("TZ", "EST5EDT,M3.2.0,M11.1.0");
    assert_eq!(system_zone(&sim), "- -05");

    let root = seam_core::root![
        Sim<fx::WallClock>::clone(&sim),
        TzDb<seam_jiff::effects::SystemTimeZone>::new(sim.clone()),
    ];
    assert_eq!(now_local(&root).unwrap().hour(), 17);
}

#[test]
fn a_database_in_another_directory() {
    let mut sim = Sim::new();
    let (_, data) = jiff_tzdb::get("Europe/Paris").unwrap();
    sim.add_file("/opt/tz/Europe/Paris", data);
    let db = TzDb::at(sim, "/opt/tz");
    assert!(db.inspect(TimeZoneFor::new("Europe/Paris")).is_ok());
}

#[test]
fn the_bundled_database_needs_no_files() {
    let sim = world();
    let root = seam_core::root![
        Sim<fx::WallClock>::clone(&sim),
        seam_jiff::Bundled<seam_jiff::effects::TimeZoneFor>,
    ];
    let paris = now_in(&root, "Europe/Paris").unwrap();
    assert_eq!(paris.to_string(), "2023-11-14T23:13:20+01:00[Europe/Paris]");
    let missing = seam_jiff::Bundled.inspect(TimeZoneFor::new("Mars/Olympus_Mons"));
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
}

/// The real tz database, where the machine has one.
#[test]
fn the_real_database_through_std() {
    if !std::path::Path::new("/usr/share/zoneinfo/UTC").exists() {
        return;
    }
    let root = seam_core::root![
        seam_std::Std<fx::WallClock>,
        TzDb<seam_jiff::effects::TimeZoneFor>::new(seam_std::Std),
    ];
    let utc = now_in(&root, "UTC").unwrap();
    assert_eq!(utc.time_zone().iana_name(), Some("UTC"));
}
