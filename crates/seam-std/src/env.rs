use std::ffi::OsString;
use std::path::PathBuf;

use seam_core::Inspect;
use seam_core::ops::env::{Args, CurrentDir, TempDir, Var};

use crate::Std;

impl Inspect<Var> for Std {
    fn inspect(&self, op: Var) -> Option<OsString> {
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
