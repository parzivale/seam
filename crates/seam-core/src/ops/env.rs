//! The process environment.
//!
//! All of these are queries.
//!
//! ```
//! use std::path::PathBuf;
//! use seam_core::Inspect;
//! use seam_core::ops::env::{CurrentDir, Var};
//!
//! /// `$XDG_CONFIG_HOME`, or `./.config`.
//! fn config_dir<C: Inspect<Var> + Inspect<CurrentDir>>(c: &C) -> std::io::Result<PathBuf> {
//!     match c.inspect(Var::new("XDG_CONFIG_HOME")) {
//!         Some(dir) => Ok(dir.into()),
//!         None => Ok(c.inspect(CurrentDir)?.join(".config")),
//!     }
//! }
//! ```

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::vec::Vec;

/// Read an environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    /// The variable's name.
    pub name: OsString,
}

/// Read the command-line arguments, including the program name.
#[derive(Debug, Clone, Copy, Default)]
pub struct Args;

/// Read the current working directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct CurrentDir;

/// Read the temporary-files directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct TempDir;

impl Var {
    /// Describes reading the variable `name`.
    pub fn new(name: impl Into<OsString>) -> Self {
        Self { name: name.into() }
    }
}

operations! {
    #[query] Var => Option<OsString>;
    #[query] Args => Vec<OsString>;
    #[query] CurrentDir => io::Result<PathBuf>;
    #[query] TempDir => PathBuf;
}

crate::root::in_kind!(crate::kinds::Env: [] Var, [] Args, [] CurrentDir, [] TempDir);
