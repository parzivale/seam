//! The process environment.
//!
//! Reading it is a query. Changing it is [`SetVar`] and [`RemoveVar`],
//! which std provides only through `seam_std::env::WritableEnv`: changing
//! the environment is unsafe while other threads may read it.
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

/// Set an environment variable, for this process and the commands it runs
/// afterwards. Fails with `io::ErrorKind::InvalidInput` if the name is
/// empty or holds `=` or a NUL byte, or the value holds a NUL byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetVar<'a> {
    /// The variable's name.
    pub name: &'a OsStr,
    /// Its new value.
    pub value: &'a OsStr,
}

/// Remove an environment variable. Removing one that is not set succeeds.
/// Fails with `io::ErrorKind::InvalidInput` for a name [`SetVar`] would
/// refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveVar<'a> {
    /// The variable's name.
    pub name: &'a OsStr,
}

impl<'a> SetVar<'a> {
    /// Describes setting `name` to `value`.
    pub fn new(
        name: &'a (impl AsRef<OsStr> + ?Sized),
        value: &'a (impl AsRef<OsStr> + ?Sized),
    ) -> Self {
        Self {
            name: name.as_ref(),
            value: value.as_ref(),
        }
    }

    /// Whether the name and value are ones every platform accepts: the
    /// check a backend makes before setting the variable.
    pub fn check(&self) -> io::Result<()> {
        check_name(self.name)?;
        if self.value.as_encoded_bytes().contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an environment variable's value cannot hold a NUL byte",
            ));
        }
        Ok(())
    }
}

impl<'a> RemoveVar<'a> {
    /// Describes removing `name`.
    pub fn new(name: &'a (impl AsRef<OsStr> + ?Sized)) -> Self {
        Self {
            name: name.as_ref(),
        }
    }

    /// The check a backend makes before removing the variable.
    pub fn check(&self) -> io::Result<()> {
        check_name(self.name)
    }
}

fn check_name(name: &OsStr) -> io::Result<()> {
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty() || bytes.contains(&b'=') || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an environment variable's name must be non-empty, with no `=` or NUL byte",
        ));
    }
    Ok(())
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
    SetVar<'_> => io::Result<()>;
    RemoveVar<'_> => io::Result<()>;
}

crate::root::in_kind!(crate::kinds::Env: ['a] Var<'a>, [] Args, [] CurrentDir, [] TempDir,
    ['a] SetVar<'a>, ['a] RemoveVar<'a>);
