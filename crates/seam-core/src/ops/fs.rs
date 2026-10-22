//! The filesystem.
//!
//! Handle operations on an open file are in [`io`](super::io).

use core::marker::PhantomData;
use std::fs;
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

/// Look up a path's metadata, following symlinks.
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
    #[query] Metadata => io::Result<fs::Metadata>;
    #[query] ReadDir => io::Result<Vec<PathBuf>>;
    WriteFile => io::Result<()>;
    CreateDir => io::Result<()>;
    RemoveFile => io::Result<()>;
    RemoveDir => io::Result<()>;
    Rename => io::Result<()>;
    CopyFile => io::Result<u64>;
}

impl<F> crate::Operation for Open<F> {
    type Outcome = io::Result<F>;
}
