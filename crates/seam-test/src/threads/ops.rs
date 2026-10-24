//! The operations [`ThreadSim`] performs.
//!
//! Each one is a scheduling point: another thread may run first. Those that
//! never wait then run on the [`Sim`](crate::Sim) as they are; those that
//! would block a real thread block the simulated one.

#[allow(unused_imports)]
use seam_core::{Closing, Inspect, Opening, Operation, Perform};

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
    // A scope is its two operations.
    (provide $([$($gen:tt)*] $op:ty;)*) => {
        delegate! { perform $([$($gen)*] Opening<$op>; [$($gen)*] Closing<$op>;)* }
    };
    (lend $([$($gen:tt)*] $op:ty;)*) => {
        delegate! { inspect $([$($gen)*] Opening<$op>; [$($gen)*] Closing<$op>;)* }
    };
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
    use seam_core::ops::random::FillRandom;

    use super::*;

    delegate! { perform ['a] FillRandom<'a>; }
}

#[cfg(feature = "env")]
mod env {
    use seam_core::ops::env::{Args, CurrentDir, RemoveVar, SetVar, TempDir, Var};

    use super::*;

    delegate! { inspect ['a] Var<'a>; [] Args; [] CurrentDir; [] TempDir; }
    delegate! { perform ['a] SetVar<'a>; ['a] RemoveVar<'a>; }
}

#[cfg(feature = "fs")]
mod fs {
    use seam_core::ops::fs::{
        CopyFile, CreateAtomic, CreateDir, Metadata, Mode, Open, ReadDir, RemoveDir, RemoveFile,
        Rename, View,
    };

    use super::*;
    use crate::fs::{AtomicFile, Dir, File, FileView};

    delegate! { inspect ['a] Metadata<'a>; }
    delegate! {
        perform ['a] CreateDir<'a>; ['a] RemoveFile<'a>; ['a] RemoveDir<'a>; ['a] Rename<'a>;
        ['a] CopyFile<'a>;
    }
    delegate! {
        provide ['a, M: Mode] Open<'a, M, File<M>>; ['a] CreateAtomic<'a, AtomicFile>;
    }
    delegate! { lend ['a] View<'a, FileView>; ['a] ReadDir<'a, Dir>; }
}

#[cfg(feature = "process")]
mod process {
    use seam_core::ops::process::{Exit, RunCommand, SpawnChild};

    use super::*;
    use crate::process::Child;

    delegate! { perform [] RunCommand; [] Exit; }
    // The child is the plain simulation's: nothing about it waits.
    delegate! { provide [] SpawnChild<Child>; }
}

#[cfg(feature = "io-std")]
mod stdio {
    use seam_core::ops::stdio::{OpenStderr, OpenStdin, OpenStdout};

    use super::*;
    use crate::stdio::{Stderr, Stdin, Stdout};

    // Standard input is a finite script, so reading it never waits.
    delegate! { provide [] OpenStdin<Stdin>; [] OpenStdout<Stdout>; [] OpenStderr<Stderr>; }
}

#[cfg(feature = "signal")]
mod signal {
    use seam_core::ops::signal::CtrlC;

    use super::*;

    /// Blocks until a Ctrl-C is raised, with `ThreadSim::raise_ctrl_c`.
    impl Perform<CtrlC> for ThreadSim {
        fn perform(&mut self, _: CtrlC) -> seam_core::io::Result<()> {
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
        fn perform(
            &mut self,
            op: seam_core::ops::time::Timeout<CtrlC>,
        ) -> seam_core::io::Result<()> {
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
    use seam_core::ops::net::ResolveInto;

    use super::*;

    delegate! { inspect ['a] ResolveInto<'a>; }
}
