/// SplitMix64: small, fast, and the same everywhere. Not cryptographic.
#[derive(Debug, Clone, Default)]
pub(crate) struct SplitMix64(pub(crate) u64);

impl SplitMix64 {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Fills `buf` from the stream.
    pub(crate) fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

#[cfg(feature = "std")]
pub use with_std::Random;

#[cfg(feature = "std")]
mod with_std {
    use std::vec::Vec;

    use seam_core::effects as fx;
    use seam_core::ops::random::FillRandom;

    use super::SplitMix64;
    use crate::sim::{SimState, Simulate, Simulated, States, World};
    use crate::world_file::{self, Node};

    /// The simulated random numbers: a seeded SplitMix64 stream. The same
    /// seed gives the same numbers.
    ///
    /// Its block is `seed N`. Written out, `N` is where the stream is now,
    /// so loading it continues from there.
    #[derive(Debug, Clone, Default)]
    pub struct Random(SplitMix64);

    impl Random {
        /// Restarts the stream from `seed`.
        pub fn reseed(&mut self, seed: u64) {
            self.0 = SplitMix64(seed);
        }
    }

    impl<G> World<G> {
        /// Restarts the random numbers from `seed`, as if the world had
        /// been created with [`Sim::with_seed`](crate::Sim::with_seed).
        pub fn set_seed(&mut self, seed: u64) -> &mut Self {
            self.lock().state::<Random>().reseed(seed);
            self
        }
    }

    impl SimState for Random {
        const BLOCKS: &'static [&'static str] = &["seed"];

        /// `seed N`: restarts the stream from `N`.
        fn load(&mut self, node: &Node) -> world_file::Result<()> {
            node.no_children()?;
            node.props_only(&[])?;
            node.arity(1, Some(1), "a seed")?;
            self.reseed(node.args()[0].uint::<u64>("the seed")?);
            Ok(())
        }

        fn to_world(&self) -> Vec<Node> {
            std::vec![Node::new("seed").with_arg(self.0.0)]
        }
    }

    impl Simulated for fx::FillRandom {
        type State = Random;
    }

    impl Simulate<FillRandom<'_>> for fx::FillRandom {
        fn run(states: &mut States<'_>, op: FillRandom<'_>) {
            states.state::<Random>().0.fill(op.0);
        }
    }
}

#[cfg(not(feature = "std"))]
mod without_std {
    use seam_core::Perform;
    use seam_core::ops::random::FillRandom;

    use super::SplitMix64;
    use crate::Sim;

    impl Sim {
        /// Restarts the random numbers from `seed`, as if the world had
        /// been created with [`Sim::with_seed`].
        pub fn set_seed(&mut self, seed: u64) -> &mut Self {
            self.rng = SplitMix64(seed);
            self
        }
    }

    impl Perform<FillRandom<'_>> for Sim {
        fn perform(&mut self, op: FillRandom<'_>) {
            self.rng.fill(op.0);
        }
    }
}
