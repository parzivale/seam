//! The filesystem.
//!
//! Whole-file operations ([`ReadFile`], [`WriteFile`], ...) are performed on
//! the backend. [`Open`] and [`CreateAtomic`] are acquisitions: a core
//! acquires the file for the length of a scope (see
//! [`acquire`](crate::acquire)), and performs the operations in
//! [`io`](super::io) on it there. Operations borrow their paths and
//! contents, so describing one allocates nothing. [`ReadDir`] is an
//! acquisition too, listing a directory one entry at a time.
//!
//! Reading operations are [`Query`](crate::Query)s, so a core that only
//! reads can ask for `Inspect`:
//!
//! ```
//! use seam_core::io;
//! use seam_core::Inspect;
//! use seam_core::ops::fs::{Metadata, ReadToString};
//!
//! /// Reads a config file, if there is one.
//! fn load_config<C>(c: &C, path: &str) -> io::Result<Option<String>>
//! where
//!     C: for<'a> Inspect<Metadata<'a>> + for<'a> Inspect<ReadToString<'a>>,
//! {
//!     match c.inspect(Metadata::new(path)) {
//!         Ok(info) if info.is_file() => c.inspect(ReadToString::new(path)).map(Some),
//!         Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "config is not a file")),
//!         Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
//!         Err(e) => Err(e),
//!     }
//! }
//! ```

use crate::io;
use core::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

/// Read a whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadFile<'a> {
    /// The file to read.
    pub path: &'a Path,
}

/// Read a whole file as UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadToString<'a> {
    /// The file to read.
    pub path: &'a Path,
}

/// Replace a file's contents, creating it if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteFile<'a> {
    /// The file to write.
    pub path: &'a Path,
    /// The new contents.
    pub contents: &'a [u8],
}

/// Look up a path's metadata, following symlinks. Produces a [`FileInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata<'a> {
    /// The path to look up.
    pub path: &'a Path,
}

/// Open a directory to list it, producing a handle of type `D`. Perform
/// [`NextEntry`] on the handle for each path in it, in no particular
/// order.
///
/// ```
/// use seam_core::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::fs::{NextEntry, ReadDir};
///
/// /// Counts the `.log` files in `dir`, one entry at a time.
/// fn count_logs<C, D>(c: &mut C, dir: &str) -> io::Result<usize>
/// where
///     C: for<'a> Provide<ReadDir<'a, D>>,
///     D: Perform<NextEntry>,
/// {
///     c.acquire(ReadDir::new(dir), |_c, entries| {
///         let mut n = 0;
///         while let Some(path) = entries.perform(NextEntry)? {
///             n += usize::from(path.extension().is_some_and(|e| e == "log"));
///         }
///         Ok(n)
///     })
/// }
/// ```
pub struct ReadDir<'a, D> {
    /// The directory to list.
    pub path: &'a Path,
    handle: PhantomData<fn() -> D>,
}

/// The next path in a directory opened with [`ReadDir`], or `None` once
/// every one has been listed. The path is the directory's path joined with
/// the entry's name.
#[derive(Debug, Clone, Copy, Default)]
pub struct NextEntry;

/// Create a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateDir<'a> {
    /// The directory to create.
    pub path: &'a Path,
    /// Also create missing parents, and succeed if it already exists.
    pub recursive: bool,
}

/// Remove a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveFile<'a> {
    /// The file to remove.
    pub path: &'a Path,
}

/// Remove a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveDir<'a> {
    /// The directory to remove.
    pub path: &'a Path,
    /// Also remove everything inside it. Otherwise it must be empty.
    pub recursive: bool,
}

/// Rename or move a file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rename<'a> {
    /// The current path.
    pub from: &'a Path,
    /// The new path.
    pub to: &'a Path,
}

/// Copy a file's contents. Returns the number of bytes copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyFile<'a> {
    /// The file to copy.
    pub from: &'a Path,
    /// Where to copy it.
    pub to: &'a Path,
}

/// Open a file, producing a handle of type `F`.
///
/// `F` is the backend's file type, for example `seam_std::fs::File`.
pub struct Open<'a, F> {
    /// The file to open.
    pub path: &'a Path,
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
/// use seam_core::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::fs::CreateAtomic;
/// use seam_core::ops::io::WriteAll;
///
/// /// Saves settings. A crash or error halfway leaves the old file intact.
/// fn save<C, F>(c: &mut C, settings: &[(&str, &str)]) -> io::Result<()>
/// where
///     C: for<'a> Provide<CreateAtomic<'a, F>>,
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
pub struct CreateAtomic<'a, F> {
    /// The file to replace.
    pub path: &'a Path,
    handle: PhantomData<fn() -> F>,
}

impl<'a, F> CreateAtomic<'a, F> {
    /// Describes atomically replacing `path`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            handle: PhantomData,
        }
    }
}

/// Delete temporary files left in `dir` by [`CreateAtomic`] writes that
/// never finished, because the process was killed before it could clean up.
/// Returns how many were removed.
///
/// Only files whose last modification is at least `older_than` ago are
/// removed, so that writes still in progress, in this process or another,
/// are left alone. Pick an age longer than any write takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepAtomic<'a> {
    /// The directory to sweep. Subdirectories are not visited.
    pub dir: &'a Path,
    /// How old a temporary file must be to count as abandoned.
    pub older_than: core::time::Duration,
}

impl<'a> SweepAtomic<'a> {
    /// Describes sweeping `dir` of temporary files at least `older_than` old.
    pub fn new(dir: &'a (impl AsRef<Path> + ?Sized), older_than: core::time::Duration) -> Self {
        Self {
            dir: dir.as_ref(),
            older_than,
        }
    }
}

impl SweepAtomic<'_> {
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
        impl<'a> $ty<'a> {
            /// Describes the operation on `path`.
            pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
                Self { path: path.as_ref() }
            }
        }
    )*};
}
path_ops!(ReadFile, ReadToString, Metadata, RemoveFile);

impl<'a> WriteFile<'a> {
    /// Describes writing `contents` to `path`.
    pub fn new(
        path: &'a (impl AsRef<Path> + ?Sized),
        contents: &'a (impl AsRef<[u8]> + ?Sized),
    ) -> Self {
        Self {
            path: path.as_ref(),
            contents: contents.as_ref(),
        }
    }
}

impl<'a> CreateDir<'a> {
    /// Describes creating `path`. Fails if a parent is missing.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: false,
        }
    }

    /// Describes creating `path` and any missing parents.
    pub fn all(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: true,
        }
    }
}

impl<'a> RemoveDir<'a> {
    /// Describes removing the empty directory `path`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: false,
        }
    }

    /// Describes removing `path` and everything inside it.
    pub fn all(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: true,
        }
    }
}

impl<'a> Rename<'a> {
    /// Describes renaming `from` to `to`.
    pub fn new(from: &'a (impl AsRef<Path> + ?Sized), to: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            from: from.as_ref(),
            to: to.as_ref(),
        }
    }
}

impl<'a> CopyFile<'a> {
    /// Describes copying `from` to `to`.
    pub fn new(from: &'a (impl AsRef<Path> + ?Sized), to: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            from: from.as_ref(),
            to: to.as_ref(),
        }
    }
}

impl<'a, D> ReadDir<'a, D> {
    /// Describes listing `path`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            handle: PhantomData,
        }
    }
}

impl<'a, F> Open<'a, F> {
    /// Describes opening `path` with `options`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized), options: OpenOptions) -> Self {
        Self {
            path: path.as_ref(),
            options,
            handle: PhantomData,
        }
    }

    /// Describes opening `path` for reading.
    pub fn read(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self::new(
            path,
            OpenOptions {
                read: true,
                ..OpenOptions::default()
            },
        )
    }

    /// Describes creating or truncating `path` for writing.
    pub fn create(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
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
    pub fn append(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
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

/// `Clone`, `Copy` and `Debug` for the acquisitions, whatever their handle
/// type.
macro_rules! handle_ops {
    ($($ty:ident { $($field:ident),* }),*) => {$(
        impl<F> Clone for $ty<'_, F> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<F> Copy for $ty<'_, F> {}

        impl<F> core::fmt::Debug for $ty<'_, F> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty))$(.field(stringify!($field), &self.$field))*.finish()
            }
        }

        impl<F> crate::Acquisition for $ty<'_, F> {
            type Resource = F;
            type Error = io::Error;
        }
    )*};
}
handle_ops!(
    Open { path, options },
    CreateAtomic { path },
    ReadDir { path }
);

operations! {
    #[query] ReadFile<'_> => io::Result<Vec<u8>>;
    #[query] ReadToString<'_> => io::Result<String>;
    #[query] Metadata<'_> => io::Result<FileInfo>;
    NextEntry => io::Result<Option<PathBuf>>;
    WriteFile<'_> => io::Result<()>;
    CreateDir<'_> => io::Result<()>;
    RemoveFile<'_> => io::Result<()>;
    RemoveDir<'_> => io::Result<()>;
    Rename<'_> => io::Result<()>;
    CopyFile<'_> => io::Result<u64>;
    SweepAtomic<'_> => io::Result<u64>;
}

crate::root::in_kind!(crate::kinds::Fs:
    ['a] ReadFile<'a>, ['a] ReadToString<'a>, ['a] WriteFile<'a>, ['a] Metadata<'a>,
    ['a] CreateDir<'a>, ['a] RemoveFile<'a>, ['a] RemoveDir<'a>, ['a] Rename<'a>,
    ['a] CopyFile<'a>, ['a] SweepAtomic<'a>,
    ['a, F] Open<'a, F>, ['a, F] CreateAtomic<'a, F>, ['a, D] ReadDir<'a, D>,
);
