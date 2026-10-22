//! Filesystem operations, and the [`File`] and [`AtomicFile`] handles.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Open, ReadDir, ReadFile,
    ReadToString, RemoveDir, RemoveFile, Rename, SweepAtomic, WriteFile,
};
use seam_core::{AsyncProvide, Token};
use tokio::io::AsyncWriteExt;

use crate::Tokio;

/// An open file, acquired with [`Open`]. Read and write it with the `io`
/// operations.
///
/// When its scope succeeds, a file opened for writing is flushed and synced,
/// and an error is returned from the scope. Otherwise it is dropped. Tokio
/// writes in the background, so writes still in flight when a failed or
/// cancelled scope drops the file may or may not land.
#[derive(Debug)]
pub struct File(tokio::fs::File, Writable);

io_ops!(File);

/// Whether a [`File`] was opened for writing. Only those are synced on
/// close; some platforms refuse to sync a read-only handle.
#[derive(Debug, Clone, Copy)]
struct Writable(bool);

impl AsyncProvide<Open<File>> for Tokio {
    async fn provide_async(&mut self, op: Open<File>, _: Token) -> io::Result<File> {
        let o = op.options;
        tokio::fs::OpenOptions::new()
            .read(o.read)
            .write(o.write)
            .append(o.append)
            .truncate(o.truncate)
            .create(o.create)
            .create_new(o.create_new)
            .open(op.path)
            .await
            .map(|f| File(f, Writable(o.write || o.append)))
    }

    async fn close_async(&mut self, mut file: File) -> io::Result<()> {
        if !file.1.0 {
            return Ok(());
        }
        file.0.flush().await?;
        file.0.sync_all().await
    }
}

/// A file that replaces its target when its scope succeeds, acquired with
/// [`CreateAtomic`].
///
/// Writes go to a temporary file in the same directory. When the scope
/// succeeds, it is synced and renamed over the target. Otherwise it is
/// deleted, whenever the scope stops: on an error, a panic, or a
/// cancellation at any point, including while the file is being created or
/// renamed.
#[derive(Debug)]
pub struct AtomicFile(tokio::fs::File, Pending);

io_ops!(AtomicFile);

/// The temporary file of an [`AtomicFile`], deleted on drop unless it was
/// renamed into place.
///
/// The steps that create and rename the temporary file run on Tokio's
/// blocking pool, and they own this guard while they do. A blocking task
/// runs to completion even if the future waiting for it is dropped, and
/// Tokio drops its result if nobody is waiting, so a cancellation at any
/// point either finishes the rename or deletes the file.
#[derive(Debug)]
struct Pending {
    temp: Option<PathBuf>,
    target: PathBuf,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let Some(temp) = self.temp.take() else { return };
        // Drop cannot wait, so hand the delete to the blocking pool rather
        // than block an async worker. Outside a runtime, delete directly.
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(move || std::fs::remove_file(temp))),
            Err(_) => drop(std::fs::remove_file(temp)),
        }
    }
}

/// A fresh temporary path beside `target`, named so that [`SweepAtomic`]
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
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(io::Error::other)?
}

impl AsyncProvide<CreateAtomic<AtomicFile>> for Tokio {
    async fn provide_async(
        &mut self,
        op: CreateAtomic<AtomicFile>,
        _: Token,
    ) -> io::Result<AtomicFile> {
        // Created together with its guard, in one blocking task: see `Pending`.
        let (file, pending) = blocking(move || {
            let temp = temp_path(&op.path);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            Ok((
                file,
                Pending {
                    temp: Some(temp),
                    target: op.path,
                },
            ))
        })
        .await?;
        Ok(AtomicFile(tokio::fs::File::from_std(file), pending))
    }

    async fn close_async(&mut self, file: AtomicFile) -> io::Result<()> {
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
    }
}

async_op!(perform [] Tokio, SweepAtomic, |_s, op| {
    blocking(move || {
        let now = SystemTime::now();
        let mut removed = 0;
        for entry in std::fs::read_dir(&op.dir)? {
            let entry = entry?;
            if !SweepAtomic::is_temp_name(&entry.file_name()) {
                continue;
            }
            let age = now.duration_since(entry.metadata()?.modified()?).unwrap_or_default();
            if age >= op.older_than {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => removed += 1,
                    // Finished or cleaned up by its writer in the meantime.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(removed)
    })
    .await
});

async_op!(inspect [] Tokio, ReadFile, |_s, op| tokio::fs::read(op.path).await);
async_op!(inspect [] Tokio, ReadToString, |_s, op| tokio::fs::read_to_string(op.path).await);
async_op!(inspect [] Tokio, Metadata, |_s, op| tokio::fs::metadata(op.path).await.map(file_info));
async_op!(inspect [] Tokio, ReadDir, |_s, op| {
    let mut dir = tokio::fs::read_dir(op.path).await?;
    let mut paths = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        paths.push(entry.path());
    }
    Ok(paths)
});

async_op!(perform [] Tokio, WriteFile, |_s, op| tokio::fs::write(op.path, op.contents).await);
async_op!(perform [] Tokio, CreateDir, |_s, op| {
    if op.recursive {
        tokio::fs::create_dir_all(op.path).await
    } else {
        tokio::fs::create_dir(op.path).await
    }
});
async_op!(perform [] Tokio, RemoveFile, |_s, op| tokio::fs::remove_file(op.path).await);
async_op!(perform [] Tokio, RemoveDir, |_s, op| {
    if op.recursive {
        tokio::fs::remove_dir_all(op.path).await
    } else {
        tokio::fs::remove_dir(op.path).await
    }
});
async_op!(perform [] Tokio, Rename, |_s, op| tokio::fs::rename(op.from, op.to).await);
async_op!(perform [] Tokio, CopyFile, |_s, op| tokio::fs::copy(op.from, op.to).await);

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
impl seam_core::Capability for File {}
