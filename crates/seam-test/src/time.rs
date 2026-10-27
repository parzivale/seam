#[cfg(feature = "std")]
pub use with_std::Clock;

#[cfg(feature = "std")]
mod with_std {
    use core::time::Duration;
    use std::format;
    use std::vec::Vec;

    use crate::sim::{SimState, World};
    use crate::world_file::{self, Failure, Node};

    /// The simulated clocks: a monotonic one, from `Instant::EPOCH`, and a
    /// wall clock, from the Unix epoch. `Sleep` advances both.
    ///
    /// Its block is `clock now=S wall=S`, in seconds; a time that is not a
    /// whole number of seconds is written in nanoseconds, `now-ns=N`.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Clock {
        /// Time since `Instant::EPOCH`.
        pub now: Duration,
        /// Time since the Unix epoch, on the wall clock.
        pub wall: Duration,
    }

    impl Clock {
        /// Moves both clocks forward, as if `d` passed.
        pub fn advance(&mut self, d: Duration) {
            self.now += d;
            self.wall += d;
        }

        /// The wall clock, as a `SystemTime`.
        pub fn wall_time(&self) -> std::time::SystemTime {
            std::time::UNIX_EPOCH + self.wall
        }
    }

    impl<G> World<G> {
        /// Moves both clocks forward, as if `d` passed outside the core.
        pub fn advance(&mut self, d: Duration) -> &mut Self {
            self.lock().state::<Clock>().advance(d);
            self
        }

        /// Sets the wall clock. The monotonic clock is unaffected.
        pub fn set_wall_clock(&mut self, time: std::time::SystemTime) -> &mut Self {
            self.lock().state::<Clock>().wall = time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            self
        }

        /// The monotonic clock, since `Instant::EPOCH`.
        #[cfg(feature = "rt")]
        pub(crate) fn clock(&self) -> Duration {
            self.lock().state::<Clock>().now
        }
    }

    impl crate::sim::Inner {
        /// The wall clock now: what writes stamp files with.
        #[cfg(feature = "fs")]
        pub(crate) fn wall(&mut self) -> std::time::SystemTime {
            self.state::<Clock>().wall_time()
        }
    }

    fn duration_prop(node: Node, key: &str, d: Duration) -> Node {
        if d.subsec_nanos() == 0 {
            node.with_prop(key, d.as_secs())
        } else {
            node.with_prop(format!("{key}-ns"), d.as_nanos() as i128)
        }
    }

    fn read_duration(node: &Node, key: &str) -> world_file::Result<Option<Duration>> {
        if let Some(v) = node.prop(key) {
            return Ok(Some(Duration::from_secs(v.uint(&format!("`{key}`"))?)));
        }
        match node.prop(&format!("{key}-ns")) {
            Some(v) => Ok(Some(Duration::from_nanos(v.uint(&format!("`{key}-ns`"))?))),
            None => Ok(None),
        }
    }

    fn clock_props(node: &Node) -> world_file::Result<()> {
        node.no_children()?;
        node.arity(0, Some(0), "only `now=` and `wall=`")?;
        node.props_only(&["now", "now-ns", "wall", "wall-ns"])
    }

    impl SimState for Clock {
        const BLOCKS: &'static [&'static str] = &["clock"];

        /// `clock now=S wall=S`: advances both clocks by `now`, then sets
        /// the wall clock to `wall` seconds after the Unix epoch.
        fn load(&mut self, node: &Node) -> world_file::Result<()> {
            clock_props(node)?;
            if let Some(d) = read_duration(node, "now")? {
                self.advance(d);
            }
            if let Some(d) = read_duration(node, "wall")? {
                self.wall = d;
            }
            Ok(())
        }

        fn to_world(&self) -> Vec<Node> {
            let node = duration_prop(Node::new("clock"), "now", self.now);
            std::vec![duration_prop(node, "wall", self.wall)]
        }

        fn check(&self, node: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
            clock_props(node)?;
            if let Some(want) = read_duration(node, "now")? {
                if self.now != want {
                    failures.push(node.failure(format!(
                        "clock: expected {want:?} since the start, found {:?}",
                        self.now
                    )));
                }
            }
            if let Some(want) = read_duration(node, "wall")? {
                if self.wall != want {
                    failures.push(node.failure(format!(
                        "wall clock: expected {} seconds after the epoch, found {}",
                        want.as_secs(),
                        self.wall.as_secs()
                    )));
                }
            }
            Ok(())
        }
    }

    #[cfg(feature = "time")]
    mod effects {
        use seam_core::effects as fx;
        use seam_core::ops::time::{Instant, Now, Sleep, WallClock};

        use super::Clock;
        use crate::sim::{Simulate, Simulated, States};

        impl Simulated for fx::Now {
            type State = Clock;
        }

        impl Simulate<Now> for fx::Now {
            fn run(states: &mut States<'_>, _: Now) -> Instant {
                Instant::from_epoch(states.state::<Clock>().now)
            }
        }

        impl Simulated for fx::Sleep {
            type State = Clock;
        }

        /// Advances the virtual clocks and returns at once.
        impl Simulate<Sleep> for fx::Sleep {
            fn run(states: &mut States<'_>, op: Sleep) {
                states.state::<Clock>().advance(op.0);
            }
        }

        impl Simulated for fx::WallClock {
            type State = Clock;
        }

        impl Simulate<WallClock> for fx::WallClock {
            fn run(states: &mut States<'_>, _: WallClock) -> std::time::SystemTime {
                states.state::<Clock>().wall_time()
            }
        }
    }
}

#[cfg(not(feature = "std"))]
mod without_std {
    use core::time::Duration;
    use seam_core::ops::time::{Instant, Now, Sleep};
    use seam_core::{Inspect, Perform};

    use crate::Sim;

    impl Sim {
        /// Moves the clock forward, as if `d` passed outside the core.
        pub fn advance(&mut self, d: Duration) -> &mut Self {
            self.clock += d;
            self
        }
    }

    impl Inspect<Now> for Sim {
        fn inspect(&self, _: Now) -> Instant {
            Instant::from_epoch(self.clock)
        }
    }

    /// Advances the virtual clock and returns at once.
    impl Perform<Sleep> for Sim {
        fn perform(&mut self, op: Sleep) {
            self.advance(op.0);
        }
    }
}
