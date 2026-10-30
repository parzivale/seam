//! rand's API on seam's randomness.

use seam_core::Perform;
use seam_core::ops::random::FillRandom;
use seam_rand::RngExt;
use seam_rand::rngs::StdRng;
use seam_test::Sim;

/// Counts the `FillRandom`s it performs.
struct Counting(Sim, usize);
impl Perform<FillRandom<'_>> for Counting {
    fn perform(&mut self, op: FillRandom<'_>) {
        self.1 += 1;
        self.0.perform(op);
    }
}

#[test]
fn draws_are_seeded_by_the_sim() {
    let draw = |seed| {
        let mut sim = Sim::with_seed(seed);
        let mut rng = seam_rand::rng(&mut sim);
        let mut bytes = [0u8; 16];
        rng.fill(&mut bytes);
        (rng.random::<u64>(), rng.random_range(10..20u32), bytes)
    };
    assert_eq!(draw(1), draw(1));
    assert_ne!(draw(1), draw(2));
    assert!((10..20).contains(&draw(3).1));
}

#[test]
fn every_draw_through_rng_is_an_effect() {
    let mut c = Counting(Sim::new(), 0);
    let mut rng = seam_rand::rng(&mut c);
    let _: u32 = rng.random();
    let _: u64 = rng.random();
    assert_eq!(c.1, 2);
}

#[test]
fn a_seeded_generator_costs_one_effect() {
    let mut c = Counting(Sim::with_seed(5), 0);
    let mut fast: StdRng = seam_rand::seeded(&mut c);
    let many: Vec<u32> = (0..1000).map(|_| fast.random()).collect();
    assert_eq!(c.1, 1);
    let mut again: StdRng = seam_rand::seeded(&mut Sim::with_seed(5));
    assert_eq!(many[0], again.random::<u32>());
}
