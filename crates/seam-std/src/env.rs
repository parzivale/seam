//! The process environment. Reading it goes through [`Std`]; changing it
//! through [`WritableEnv`].

use core::marker::PhantomData;
use std::ffi::OsString;
use std::path::PathBuf;

use seam_core::ops::env::{Args, CurrentDir, RemoveVar, SetVar, TempDir, Var};
use seam_core::{Inspect, Perform};

use crate::Std;

impl Inspect<Var<'_>> for Std {
    fn inspect(&self, op: Var<'_>) -> Option<OsString> {
        std::env::var_os(op.name)
    }
}

impl Inspect<Args> for Std {
    fn inspect(&self, _: Args) -> Vec<OsString> {
        std::env::args_os().collect()
    }
}

impl Inspect<CurrentDir> for Std {
    fn inspect(&self, _: CurrentDir) -> seam_core::io::Result<PathBuf> {
        crate::sio(std::env::current_dir())
    }
}

impl Inspect<TempDir> for Std {
    fn inspect(&self, _: TempDir) -> PathBuf {
        std::env::temp_dir()
    }
}

/// The environment, with the power to change it: [`SetVar`] and
/// [`RemoveVar`], as well as everything [`Std`] reads. Put it in a root for
/// `effects::SetVar` and `effects::RemoveVar`, next to `Std`.
///
/// Changing the environment is unsafe in Rust (see `std::env::set_var`):
/// on most platforms, a thread reading it while another changes it is
/// undefined behaviour, and C code reads it without any lock. So this can
/// only be made with an `unsafe` promise, and it cannot move to another
/// thread.
#[derive(Debug, Clone, Copy)]
pub struct WritableEnv {
    /// Not `Send` or `Sync`: it is promised to one thread.
    _one_thread: PhantomData<*const ()>,
}

impl WritableEnv {
    /// The environment, writable.
    ///
    /// # Safety
    ///
    /// The same as for `std::env::set_var`: while this value or a copy of
    /// it is used to change the environment, no other thread may read or
    /// change it, including through `Std`, `std::env` or C functions such
    /// as `getenv`. In practice: use it in a single-threaded program, or
    /// before any other thread starts.
    #[allow(unsafe_code)]
    pub unsafe fn new() -> Self {
        Self {
            _one_thread: PhantomData,
        }
    }
}

#[allow(unsafe_code)]
impl Perform<SetVar<'_>> for WritableEnv {
    fn perform(&mut self, op: SetVar<'_>) -> seam_core::io::Result<()> {
        op.check()?;
        // SAFETY: promised by the caller of `WritableEnv::new`, and the
        // value cannot have left that thread.
        unsafe { std::env::set_var(op.name, op.value) };
        Ok(())
    }
}

#[allow(unsafe_code)]
impl Perform<RemoveVar<'_>> for WritableEnv {
    fn perform(&mut self, op: RemoveVar<'_>) -> seam_core::io::Result<()> {
        op.check()?;
        // SAFETY: as for `SetVar`.
        unsafe { std::env::remove_var(op.name) };
        Ok(())
    }
}

/// Reading goes through `Std`.
macro_rules! reads {
    ($([$($gen:tt)*] $op:ty),*) => {$(
        impl<$($gen)*> Inspect<$op> for WritableEnv {
            fn inspect(&self, op: $op) -> <$op as seam_core::Operation>::Outcome {
                Std::new().inspect(op)
            }
        }
    )*};
}
reads!(['a] Var<'a>, [] Args, [] CurrentDir, [] TempDir);

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for WritableEnv {}
