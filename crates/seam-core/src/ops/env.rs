//! The process environment.
//!
//! Reading it is a query. Changing it is [`SetVar`] and [`RemoveVar`],
//! which std provides only through `seam_std::env::WritableEnv`: changing
//! the environment is unsafe while other threads may read it.
//!
//! Helpers: [`var_string`] reads a variable as UTF-8, and [`var_or`] falls
//! back to a default when it is not set.
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
use crate::three_ways::three_ways;
use std::borrow::ToOwned;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::string::String;
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
    /// check a handler makes before setting the variable.
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

    /// The check a handler makes before removing the variable.
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

crate::root::keys!(
    ['a] Var<'a> => Var,
    [] Args => Args,
    [] CurrentDir => CurrentDir,
    [] TempDir => TempDir,
    ['a] SetVar<'a> => SetVar,
    ['a] RemoveVar<'a> => RemoveVar,
);

three_ways! {
    /// The variable `name` as a `String`, or `None` if it is not set. Fails
    /// with `io::ErrorKind::InvalidData` if it is set but not UTF-8.
    pub fn var_string, var_string_async, var_string_send
    [C] (c: &C, name: &str) -> io::Result<Option<String>>
    where
        sync { C: for<'a> crate::Inspect<Var<'a>> }
        async { C: for<'a> crate::future::AsyncInspect<Var<'a>> }
        send { C: for<'a> crate::future::SendInspect<Var<'a>> + Sync }
    {
        match ask!(c, Var::new(name)) {
            None => Ok(None),
            Some(value) => value.into_string().map(Some).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the environment variable is not valid UTF-8",
                )
            }),
        }
    }

    /// The variable `name` as a `String`, or `default` if it is not set.
    /// Fails with `io::ErrorKind::InvalidData` if it is set but not UTF-8.
    pub fn var_or, var_or_async, var_or_send
    [C] (c: &C, name: &str, default: &str) -> io::Result<String>
    where
        sync { C: for<'a> crate::Inspect<Var<'a>> }
        async { C: for<'a> crate::future::AsyncInspect<Var<'a>> }
        send { C: for<'a> crate::future::SendInspect<Var<'a>> + Sync }
    {
        let value = call!(var_string, var_string_async, var_string_send; (c, name))?;
        Ok(value.unwrap_or_else(|| default.to_owned()))
    }
}
