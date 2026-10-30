//! Record IDs, as UUIDs from seam's randomness and clock (`seam::uuid`).
//!
//! The core stamps records with v7 UUIDs from a `V7` generator, which
//! sort in the order they were made (within one millisecond too), and v5
//! UUIDs for names, which are the same every time. The program uses the
//! system's clock and randomness; the tests a simulation with a seed and a
//! clock they set, so the IDs are known in advance.
//!
//! ```text
//! cargo run -p seam-rs --example ids --features std,test,uuid
//! ```

use seam::ops::random::FillRandom;
use seam::ops::time::WallClock;
use seam::prelude::*;
use seam::uuid::Uuid;

/// A record, stamped when it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    id: Uuid,
    title: String,
}

/// Makes a record for each title: v7 IDs from `ids`, so each sorts after
/// the ones made before it.
fn stamp<C>(c: &mut C, ids: &mut seam::uuid::V7, titles: &[&str]) -> Vec<Record>
where
    C: for<'a> Perform<FillRandom<'a>> + Inspect<WallClock>,
{
    titles
        .iter()
        .map(|t| Record {
            id: ids.next(c),
            title: (*t).to_owned(),
        })
        .collect()
}

/// A stable ID for a tag name: v5, under a namespace of our own. Pure: it
/// needs no capability at all.
fn tag_id(name: &str) -> Uuid {
    const TAGS: Uuid = seam::uuid::uuid!("6ba7b812-9dad-11d1-80b4-00c04fd430c8");
    Uuid::new_v5(&TAGS, name.as_bytes())
}

/// The edge: the system's clock and randomness.
fn main() {
    use seam::effects as fx;
    let mut root = seam::root![seam::stdlib::Std<fx::FillRandom, fx::WallClock>];
    let mut ids = seam::uuid::V7::new();
    for record in stamp(
        &mut root,
        &mut ids,
        &["buy milk", "write examples", "ship 0.7"],
    ) {
        println!("{}  {}", record.id, record.title);
    }
    println!("tag `urgent` is {}", tag_id("urgent"));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use seam::test::Sim;

    use super::*;

    fn sim_at(millis: u64) -> Sim {
        let mut sim = Sim::with_seed(11);
        sim.set_wall_clock(UNIX_EPOCH + Duration::from_millis(millis));
        sim
    }

    #[test]
    fn ids_are_v7_and_sort_in_order_made() {
        let mut sim = sim_at(1_700_000_000_000);
        let mut ids = seam::uuid::V7::new();
        // Three in one millisecond, then one later.
        let mut made = stamp(&mut sim, &mut ids, &["a", "b", "c"]);
        sim.set_wall_clock(UNIX_EPOCH + Duration::from_millis(1_700_000_000_500));
        made.extend(stamp(&mut sim, &mut ids, &["d"]));
        assert!(made.iter().all(|r| r.id.get_version_num() == 7));
        assert!(made.windows(2).all(|w| w[0].id < w[1].id));
    }

    #[test]
    fn a_seed_and_a_clock_fix_the_ids() {
        let mut ids = seam::uuid::V7::new();
        let a = stamp(&mut sim_at(1_700_000_000_000), &mut ids, &["x", "y"]);
        let mut ids = seam::uuid::V7::new();
        let b = stamp(&mut sim_at(1_700_000_000_000), &mut ids, &["x", "y"]);
        assert_eq!(a, b);
        assert_ne!(a[0].id, a[1].id);
    }

    #[test]
    fn tag_ids_are_stable() {
        assert_eq!(tag_id("urgent"), tag_id("urgent"));
        assert_ne!(tag_id("urgent"), tag_id("later"));
        assert_eq!(tag_id("urgent").get_version_num(), 5);
    }
}
