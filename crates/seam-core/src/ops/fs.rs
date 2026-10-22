//! The filesystem.
//!
//! Whole-file operations ([`ReadFile`], [`WriteFile`], ...) are performed on
//! the backend. [`Open`] and [`CreateAtomic`] are acquisitions: a core
//! acquires the file for the length of a scope (see
//! [`acquire`](crate::acquire)), and performs the operations in
//! [`io`](super::io) on it there. Paths are owned, so operations can be sent to
//! another thread or held across an `.await`.
//!
//! Reading operations are [`Query`](crate::Query)s, so a core that only
//! reads can ask for `Inspect`:
//!
//! ```
//! use std::io;
//! use seam_core::Inspect;
//! use seam_core::ops::fs::{Metadata, ReadToString};
//!
//! /// Reads a config file, if there is one.
//! fn load_config<C>(c: &C, path: &str) -> io::Result<Option<String>>
//! where
//!     C: Inspect<Metadata> + Inspect<ReadToString>,
//! {
//!     match c.inspect(Metadata::new(path)) {
//!         Ok(info) if info.is_file() => c.inspect(ReadToString::new(path)).map(Some),
//!         Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "config is not a file")),
//!         Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
//!         Err(e) => Err(e),
//!     }
//! }
//! ```

use core::marker::PhantomData;
use std::io;
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

/// Read a whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFile {
    /// The file to read.
    pub path: PathBuf,
}

/// Read a whole file as UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadToString {
    /// The file to read.
    pub path: PathBuf,
}

/// Replace a file's contents, creating it if needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFile {
    /// The file to write.
    pub path: PathBuf,
    /// The new contents.
    pub contents: Vec<u8>,
}

/// Look up a path's metadata, following symlinks. Produces a [`FileInfo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// The path to look up.
    pub path: PathBuf,
}

/// List the paths in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadDir {
    /// The directory to list.
    pub path: PathBuf,
}

/// Create a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDir {
    /// The directory to create.
    pub path: PathBuf,
    /// Also create missing parents, and succeed if it already exists.
    pub recursive: bool,
}

/// Remove a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveFile {
    /// The file to remove.
    pub path: PathBuf,
}

/// Remove a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveDir {
    /// The directory to remove.
    pub path: PathBuf,
    /// Also remove everything inside it. Otherwise it must be empty.
    pub recursive: bool,
}

/// Rename or move a file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    /// The current path.
    pub from: PathBuf,
    /// The new path.
    pub to: PathBuf,
}

/// Copy a file's contents. Returns the number of bytes copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFile {
    /// The file to copy.
    pub from: PathBuf,
    /// Where to copy it.
    pub to: PathBuf,
}

/// Open a file, producing a handle of type `F`.
///
/// `F` is the backend's file type, for example `seam_std::fs::File`.
pub struct Open<F> {
    /// The file to open.
    pub path: PathBuf,
    /// How to open it.
    pub options: OpenOptions,
    handle: PhantomData<fn() -> F>,
}

/// Create a file that atomically replaces `path` when it is closed,
/// producing a handle of type `F`.
///
/// Writes go to a temporary file beside `path`. When the scope succeeds, the
/// provider syncs it and renames it over `path` in one step, so readers see
/// either the old contents or the new, never a mix. When the scope fails,
/// panics or is cancelled, the temporary file is deleted and `path` is left
/// untouched.
///
/// Temporary files are named `.<name>.seam-<suffix>.tmp`. If the process is
/// killed outright, one can be left behind; [`SweepAtomic`] removes them.
///
/// ```
/// use std::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::fs::CreateAtomic;
/// use seam_core::ops::io::WriteAll;
///
/// /// Saves settings. A crash or error halfway leaves the old file intact.
/// fn save<C, F>(c: &mut C, settings: &[(&str, &str)]) -> io::Result<()>
/// where
///     C: Provide<CreateAtomic<F>>,
///     F: for<'a> Perform<WriteAll<'a>>,
/// {
///     c.acquire(CreateAtomic::new("settings.conf"), |_c, file| {
///         for (key, value) in settings {
///             file.perform(WriteAll(format!("{key} = {value}\n").as_bytes()))?;
///         }
///         Ok(())
///     })
/// }
/// ```
pub struct CreateAtomic<F> {
    /// The file to replace.
    pub path: PathBuf,
    handle: PhantomData<fn() -> F>,
}

impl<F> CreateAtomic<F> {
    /// Describes atomically replacing `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            handle: PhantomData,
        }
    }
}

impl<F> Clone for CreateAtomic<F> {
    fn clone(&self) -> Self {
        Self::new(self.path.clone())
    }
}

impl<F> core::fmt::Debug for CreateAtomic<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CreateAtomic")
            .field("path", &self.path)
            .finish()
    }
}

impl<F> crate::Acquisition for CreateAtomic<F> {
    type Resource = F;
    type Error = io::Error;
}

/// Delete temporary files left in `dir` by [`CreateAtomic`] writes that
/// never finished, because the process was killed before it could clean up.
/// Returns how many were removed.
///
/// Only files whose last modification is at least `older_than` ago are
/// removed, so that writes still in progress, in this process or another,
/// are left alone. Pick an age longer than any write takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepAtomic {
    /// The directory to sweep. Subdirectories are not visited.
    pub dir: PathBuf,
    /// How old a temporary file must be to count as abandoned.
    pub older_than: core::time::Duration,
}

impl SweepAtomic {
    /// Describes sweeping `dir` of temporary files at least `older_than` old.
    pub fn new(dir: impl Into<PathBuf>, older_than: core::time::Duration) -> Self {
        Self {
            dir: dir.into(),
            older_than,
        }
    }

    /// Whether `name` is a temporary file name from [`CreateAtomic`].
    pub fn is_temp_name(name: &std::ffi::OsStr) -> bool {
        name.to_str()
            .is_some_and(|n| n.starts_with('.') && n.ends_with(".tmp") && n.contains(".seam-"))
    }
}

/// How to open a file. Mirrors `std::fs::OpenOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    pub create: bool,
    pub create_new: bool,
}

/// What [`Metadata`] reports about a path.
///
/// A seam type rather than `std::fs::Metadata`, which cannot be constructed
/// outside std, so that test backends can produce one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    /// What the path is.
    pub kind: FileKind,
    /// The size in bytes.
    pub len: u64,
    /// Whether the permissions forbid writing.
    pub readonly: bool,
    /// The last modification time, if the platform records one.
    pub modified: Option<std::time::SystemTime>,
}

/// The kind of thing a path names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Symlink,
    /// Anything else: a socket, a device, a FIFO.
    Other,
}

impl FileInfo {
    /// Whether this is a regular file.
    pub fn is_file(&self) -> bool {
        self.kind == FileKind::File
    }

    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir
    }
}

macro_rules! path_ops {
    ($($ty:ident),*) => {$(
        impl $ty {
            /// Describes the operation on `path`.
            pub fn new(path: impl Into<PathBuf>) -> Self {
                Self { path: path.into() }
            }
        }
    )*};
}
path_ops!(ReadFile, ReadToString, Metadata, ReadDir, RemoveFile);

impl WriteFile {
    /// Describes writing `contents` to `path`.
    pub fn new(path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

impl CreateDir {
    /// Describes creating `path`. Fails if a parent is missing.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recursive: false,
        }
    }

    /// Describes creating `path` and any missing parents.
    pub fn all(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recursive: true,
        }
    }
}

impl RemoveDir {
    /// Describes removing the empty directory `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recursive: false,
        }
    }

    /// Describes removing `path` and everything inside it.
    pub fn all(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recursive: true,
        }
    }
}

impl Rename {
    /// Describes renaming `from` to `to`.
    pub fn new(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

impl CopyFile {
    /// Describes copying `from` to `to`.
    pub fn new(from: impl Into<PathBuf>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

impl<F> Open<F> {
    /// Describes opening `path` with `options`.
    pub fn new(path: impl Into<PathBuf>, options: OpenOptions) -> Self {
        Self {
            path: path.into(),
            options,
            handle: PhantomData,
        }
    }

    /// Describes opening `path` for reading.
    pub fn read(path: impl Into<PathBuf>) -> Self {
        Self::new(
            path,
            OpenOptions {
                read: true,
                ..OpenOptions::default()
            },
        )
    }

    /// Describes creating or truncating `path` for writing.
    pub fn create(path: impl Into<PathBuf>) -> Self {
        Self::new(
            path,
            OpenOptions {
                write: true,
                create: true,
                truncate: true,
                ..OpenOptions::default()
            },
        )
    }

    /// Describes opening `path` for appending, creating it if needed.
    pub fn append(path: impl Into<PathBuf>) -> Self {
        Self::new(
            path,
            OpenOptions {
                append: true,
                create: true,
                ..OpenOptions::default()
            },
        )
    }
}

impl<F> Clone for Open<F> {
    fn clone(&self) -> Self {
        Self::new(self.path.clone(), self.options)
    }
}

impl<F> core::fmt::Debug for Open<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Open")
            .field("path", &self.path)
            .field("options", &self.options)
            .finish()
    }
}

operations! {
    #[query] ReadFile => io::Result<Vec<u8>>;
    #[query] ReadToString => io::Result<String>;
    #[query] Metadata => io::Result<FileInfo>;
    #[query] ReadDir => io::Result<Vec<PathBuf>>;
    WriteFile => io::Result<()>;
    CreateDir => io::Result<()>;
    RemoveFile => io::Result<()>;
    RemoveDir => io::Result<()>;
    Rename => io::Result<()>;
    CopyFile => io::Result<u64>;
    SweepAtomic => io::Result<u64>;
}

impl<F> crate::Acquisition for Open<F> {
    type Resource = F;
    type Error = io::Error;
}
