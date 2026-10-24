//! Filesystem operations, and the [`File`], [`AtomicFile`] and [`Dir`]
//! handles.

use core::marker::PhantomData;
use std::io;
use std::path::{Path, PathBuf};

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Mode, NextEntry, Open,
    ReadDir, Readable, RemoveDir, RemoveFile, Rename, SetLen, Stat, SyncAll, SyncData, Writable,
};
use seam_core::{Inspect, Perform, Provide, Token};

use crate::Std;

/// Implements the operations every open file has: `Seek` and `Stat`.
macro_rules! any_file_ops {
    ([$($g:tt)*] $ty:ty) => {
        impl<$($g)*> Perform<seam_core::ops::io::Seek> for $ty {
            fn perform(&mut self, op: seam_core::ops::io::Seek) -> seam_core::io::Result<u64> {
                crate::sio(io::Seek::seek(&mut self.0, op.0.into()))
            }
        }

        impl<$($g)*> Inspect<Stat> for $ty {
            fn inspect(&self, _: Stat) -> seam_core::io::Result<FileInfo> {
                crate::sio(self.0.metadata().map(file_info))
            }
        }
    };
}

/// Implements the operations a writable file has: `SetLen` and the syncs.
macro_rules! writable_file_ops {
    ([$($g:tt)*] $ty:ty) => {
        impl<$($g)*> Perform<SetLen> for $ty {
            fn perform(&mut self, op: SetLen) -> seam_core::io::Result<()> {
                crate::sio(self.0.set_len(op.0))
            }
        }

        impl<$($g)*> Perform<SyncAll> for $ty {
            fn perform(&mut self, _: SyncAll) -> seam_core::io::Result<()> {
                crate::sio(self.0.sync_all())
            }
        }

        impl<$($g)*> Perform<SyncData> for $ty {
            fn perform(&mut self, _: SyncData) -> seam_core::io::Result<()> {
                crate::sio(self.0.sync_data())
            }
        }
    };
}

/// An open file in mode `M`, acquired with [`Open`]. It performs `Read` if
/// `M` is readable, and `Write`, `Flush`, `SetLen`, `SyncAll` and
/// `SyncData` if `M` is writable; `Seek` and `Stat` in any mode.
///
/// When its scope succeeds, a file opened for writing is flushed and synced
/// to disk, and an error is returned from the scope. Otherwise it is closed
/// without syncing.
#[derive(Debug)]
pub struct File<M>(std::fs::File, PhantomData<fn() -> M>);

crate::read_ops!([M: Readable] File<M>);
crate::write_ops!([M: Writable] File<M>);
any_file_ops!([M] File<M>);
writable_file_ops!([M: Writable] File<M>);

impl<M: Mode> File<M> {
    fn commit(mut self) -> seam_core::io::Result<()> {
        // Only writable files are synced; some platforms refuse to sync a
        // read-only handle.
        if !M::WRITE {
            return Ok(());
        }
        io::Write::flush(&mut self.0)?;
        crate::sio(self.0.sync_all())
    }
}

/// A file that replaces its target when its scope succeeds, acquired with
/// [`CreateAtomic`].
///
/// Writes go to a temporary file in the same directory. When the scope
/// succeeds, it is synced and renamed over the target. Otherwise, including
/// on a panic, it is deleted.
#[derive(Debug)]
pub struct AtomicFile(std::fs::File, Pending);

crate::write_ops!([] AtomicFile);
any_file_ops!([] AtomicFile);
writable_file_ops!([] AtomicFile);

/// The temporary file of an [`AtomicFile`], deleted on drop unless it was
/// renamed into place. Declared after the `File` in `AtomicFile`, so the
/// file is closed before it is deleted.
#[derive(Debug)]
struct Pending {
    temp: Option<PathBuf>,
    target: PathBuf,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

impl AtomicFile {
    fn commit(mut self) -> seam_core::io::Result<()> {
        io::Write::flush(&mut self.0)?;
        self.0.sync_all()?;
        let temp = self
            .1
            .temp
            .as_ref()
            .expect("the temporary file exists until close");
        std::fs::rename(temp, &self.1.target)?;
        self.1.temp = None;
        sync_parent(&self.1.target);
        Ok(())
    }
}

/// Makes a rename durable by syncing the directory that holds it. Best
/// effort: not every platform can open a directory for syncing.
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
    }
}

/// A fresh temporary path beside `target`.
fn temp_path(target: &Path) -> PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let salt = RandomState::new().build_hasher().finish();
    target.with_file_name(format!(".{name}.seam-{salt:016x}.tmp"))
}

impl<M: Mode> Provide<Open<'_, M, File<M>>> for Std {
    fn provide(&mut self, op: Open<'_, M, File<M>>, _: Token) -> seam_core::io::Result<File<M>> {
        let o = op.options;
        crate::sio(
            std::fs::OpenOptions::new()
                .read(M::READ)
                .write(M::WRITE && !M::APPEND)
                .append(M::APPEND)
                .truncate(o.truncate)
                .create(o.create)
                .create_new(o.create_new)
                .open(op.path)
                .map(|f| File(f, PhantomData)),
        )
    }

    fn close(&mut self, file: File<M>) -> seam_core::io::Result<()> {
        file.commit()
    }
}

impl Provide<CreateAtomic<'_, AtomicFile>> for Std {
    fn close(&mut self, file: AtomicFile) -> seam_core::io::Result<()> {
        file.commit()
    }

    fn provide(
        &mut self,
        op: CreateAtomic<'_, AtomicFile>,
        _: Token,
    ) -> seam_core::io::Result<AtomicFile> {
        let temp = temp_path(op.path);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        Ok(AtomicFile(
            file,
            Pending {
                temp: Some(temp),
                // Kept for the rename on close, after the operation is gone.
                target: op.path.to_path_buf(),
            },
        ))
    }
}

impl Inspect<Metadata<'_>> for Std {
    fn inspect(&self, op: Metadata<'_>) -> seam_core::io::Result<FileInfo> {
        crate::sio(std::fs::metadata(op.path).map(file_info))
    }
}

/// A directory being listed, acquired with [`ReadDir`]. Perform
/// [`NextEntry`] on it for each path.
#[derive(Debug)]
pub struct Dir(std::fs::ReadDir);

impl Provide<ReadDir<'_, Dir>> for Std {
    fn provide(&mut self, op: ReadDir<'_, Dir>, _: Token) -> seam_core::io::Result<Dir> {
        crate::sio(std::fs::read_dir(op.path).map(Dir))
    }

    fn close(&mut self, _: Dir) -> seam_core::io::Result<()> {
        Ok(())
    }
}

impl Perform<NextEntry> for Dir {
    fn perform(&mut self, _: NextEntry) -> seam_core::io::Result<Option<PathBuf>> {
        crate::sio(self.0.next().transpose().map(|e| e.map(|e| e.path())))
    }
}

impl Perform<CreateDir<'_>> for Std {
    fn perform(&mut self, op: CreateDir<'_>) -> seam_core::io::Result<()> {
        crate::sio(std::fs::create_dir(op.path))
    }
}

impl Perform<RemoveFile<'_>> for Std {
    fn perform(&mut self, op: RemoveFile<'_>) -> seam_core::io::Result<()> {
        crate::sio(std::fs::remove_file(op.path))
    }
}

impl Perform<RemoveDir<'_>> for Std {
    fn perform(&mut self, op: RemoveDir<'_>) -> seam_core::io::Result<()> {
        if op.recursive {
            crate::sio(std::fs::remove_dir_all(op.path))
        } else {
            crate::sio(std::fs::remove_dir(op.path))
        }
    }
}

impl Perform<Rename<'_>> for Std {
    fn perform(&mut self, op: Rename<'_>) -> seam_core::io::Result<()> {
        crate::sio(std::fs::rename(op.from, op.to))
    }
}

impl Perform<CopyFile<'_>> for Std {
    fn perform(&mut self, op: CopyFile<'_>) -> seam_core::io::Result<u64> {
        crate::sio(std::fs::copy(op.from, op.to))
    }
}

fn file_info(m: std::fs::Metadata) -> FileInfo {
    let t = m.file_type();
    let kind = if t.is_file() {
        FileKind::File
    } else if t.is_dir() {
        FileKind::Dir
    } else if t.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    };
    FileInfo {
        kind,
        len: m.len(),
        readonly: m.permissions().readonly(),
        modified: m.modified().ok(),
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for AtomicFile {}
impl seam_core::Capability for Dir {}
impl<M> seam_core::Capability for File<M> {}
