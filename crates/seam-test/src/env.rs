use seam_core::io;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::vec::Vec;

use seam_core::Inspect;
use seam_core::ops::env::{Args, CurrentDir, TempDir, Var};

use crate::Sim;

#[derive(Debug)]
pub(crate) struct Env {
    vars: BTreeMap<OsString, OsString>,
    args: Vec<OsString>,
    temp: PathBuf,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            vars: BTreeMap::new(),
            args: Vec::from([OsString::from("sim")]),
            temp: PathBuf::from("/tmp"),
        }
    }
}

impl Sim {
    /// Sets an environment variable.
    pub fn set_var(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.env.vars.insert(name.into(), value.into());
        self
    }

    /// Removes an environment variable.
    pub fn remove_var(&mut self, name: impl Into<OsString>) -> &mut Self {
        self.env.vars.remove(&name.into());
        self
    }

    /// Sets the command-line arguments, including the program name. The
    /// default is `["sim"]`.
    pub fn set_args<I: IntoIterator<Item = A>, A: Into<OsString>>(&mut self, args: I) -> &mut Self {
        self.env.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the directory `TempDir` reports. The default is `/tmp`.
    pub fn set_temp_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.env.temp = path.into();
        self
    }

    /// Sets the current directory. Relative paths in filesystem operations
    /// are resolved against it.
    pub fn set_current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.cwd = self.cwd.join(path);
        self
    }
}

impl Inspect<Var<'_>> for Sim {
    fn inspect(&self, op: Var<'_>) -> Option<OsString> {
        self.env.vars.get(op.name).cloned()
    }
}

impl Inspect<Args> for Sim {
    fn inspect(&self, _: Args) -> Vec<OsString> {
        self.env.args.clone()
    }
}

impl Inspect<CurrentDir> for Sim {
    fn inspect(&self, _: CurrentDir) -> io::Result<PathBuf> {
        Ok(self.cwd.clone())
    }
}

impl Inspect<TempDir> for Sim {
    fn inspect(&self, _: TempDir) -> PathBuf {
        self.env.temp.clone()
    }
}
