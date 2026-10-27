//! Filesystem operations, and the [`File`], [`FileView`], [`AtomicFile`]
//! and [`Dir`] handles.

use core::marker::PhantomData;
use std::io;
use std::path::{Path, PathBuf};

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Mode, NextEntry, Open, ReadAt,
    ReadDir, Readable, RemoveDir, RemoveFile, Rename, SetLen, Stat, SyncAll, SyncData, View,
    Writable,
};
use seam_core::{Closing, Opening};
use smol::io::AsyncWriteExt;
use smol::stream::StreamExt;

use crate::Smol;

/// Implements the operations every open file has: `Seek` and `Stat`.
macro_rules! any_file_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform [$($g)*] $ty, seam_core::ops::io::Seek, |s, op| {
            smol::io::AsyncSeekExt::seek(&mut s.0, op.0.into()).await
        });
        async_op!(inspect [$($g)*] $ty, Stat, |s, _op| s.0.metadata().await.map(file_info));
    };
}

/// Implements the operations a writable file has: `SetLen` and the syncs.
macro_rules! writable_file_ops {
    ([$($g:tt)*] $ty:ty) => {
        async_op!(perform [$($g)*] $ty, SetLen, |s, op| s.0.set_len(op.0).await);
        async_op!(perform [$($g)*] $ty, SyncAll, |s, _op| s.0.sync_all().await);
        async_op!(perform [$($g)*] $ty, SyncData, |s, _op| s.0.sync_data().await);
    };
}

/// An open file in mode `M`, acquired with [`Open`]. It performs `Read` if
/// `M` is readable, and `Write`, `Flush`, `SetLen`, `SyncAll` and
/// `SyncData` if `M` is writable; `Seek` and `Stat` in any mode.
///
/// When its scope succeeds, a file opened for writing is flushed and synced,
/// and an error is returned from the scope. Otherwise it is dropped. smol
/// writes on its blocking pool, so writes still in flight when a failed or
/// cancelled scope drops the file may or may not land.
#[derive(Debug)]
pub struct File<M>(smol::fs::File, PhantomData<fn() -> M>);

read_ops!([M: Readable] File<M>);
write_ops!([M: Writable] File<M>);
any_file_ops!([M: Mode] File<M>);
writable_file_ops!([M: Writable] File<M>);

async_op!(perform [M: Mode] Smol, Opening<Open<'_, M, File<M>>>, |_s, Opening(op, _)| {
    let o = op.options;
    crate::sio(
        smol::fs::OpenOptions::new()
            .read(M::READ)
            .write(M::WRITE && !M::APPEND)
            .append(M::APPEND)
            .truncate(o.truncate)
            .create(o.create)
            .create_new(o.create_new)
            .open(op.path)
            .await
            .map(|f| File(f, PhantomData)),
    )
});
async_op!(perform [M: Mode] Smol, Closing<Open<'_, M, File<M>>>, |_s, Closing(mut file, _)| {
    // Only writable files are synced; some platforms refuse to sync a
    // read-only handle.
    if !M::WRITE {
        return Ok(());
    }
    file.0.flush().await?;
    crate::sio(file.0.sync_all().await)
});

/// A file that replaces its target when its scope succeeds, acquired with
/// [`CreateAtomic`].
///
/// Writes go to a temporary file in the same directory. When the scope
/// succeeds, it is synced and renamed over the target. Otherwise it is
/// deleted, whenever the scope stops: on an error, a panic, or a
/// cancellation at any point, including while the file is being created or
/// renamed.
#[derive(Debug)]
pub struct AtomicFile(smol::fs::File, Pending);

write_ops!([] AtomicFile);
any_file_ops!([] AtomicFile);
writable_file_ops!([] AtomicFile);

/// The temporary file of an [`AtomicFile`], deleted on drop unless it was
/// renamed into place.
///
/// The steps that create and rename the temporary file run on smol's
/// blocking pool, and they own this guard while they do. A blocking task
/// that has started runs to completion even if the future waiting for it is
/// dropped, and its result is then dropped; one that has not started never
/// runs. So a cancellation at any point either finishes the rename or
/// deletes the file.
#[derive(Debug)]
struct Pending {
    temp: Option<PathBuf>,
    target: PathBuf,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let Some(temp) = self.temp.take() else { return };
        // Drop cannot wait, so hand the delete to the blocking pool rather
        // than block an async thread.
        smol::unblock(move || std::fs::remove_file(temp)).detach();
    }
}

/// A fresh temporary path beside `target`, named so that `sweep_atomic`
/// recognises it.
fn temp_path(target: &Path) -> PathBuf {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let salt = RandomState::new().build_hasher().finish();
    target.with_file_name(format!(".{name}.seam-{salt:016x}.tmp"))
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

/// Runs `f` on the blocking pool.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> seam_core::io::Result<T> + Send + 'static,
) -> seam_core::io::Result<T> {
    smol::unblock(f).await
}

async_op!(perform [] Smol, Opening<CreateAtomic<'_, AtomicFile>>, |_s, Opening(op, _)| {
    // Created together with its guard, in one blocking task: see `Pending`.
    // The target is kept for the rename on close.
    let target = op.path.to_path_buf();
    let (file, pending) = blocking(move || {
        let temp = temp_path(&target);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        Ok((
            file,
            Pending {
                temp: Some(temp),
                target,
            },
        ))
    })
    .await?;
    seam_core::io::Result::Ok(AtomicFile(smol::fs::File::from(file), pending))
});
async_op!(perform [] Smol, Closing<CreateAtomic<'_, AtomicFile>>, |_s, Closing(file, _)| {
    let AtomicFile(mut file, mut pending) = file;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    // Renamed in one blocking task that owns the guard: see `Pending`.
    blocking(move || {
        let temp = pending
            .temp
            .as_ref()
            .expect("the temporary file exists until close");
        std::fs::rename(temp, &pending.target)?;
        pending.temp = None;
        sync_parent(&pending.target);
        Ok(())
    })
    .await
});

async_op!(inspect ['a] Smol, Metadata<'a>, |_s, op| smol::fs::metadata(op.path).await.map(file_info));

/// A regular file open to read at offsets, lent with [`View`]. It inspects
/// [`ReadAt`] and [`Stat`].
///
/// smol has no positional read, so each `ReadAt` runs on the blocking
/// pool, through a buffer of its own.
#[derive(Debug)]
pub struct FileView(std::sync::Arc<std::fs::File>);

/// Opens `path` to read, failing unless it is a regular file. Opening a
/// FIFO waits for a writer, so the path is checked before opening it, and
/// the file again after, in case the path was replaced in between.
fn open_view(path: &Path) -> io::Result<std::fs::File> {
    let check = |m: std::fs::Metadata| match () {
        _ if m.is_file() => Ok(()),
        _ if m.is_dir() => Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "is a directory",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        )),
    };
    check(std::fs::metadata(path)?)?;
    let file = std::fs::File::open(path)?;
    check(file.metadata()?)?;
    Ok(file)
}

async_op!(inspect ['a] Smol, Opening<View<'a, FileView>>, |_s, Opening(op, _)| {
    let path = op.path.to_path_buf();
    blocking(move || crate::sio(open_view(&path))).await.map(|f| FileView(f.into()))
});
async_op!(inspect ['a] Smol, Closing<View<'a, FileView>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(inspect ['a] FileView, ReadAt<'a>, |s, op| {
    let (file, len, offset) = (std::sync::Arc::clone(&s.0), op.buf.len(), op.offset);
    let read = blocking(move || {
        let mut buf = vec![0; len];
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(&*file, &mut buf, offset);
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(&*file, &mut buf, offset);
        #[cfg(not(any(unix, windows)))]
        let n = Err(io::Error::new(io::ErrorKind::Unsupported, "no positional reads here"));
        crate::sio(n.map(|n| (buf, n)))
    });
    let (buf, n) = read.await?;
    op.buf[..n].copy_from_slice(&buf[..n]);
    seam_core::io::Result::Ok(n)
});
async_op!(inspect [] FileView, Stat, |s, _op| {
    let file = std::sync::Arc::clone(&s.0);
    blocking(move || crate::sio(file.metadata().map(file_info))).await
});

/// A directory being listed, acquired with [`ReadDir`]. Perform
/// [`NextEntry`] on it for each path.
#[derive(Debug)]
pub struct Dir(smol::fs::ReadDir);

async_op!(inspect [] Smol, Opening<ReadDir<'_, Dir>>, |_s, Opening(op, _)| {
    crate::sio(smol::fs::read_dir(op.path).await.map(Dir))
});
async_op!(inspect [] Smol, Closing<ReadDir<'_, Dir>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] Dir, NextEntry, |s, _op| {
    s.0.next().await.transpose().map(|e| e.map(|e| e.path()))
});

async_op!(perform ['a] Smol, CreateDir<'a>, |_s, op| smol::fs::create_dir(op.path).await);
async_op!(perform ['a] Smol, RemoveFile<'a>, |_s, op| smol::fs::remove_file(op.path).await);
async_op!(perform ['a] Smol, RemoveDir<'a>, |_s, op| {
    if op.recursive {
        smol::fs::remove_dir_all(op.path).await
    } else {
        smol::fs::remove_dir(op.path).await
    }
});
async_op!(perform ['a] Smol, Rename<'a>, |_s, op| smol::fs::rename(op.from, op.to).await);
async_op!(perform ['a] Smol, CopyFile<'a>, |_s, op| smol::fs::copy(op.from, op.to).await);

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
impl seam_core::Capability for FileView {}
