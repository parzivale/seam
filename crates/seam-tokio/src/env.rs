use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use seam_core::Inspect;
use seam_core::ops::env::{Args, CurrentDir, TempDir, Var};

use crate::Tokio;

impl Inspect<Var> for Tokio {
    fn inspect(&self, op: Var) -> Option<OsString> {
        std::env::var_os(op.name)
    }
}

impl Inspect<Args> for Tokio {
    fn inspect(&self, _: Args) -> Vec<OsString> {
        std::env::args_os().collect()
    }
}

impl Inspect<CurrentDir> for Tokio {
    fn inspect(&self, _: CurrentDir) -> io::Result<PathBuf> {
        std::env::current_dir()
    }
}

impl Inspect<TempDir> for Tokio {
    fn inspect(&self, _: TempDir) -> PathBuf {
        std::env::temp_dir()
    }
}
