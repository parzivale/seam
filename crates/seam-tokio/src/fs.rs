//! Filesystem operations, and the [`File`] handle.

use std::io;
use std::path::PathBuf;

use seam_core::AsyncFinalize;
use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Open, ReadDir, ReadFile,
    ReadToString, RemoveDir, RemoveFile, Rename, WriteFile,
};

use tokio::io::AsyncWriteExt;

use crate::Tokio;

/// An open file. Read and write it with the `io` operations.
///
/// [`AsyncFinalize::close_async`] flushes and syncs it, returning any error.
/// Tokio writes in the background, so a file that is dropped instead may
/// lose writes still in flight.
#[derive(Debug)]
pub struct File(tokio::fs::File, Writable);

io_ops!(File);

impl AsyncFinalize for File {
    type Error = io::Error;

    async fn close_async(mut self) -> io::Result<()> {
        if !self.1.0 {
            return Ok(());
        }
        self.0.flush().await?;
        self.0.sync_all().await
    }
}

/// Whether a [`File`] was opened for writing. Only those are synced on
/// close; some platforms refuse to sync a read-only handle.
#[derive(Debug, Clone, Copy)]
struct Writable(bool);

/// A file that replaces its target when closed. See
/// [`CreateAtomic`].
///
/// Writes go to a temporary file in the same directory.
/// [`AsyncFinalize::close_async`] syncs it and renames it over the target.
/// Abandoning or dropping it, including cancelling a scope partway through
/// closing, deletes the temporary file.
#[derive(Debug)]
pub struct AtomicFile(tokio::fs::File, Pending);

io_ops!(AtomicFile);

/// The temporary file of an [`AtomicFile`], deleted on drop unless it was
/// renamed into place.
#[derive(Debug)]
struct Pending {
    temp: Option<PathBuf>,
    target: PathBuf,
}

impl Drop for Pending {
    fn drop(&mut self) {
        // Drop cannot wait, so this deletes synchronously. If the rename
        // already happened, the temporary path is gone and this is a no-op.
        if let Some(temp) = self.temp.take() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

impl AsyncFinalize for AtomicFile {
    type Error = io::Error;

    async fn close_async(mut self) -> io::Result<()> {
        self.0.flush().await?;
        self.0.sync_all().await?;
        let temp = self
            .1
            .temp
            .clone()
            .expect("the temporary file exists until close");
        tokio::fs::rename(&temp, &self.1.target).await?;
        self.1.temp = None;
        if let Some(dir) = self.1.target.parent().filter(|d| !d.as_os_str().is_empty()) {
            if let Ok(dir) = tokio::fs::File::open(dir).await {
                let _ = dir.sync_all().await;
            }
        }
        Ok(())
    }
}

async_op!(perform [] Tokio, CreateAtomic<AtomicFile>, |_s, op| {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let name = op.path.file_name().unwrap_or_default().to_string_lossy();
    let salt = RandomState::new().build_hasher().finish();
    let temp = op.path.with_file_name(format!(".{name}.{salt:016x}.tmp"));
    let file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&temp).await?;
    Ok(AtomicFile(file, Pending { temp: Some(temp), target: op.path }))
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
async_op!(perform [] Tokio, Open<File>, |_s, op| {
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
});

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
