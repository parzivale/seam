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
//! fn config_dir<C>(c: &C) -> seam_core::io::Result<PathBuf>
//! where
//!     C: for<'a> Inspect<Var<'a>> + Inspect<CurrentDir>,
//! {
//!     match c.inspect(Var::new("XDG_CONFIG_HOME")) {
//!         Some(dir) => Ok(dir.into()),
//!         None => Ok(c.inspect(CurrentDir)?.join(".config")),
//!     }
//! }
//! ```

use crate::io;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::vec::Vec;

/// Read an environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Var<'a> {
    /// The variable's name.
    pub name: &'a OsStr,
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

impl<'a> Var<'a> {
    /// Describes reading the variable `name`.
    pub fn new(name: &'a (impl AsRef<OsStr> + ?Sized)) -> Self {
        Self {
            name: name.as_ref(),
        }
    }
}

operations! {
    #[query] Var<'_> => Option<OsString>;
    #[query] Args => Vec<OsString>;
    #[query] CurrentDir => io::Result<PathBuf>;
    #[query] TempDir => PathBuf;
}

crate::root::in_kind!(crate::kinds::Env: ['a] Var<'a>, [] Args, [] CurrentDir, [] TempDir);
