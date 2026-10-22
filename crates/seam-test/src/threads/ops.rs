//! The operations [`ThreadSim`] performs.
//!
//! Each one is a scheduling point: another thread may run first. Those that
//! never wait then run on the [`Sim`](crate::Sim) as they are; those that
//! would block a real thread block the simulated one.

#[allow(unused_imports)]
use seam_core::{Inspect, Operation, Perform, Provide, Token};

use super::ThreadSim;

/// Implements `Inspect`, `Perform` or `Provide` for operations by switching
/// threads, then running them on the `Sim`.
#[allow(unused_macros)]
macro_rules! delegate {
    (inspect $([$($gen:tt)*] $op:ty;)*) => {$(
        impl<$($gen)*> Inspect<$op> for ThreadSim {
            fn inspect(&self, op: $op) -> <$op as Operation>::Outcome {
                self.shared.switch();
                self.shared.sim().inspect(op)
            }
        }
    )*};
    (perform $([$($gen:tt)*] $op:ty;)*) => {$(
        impl<$($gen)*> Perform<$op> for ThreadSim {
            fn perform(&mut self, op: $op) -> <$op as Operation>::Outcome {
                self.shared.switch();
                self.shared.sim().perform(op)
            }
        }
    )*};
    (provide $($op:ty;)*) => {$(
        impl Provide<$op> for ThreadSim {
            fn provide(
                &mut self,
                op: $op,
                token: Token,
            ) -> Result<<$op as seam_core::Acquisition>::Resource, <$op as seam_core::Acquisition>::Error> {
                self.shared.switch();
                Provide::provide(&mut *self.shared.sim(), op, token)
            }

            fn close(
                &mut self,
                resource: <$op as seam_core::Acquisition>::Resource,
            ) -> Result<(), <$op as seam_core::Acquisition>::Error> {
                self.shared.switch();
                Provide::<$op>::close(&mut *self.shared.sim(), resource)
            }
        }
    )*};
}

#[cfg(feature = "time")]
mod time {
    use seam_core::ops::time::{Now, Sleep, WallClock};

    use super::*;

    delegate! { inspect [] Now; [] WallClock; }

    /// Blocks until the clock reaches the deadline. When no thread can run,
    /// the clock moves there directly. Sleeping for zero just switches.
    impl Perform<Sleep> for ThreadSim {
        fn perform(&mut self, op: Sleep) {
            if op.0.is_zero() {
                self.shared.switch();
                return;
            }
            let deadline = self.shared.sim().clock + op.0;
            self.shared.sleep_until(deadline);
        }
    }
}

#[cfg(feature = "random")]
mod random {
    use seam_core::ops::random::{FillRandom, RandomU64};

    use super::*;

    delegate! { perform [] RandomU64; ['a] FillRandom<'a>; }
}

#[cfg(feature = "env")]
mod env {
    use seam_core::ops::env::{Args, CurrentDir, TempDir, Var};

    use super::*;

    delegate! { inspect [] Var; [] Args; [] CurrentDir; [] TempDir; }
}

#[cfg(feature = "fs")]
mod fs {
    use seam_core::ops::fs::{
        CopyFile, CreateAtomic, CreateDir, Metadata, Open, ReadDir, ReadFile, ReadToString,
        RemoveDir, RemoveFile, Rename, SweepAtomic, WriteFile,
    };

    use super::*;
    use crate::fs::{AtomicFile, File};

    delegate! { inspect [] ReadFile; [] ReadToString; [] Metadata; [] ReadDir; }
    delegate! {
        perform [] WriteFile; [] CreateDir; [] RemoveFile; [] RemoveDir; [] Rename; [] CopyFile;
        [] SweepAtomic;
    }
    delegate! { provide Open<File>; CreateAtomic<AtomicFile>; }
}

#[cfg(feature = "process")]
mod process {
    use seam_core::ops::process::{Exit, RunCommand};

    use super::*;

    delegate! { perform [] RunCommand; [] Exit; }
}

#[cfg(feature = "io-std")]
mod stdio {
    use seam_core::ops::stdio::{EPrint, Print, ReadLine};

    use super::*;

    // Standard input is a finite script, so reading it never waits.
    delegate! { perform ['a] Print<'a>; ['a] EPrint<'a>; [] ReadLine; }
}

#[cfg(feature = "signal")]
mod signal {
    use seam_core::ops::signal::CtrlC;

    use super::*;

    /// Blocks until a Ctrl-C is raised, with `ThreadSim::raise_ctrl_c`.
    impl Perform<CtrlC> for ThreadSim {
        fn perform(&mut self, _: CtrlC) -> std::io::Result<()> {
            self.shared.switch();
            self.shared.wait_until(|waker| {
                let mut sim = self.shared.sim();
                if sim.ctrl_c > 0 {
                    sim.ctrl_c -= 1;
                    return Some(Ok(()));
                }
                drop(sim);
                self.shared.wait_for_signal(waker);
                None
            })
        }
    }

    /// Blocks at most the timeout on the virtual clock.
    #[cfg(feature = "time")]
    impl Perform<seam_core::ops::time::Timeout<CtrlC>> for ThreadSim {
        fn perform(&mut self, op: seam_core::ops::time::Timeout<CtrlC>) -> std::io::Result<()> {
            use seam_core::ops::time::TimesOut;
            self.shared.switch();
            self.shared
                .wait_within(op.after, |waker| {
                    let mut sim = self.shared.sim();
                    if sim.ctrl_c > 0 {
                        sim.ctrl_c -= 1;
                        return Some(Ok(()));
                    }
                    drop(sim);
                    self.shared.wait_for_signal(waker);
                    None
                })
                .unwrap_or_else(|| op.op.timed_out())
        }
    }
}

mod task {
    use seam_core::ops::task::AvailableParallelism;

    use super::*;

    delegate! { inspect [] AvailableParallelism; }
}

#[cfg(feature = "net")]
mod net {
    use seam_core::ops::net::Resolve;

    use super::*;

    delegate! { inspect [] Resolve; }
}
