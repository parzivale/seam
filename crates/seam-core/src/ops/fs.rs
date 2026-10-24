//! The filesystem.
//!
//! Files are opened as handles: [`Open`] and [`CreateAtomic`] are
//! acquisitions, lending a file to a scope (see [`acquire`](crate::acquire)),
//! where the [`io`](super::io) operations read and write it, along with the
//! file operations here: [`Stat`], [`SetLen`], [`SyncAll`] and [`SyncData`].
//! [`View`] opens a regular file to read it at offsets, with [`ReadAt`]: a
//! scope that is a query, so it needs only shared access.
//! [`ReadDir`] lends a directory listing, stepped through with
//! [`NextEntry`]. Operations borrow their paths, so describing one
//! allocates nothing.
//!
//! An [`Open`] names its mode as a type: [`ReadOnly`], [`WriteOnly`],
//! [`Append`] or [`ReadWrite`]. A backend's file handle implements `Read`
//! only for readable modes and `Write` only for writable ones, so a core
//! bounded on `Open<'a, ReadOnly, F>` cannot write, and its bounds say so.
//!
//! Whole-file work is built on those, as plain functions: [`read_file`],
//! [`read_to_string`], [`write_file`], [`create_dir_all`] and
//! [`sweep_atomic`], each in three forms (sync, `_async`, `_send`).
//!
//! ```
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::fs::{self, Metadata, ReadAt, View};
//!
//! /// Reads a config file, if there is one. It can look up and read files,
//! /// and nothing else: its bounds are all queries, so it needs only `&C`.
//! fn load_config<C, F>(c: &C, path: &str) -> io::Result<Option<String>>
//! where
//!     C: for<'a> Inspect<Metadata<'a>> + for<'a> Lend<View<'a, F>>,
//!     F: for<'a> Inspect<ReadAt<'a>>,
//! {
//!     match c.inspect(Metadata::new(path)) {
//!         Ok(info) if info.is_file() => fs::read_to_string(c, path).map(Some),
//!         Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "config is not a file")),
//!         Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
//!         Err(e) => Err(e),
//!     }
//! }
//! ```

#[allow(unused_imports)]
use super::io::{Read, Write};
use crate::io;
use crate::three_ways::three_ways;
use core::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

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

/// Truncate or extend an open file to `len` bytes. Extending fills with
/// zeros. The position is unchanged. Performed on a writable file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetLen(pub u64);

/// Write an open file's data and metadata through to the disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncAll;

/// Write an open file's data through to the disk, with only the metadata
/// needed to read it back (not, for example, its modification time).
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncData;

/// Look up an open file's metadata. Produces a [`FileInfo`], as
/// [`Metadata`] does for a path.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stat;

/// Create one directory. Fails with `io::ErrorKind::AlreadyExists` if the
/// path exists, and `io::ErrorKind::NotFound` if its parent does not;
/// [`create_dir_all`] creates the parents too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateDir<'a> {
    /// The directory to create.
    pub path: &'a Path,
}

/// Remove a file, or a symlink (not what it points to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveFile<'a> {
    /// The file to remove.
    pub path: &'a Path,
}

/// Remove a directory.
///
/// Removing everything inside is a primitive too, rather than a walk built
/// on [`RemoveFile`]: a walk that follows a symlink to a directory deletes
/// what the link points to, and doing it safely while other processes
/// change the tree takes platform calls a core does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveDir<'a> {
    /// The directory to remove.
    pub path: &'a Path,
    /// Also remove everything inside it, without following symlinks.
    /// Otherwise it must be empty.
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

/// Copy a file's contents and permissions. Returns the number of bytes
/// copied.
///
/// A primitive rather than a loop of reads and writes, because platforms
/// copy faster than that (reflinks, `copy_file_range`) and copy
/// permissions with the contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyFile<'a> {
    /// The file to copy.
    pub from: &'a Path,
    /// Where to copy it.
    pub to: &'a Path,
}

/// The mode an [`Open`] opens a file in, as a type: [`ReadOnly`],
/// [`WriteOnly`], [`Append`] or [`ReadWrite`]. Sealed: backends match on
/// these four.
pub trait Mode: sealed::Sealed + Send + Sync + 'static {
    /// Whether the file is opened for reading.
    const READ: bool;
    /// Whether the file is opened for writing.
    const WRITE: bool;
    /// Whether every write goes to the end.
    const APPEND: bool;
}

/// A mode that can read: a file opened in it performs [`Read`].
pub trait Readable: Mode {}

/// A mode that can write: a file opened in it performs [`Write`],
/// [`SetLen`] and the syncs.
pub trait Writable: Mode {}

mod sealed {
    pub trait Sealed {}
}

macro_rules! modes {
    ($($(#[$m:meta])* $ty:ident: $read:literal $write:literal $append:literal [$($cap:ident)*];)*) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $ty {}

        impl sealed::Sealed for $ty {}

        impl Mode for $ty {
            const READ: bool = $read;
            const WRITE: bool = $write;
            const APPEND: bool = $append;
        }

        $(impl $cap for $ty {})*
    )*};
}

modes! {
    /// Read only.
    ReadOnly: true false false [Readable];
    /// Write only.
    WriteOnly: false true false [Writable];
    /// Write only, every write at the end.
    Append: false true true [Writable];
    /// Read and write.
    ReadWrite: true true false [Readable Writable];
}

/// Open a file in mode `M`, producing a handle of type `F`: the backend's
/// file type for that mode, for example `seam_std::fs::File<M>`.
///
/// When the scope succeeds, a file opened for writing is flushed and
/// synced, and an error doing so is the scope's error.
pub struct Open<'a, M, F> {
    /// The file to open.
    pub path: &'a Path,
    /// Whether to create or truncate it.
    pub options: OpenOptions,
    mode: PhantomData<fn() -> M>,
    handle: PhantomData<fn() -> F>,
}

/// How [`Open`] treats a file that does or does not exist. What `std`
/// calls read, write and append are the mode, a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenOptions {
    /// Create the file if it is missing.
    pub create: bool,
    /// Create the file, failing with `io::ErrorKind::AlreadyExists` if it
    /// exists. Takes precedence over `create`.
    pub create_new: bool,
    /// Empty the file when it is opened. Only for writable modes.
    pub truncate: bool,
}

/// Create a file that atomically replaces `path` when it is closed,
/// producing a writable handle of type `F`.
///
/// Writes go to a temporary file beside `path`. When the scope succeeds, the
/// provider syncs it and renames it over `path` in one step, so readers see
/// either the old contents or the new, never a mix. When the scope fails,
/// panics or is cancelled, the temporary file is deleted and `path` is left
/// untouched.
///
/// A primitive, because that cleanup must run when the scope panics or is
/// cancelled, in `Drop`, where a core cannot perform effects.
///
/// Temporary files are named `.<name>.seam-<suffix>.tmp`. If the process is
/// killed outright, one can be left behind; [`sweep_atomic`] removes them.
///
/// ```
/// use seam_core::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::fs::CreateAtomic;
/// use seam_core::ops::io::{Write, write_all};
///
/// /// Saves settings. A crash or error halfway leaves the old file intact.
/// fn save<C, F>(c: &mut C, settings: &[(&str, &str)]) -> io::Result<()>
/// where
///     C: for<'a> Provide<CreateAtomic<'a, F>>,
///     F: for<'a> Perform<Write<'a>>,
/// {
///     c.acquire(CreateAtomic::new("settings.conf"), |_c, file| {
///         for (key, value) in settings {
///             write_all(file, format!("{key} = {value}\n").as_bytes())?;
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

/// Open the regular file at `path` to read it at offsets, producing a
/// handle of type `F`, which performs [`ReadAt`] and nothing else.
///
/// A scope that is a query: the handle has no cursor and cannot write, so
/// nothing outside the scope changes, and it is lent, with
/// [`Lend`](crate::Lend), from shared access. [`read_file`] and
/// [`read_to_string`] are built on it.
///
/// Opening fails with `io::ErrorKind::IsADirectory` for a directory, and
/// `io::ErrorKind::InvalidInput` for anything else that is not a regular
/// file: a FIFO or a device can block, or lose what is read, so open those
/// with [`Open`], in a scope that is not a query.
///
/// ```
/// use seam_core::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::fs::{ReadAt, View};
///
/// /// The first four bytes of a file, from shared access.
/// fn magic<C, F>(c: &C, path: &str) -> io::Result<[u8; 4]>
/// where
///     C: for<'a> Lend<View<'a, F>>,
///     F: for<'a> Inspect<ReadAt<'a>>,
/// {
///     c.lend(View::new(path), |_, file| {
///         let mut magic = [0; 4];
///         let n = file.inspect(ReadAt::new(0, &mut magic))?;
///         if n < magic.len() {
///             return Err(io::ErrorKind::UnexpectedEof.into());
///         }
///         Ok(magic)
///     })
/// }
/// ```
pub struct View<'a, F> {
    /// The file to read.
    pub path: &'a Path,
    handle: PhantomData<fn() -> F>,
}

impl<'a, F> View<'a, F> {
    /// Describes opening `path` to read it at offsets.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            handle: PhantomData,
        }
    }
}

/// Read from a file at `offset` into `buf`, moving no cursor. Produces how
/// many bytes were read: 0 at the end of the file, and possibly fewer than
/// asked for before it.
///
/// A query: the same offset reads the same bytes until the file is written.
#[derive(Debug)]
pub struct ReadAt<'a> {
    /// Where in the file to start.
    pub offset: u64,
    /// Where to put what is read.
    pub buf: &'a mut [u8],
}

impl<'a> ReadAt<'a> {
    /// Describes reading into `buf` from `offset`.
    pub fn new(offset: u64, buf: &'a mut [u8]) -> Self {
        Self { offset, buf }
    }
}

/// Whether `name` is a temporary file name from [`CreateAtomic`].
pub fn is_temp_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|n| n.starts_with('.') && n.ends_with(".tmp") && n.contains(".seam-"))
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
path_ops!(Metadata, CreateDir, RemoveFile);

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

impl<'a, M: Mode, F> Open<'a, M, F> {
    /// Describes opening `path` in mode `M` with `options`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized), options: OpenOptions) -> Self {
        Self {
            path: path.as_ref(),
            options,
            mode: PhantomData,
            handle: PhantomData,
        }
    }
}

impl<'a, F> Open<'a, ReadOnly, F> {
    /// Describes opening the existing file `path` to read it.
    pub fn read(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self::new(path, OpenOptions::default())
    }
}

impl<'a, F> Open<'a, WriteOnly, F> {
    /// Describes creating `path`, or emptying it if it exists, to write it.
    pub fn create(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self::new(
            path,
            OpenOptions {
                create: true,
                truncate: true,
                ..OpenOptions::default()
            },
        )
    }

    /// Describes creating `path` to write it, failing if it exists.
    pub fn create_new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self::new(
            path,
            OpenOptions {
                create_new: true,
                ..OpenOptions::default()
            },
        )
    }
}

impl<'a, F> Open<'a, Append, F> {
    /// Describes opening `path` to add to its end, creating it if needed.
    pub fn append(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self::new(
            path,
            OpenOptions {
                create: true,
                ..OpenOptions::default()
            },
        )
    }
}

/// `Clone`, `Copy`, `Debug` and `Acquisition` for the acquisitions,
/// whatever their handle type.
macro_rules! handle_ops {
    ($([$($g:ident),*] $ty:ident { $($field:ident),* }),*) => {$(
        impl<$($g),*> Clone for $ty<'_, $($g),*> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<$($g),*> Copy for $ty<'_, $($g),*> {}

        impl<$($g),*> core::fmt::Debug for $ty<'_, $($g),*> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty))$(.field(stringify!($field), &self.$field))*.finish()
            }
        }
    )*};
}
handle_ops!(
    [M, F] Open { path, options },
    [F] CreateAtomic { path },
    [F] View { path },
    [D] ReadDir { path }
);

impl<M, F> crate::Acquisition for Open<'_, M, F> {
    type Resource = F;
    type Error = io::Error;
}

impl<F> crate::Acquisition for CreateAtomic<'_, F> {
    type Resource = F;
    type Error = io::Error;
}

impl<F> crate::Acquisition for View<'_, F> {
    type Resource = F;
    type Error = io::Error;
}

impl<D> crate::Acquisition for ReadDir<'_, D> {
    type Resource = D;
    type Error = io::Error;
}

operations! {
    #[query] Metadata<'_> => io::Result<FileInfo>;
    NextEntry => io::Result<Option<PathBuf>>;
    SetLen => io::Result<()>;
    SyncAll => io::Result<()>;
    SyncData => io::Result<()>;
    #[query] Stat => io::Result<FileInfo>;
    #[query] ReadAt<'_> => io::Result<usize>;
    CreateDir<'_> => io::Result<()>;
    RemoveFile<'_> => io::Result<()>;
    RemoveDir<'_> => io::Result<()>;
    Rename<'_> => io::Result<()>;
    CopyFile<'_> => io::Result<u64>;
}

crate::root::in_kind!(crate::kinds::Fs:
    ['a] Metadata<'a>, ['a] CreateDir<'a>, ['a] RemoveFile<'a>, ['a] RemoveDir<'a>,
    ['a] Rename<'a>, ['a] CopyFile<'a>,
    ['a, M, F] Open<'a, M, F>, ['a, F] CreateAtomic<'a, F>, ['a, F] View<'a, F>,
    ['a, D] ReadDir<'a, D>,
);

fn not_utf8() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "the file is not valid UTF-8")
}

three_ways! {
    /// Reads the whole regular file at `path`, through a [`View`], from
    /// shared access. For a FIFO or a device, open it with [`Open`] and use
    /// `io::read_to_end`.
    pub fn read_file, read_file_async, read_file_send
    [C, F] (c: &C, path: &(impl AsRef<Path> + ?Sized)) -> io::Result<Vec<u8>>
    where
        sync {
            C: for<'a> crate::Lend<View<'a, F>>,
            F: for<'a> crate::Inspect<ReadAt<'a>>,
        }
        async {
            C: for<'a> crate::AsyncLend<View<'a, F>>,
            F: for<'a> crate::future::AsyncInspect<ReadAt<'a>>,
        }
        send {
            C: for<'a> crate::SendLend<View<'a, F>> + Sync,
            F: for<'a> crate::future::SendInspect<ReadAt<'a>> + Sync,
        }
    {
        lend!(c, View::new(path), |_, file| {
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                match ask!(&*file, ReadAt::new(bytes.len() as u64, &mut chunk)) {
                    Ok(0) => return io::Result::Ok(bytes),
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
        })
    }

    /// Reads the whole regular file at `path` as UTF-8, as [`read_file`]
    /// does. Fails with `io::ErrorKind::InvalidData` if it is not UTF-8.
    pub fn read_to_string, read_to_string_async, read_to_string_send
    [C, F] (c: &C, path: &(impl AsRef<Path> + ?Sized)) -> io::Result<String>
    where
        sync {
            C: for<'a> crate::Lend<View<'a, F>>,
            F: for<'a> crate::Inspect<ReadAt<'a>>,
        }
        async {
            C: for<'a> crate::AsyncLend<View<'a, F>>,
            F: for<'a> crate::future::AsyncInspect<ReadAt<'a>>,
        }
        send {
            C: for<'a> crate::SendLend<View<'a, F>> + Sync,
            F: for<'a> crate::future::SendInspect<ReadAt<'a>> + Sync,
        }
    {
        let bytes = call!(read_file, read_file_async, read_file_send; (c, path))?;
        String::from_utf8(bytes).map_err(|_| not_utf8())
    }

    /// Replaces the contents of `path` with `contents`, creating it if
    /// needed. The file is synced when it is closed, as every file opened
    /// for writing is.
    pub fn write_file, write_file_async, write_file_send
    [C, F] (
        c: &mut C,
        path: &(impl AsRef<Path> + ?Sized),
        contents: &(impl AsRef<[u8]> + ?Sized),
    ) -> io::Result<()>
    where
        sync {
            C: for<'a> crate::Provide<Open<'a, WriteOnly, F>>,
            F: for<'a> crate::Perform<Write<'a>>,
        }
        async {
            C: for<'a> crate::AsyncProvide<Open<'a, WriteOnly, F>>,
            F: for<'a> crate::future::AsyncPerform<Write<'a>>,
        }
        send {
            C: for<'a> crate::SendProvide<Open<'a, WriteOnly, F>> + Send,
            F: for<'a> crate::future::SendPerform<Write<'a>> + Send,
        }
    {
        let contents = contents.as_ref();
        acquire!(c, Open::create(path), |_, file| {
            call!(super::io::write_all, super::io::write_all_async, super::io::write_all_send; (file, contents))
        })
    }

    /// Creates `path` and any missing parents. Succeeds if `path` is
    /// already a directory.
    pub fn create_dir_all, create_dir_all_async, create_dir_all_send
    [C] (c: &mut C, path: &(impl AsRef<Path> + ?Sized)) -> io::Result<()>
    where
        sync { C: for<'a> crate::Perform<CreateDir<'a>> + for<'a> crate::Inspect<Metadata<'a>> }
        async {
            C: for<'a> crate::future::AsyncPerform<CreateDir<'a>>
                + for<'a> crate::future::AsyncInspect<Metadata<'a>>,
        }
        send {
            C: for<'a> crate::future::SendPerform<CreateDir<'a>>
                + for<'a> crate::future::SendInspect<Metadata<'a>>
                + Send,
        }
    {
        let path = path.as_ref();
        // Outermost first: each directory is created after its parent.
        let ancestors: Vec<&Path> = path.ancestors().filter(|p| !p.as_os_str().is_empty()).collect();
        for dir in ancestors.into_iter().rev() {
            match run!(c, CreateDir::new(dir)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if !ask!(c, Metadata::new(dir))?.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "a path component is not a directory",
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(feature = "time")]
three_ways! {
    /// Deletes temporary files left in `dir` by [`CreateAtomic`] writes that
    /// never finished, because the process was killed before it could clean
    /// up. Returns how many were removed.
    ///
    /// Only files whose last modification is at least `older_than` ago, by
    /// the wall clock, are removed, so that writes still in progress, in this
    /// process or another, are left alone. Pick an age longer than any write
    /// takes. Subdirectories are not visited.
    pub fn sweep_atomic, sweep_atomic_async, sweep_atomic_send
    [C, D] (c: &mut C, dir: &(impl AsRef<Path> + ?Sized), older_than: core::time::Duration) -> io::Result<u64>
    where
        sync {
            C: for<'a> crate::Provide<ReadDir<'a, D>>
                + for<'a> crate::Inspect<Metadata<'a>>
                + for<'a> crate::Perform<RemoveFile<'a>>
                + crate::Inspect<crate::ops::time::WallClock>,
            D: crate::Perform<NextEntry>,
        }
        async {
            C: for<'a> crate::AsyncProvide<ReadDir<'a, D>>
                + for<'a> crate::future::AsyncInspect<Metadata<'a>>
                + for<'a> crate::future::AsyncPerform<RemoveFile<'a>>
                + crate::future::AsyncInspect<crate::ops::time::WallClock>,
            D: crate::future::AsyncPerform<NextEntry>,
        }
        send {
            C: for<'a> crate::SendProvide<ReadDir<'a, D>>
                + for<'a> crate::future::SendInspect<Metadata<'a>>
                + for<'a> crate::future::SendPerform<RemoveFile<'a>>
                + crate::future::SendInspect<crate::ops::time::WallClock>
                + Send,
            D: crate::future::SendPerform<NextEntry> + Send,
        }
    {
        let now = ask!(c, crate::ops::time::WallClock);
        acquire!(c, ReadDir::new(dir.as_ref()), |c, entries| {
            let mut removed = 0;
            while let Some(path) = run!(entries, NextEntry)? {
                if !path.file_name().is_some_and(is_temp_name) {
                    continue;
                }
                let info = match ask!(c, Metadata::new(&path)) {
                    Ok(info) => info,
                    // Finished or cleaned up by its writer in the meantime.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                let age = info
                    .modified
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or_default();
                if info.is_file() && age >= older_than {
                    match run!(c, RemoveFile::new(&path)) {
                        Ok(()) => removed += 1,
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            io::Result::Ok(removed)
        })
    }
}
