//! The operations [`AsyncSim`] performs.
//!
//! Operations that never wait are run on the [`Sim`](crate::Sim) as they
//! are, so they behave exactly as in a synchronous test. Those that would
//! wait on a real system are native async here.

#[allow(unused_imports)]
use core::future::poll_fn;
#[allow(unused_imports)]
use core::task::Poll;

#[allow(unused_imports)]
use seam_core::{Inspect, Operation, Perform, Provide, Token};

#[allow(unused_imports)]
use super::AsyncSim;
#[allow(unused_imports)]
use super::async_op;

/// Implements `Inspect` or `Perform` for operations by running them on the
/// `Sim`.
#[allow(unused_macros)]
macro_rules! delegate {
    (inspect $([$($gen:tt)*] $op:ty;)*) => {$(
        impl<$($gen)*> Inspect<$op> for AsyncSim {
            fn inspect(&self, op: $op) -> <$op as Operation>::Outcome {
                self.shared.sim().inspect(op)
            }
        }
    )*};
    (perform $([$($gen:tt)*] $op:ty;)*) => {$(
        impl<$($gen)*> Perform<$op> for AsyncSim {
            fn perform(&mut self, op: $op) -> <$op as Operation>::Outcome {
                self.shared.sim().perform(op)
            }
        }
    )*};
    (provide $([$($gen:tt)*] $op:ty;)*) => {$(
        impl<$($gen)*> Provide<$op> for AsyncSim {
            fn provide(
                &mut self,
                op: $op,
                token: Token,
            ) -> Result<<$op as seam_core::Acquisition>::Resource, <$op as seam_core::Acquisition>::Error> {
                Provide::provide(&mut *self.shared.sim(), op, token)
            }

            fn close(
                &mut self,
                resource: <$op as seam_core::Acquisition>::Resource,
            ) -> Result<(), <$op as seam_core::Acquisition>::Error> {
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

    // Sleeping suspends until the clock reaches the deadline. When nothing
    // else can run, the runtime moves the clock there directly.
    async_op!([] AsyncSim, Sleep, |s, op| {
        let deadline = s.shared.sim().clock + op.0;
        let mut registered = false;
        poll_fn(|cx| {
            if s.shared.sim().clock >= deadline {
                return Poll::Ready(());
            }
            if !registered {
                s.shared.add_timer(deadline, cx.waker().clone());
                registered = true;
            }
            Poll::Pending
        })
        .await
    });
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
        Rename,
    };

    use super::*;
    use crate::fs::{AtomicFile, Dir, File};

    delegate! { inspect ['a] Metadata<'a>; }
    delegate! {
        perform ['a] CreateDir<'a>; ['a] RemoveFile<'a>; ['a] RemoveDir<'a>; ['a] Rename<'a>;
        ['a] CopyFile<'a>;
    }
    delegate! {
        provide ['a, M: Mode] Open<'a, M, File<M>>; ['a] CreateAtomic<'a, AtomicFile>;
        ['a] ReadDir<'a, Dir>;
    }
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

    // Waits until a Ctrl-C is raised, with `AsyncSim::raise_ctrl_c`.
    async_op!([] AsyncSim, CtrlC, |s, _op| {
        poll_fn(|cx| {
            let mut sim = s.shared.sim();
            if sim.ctrl_c > 0 {
                sim.ctrl_c -= 1;
                return Poll::Ready(Ok(()));
            }
            drop(sim);
            s.shared.wait_for_signal(cx.waker());
            Poll::Pending
        })
        .await
    });
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
