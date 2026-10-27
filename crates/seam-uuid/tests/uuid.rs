//! UUIDs on the simulation.

use std::time::{Duration, UNIX_EPOCH};

use seam_core::future::block_on;
use seam_test::Sim;

#[test]
fn v4_is_random_and_seeded() {
    let id = seam_uuid::new_v4(&mut Sim::with_seed(1));
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(id, seam_uuid::new_v4(&mut Sim::with_seed(1)));
    assert_ne!(id, seam_uuid::new_v4(&mut Sim::with_seed(2)));
    let mut sim = Sim::with_seed(1);
    assert_ne!(seam_uuid::new_v4(&mut sim), seam_uuid::new_v4(&mut sim));
    assert_eq!(block_on(seam_uuid::new_v4_send(&mut Sim::with_seed(1))), id);
}

#[test]
fn v7_follows_the_wall_clock() {
    let mut sim = Sim::with_seed(3);
    sim.set_wall_clock(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123));
    let first = seam_uuid::new_v7(&mut sim);
    sim.advance(Duration::from_millis(5));
    let later = block_on(seam_uuid::new_v7_async(&mut sim));
    assert_eq!(first.get_version_num(), 7);
    assert!(later > first);
    let (secs, nanos) = first.get_timestamp().unwrap().to_unix();
    assert_eq!((secs, nanos / 1_000_000), (1_700_000_000, 123));
}
