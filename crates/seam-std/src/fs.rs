//! Filesystem operations, and the [`File`], [`AtomicFile`] and [`Dir`]
//! handles.

use std::io;
use std::path::{Path, PathBuf};

use std::time::SystemTime;

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, NextEntry, Open, ReadDir,
    ReadFile, ReadToString, RemoveDir, RemoveFile, Rename, SweepAtomic, WriteFile,
};
use seam_core::{Inspect, Perform, Provide, Token};

use crate::Std;

/// An open file, acquired with [`Open`]. Read and write it with the `io`
/// operations.
///
/// When its scope succeeds, a file opened for writing is flushed and synced
/// to disk, and an error is returned from the scope. Otherwise it is closed
/// without syncing.
#[derive(Debug)]
pub struct File(std::fs::File, Writable);

crate::io_ops!(File);

impl File {
    fn commit(mut self) -> seam_core::io::Result<()> {
        if !self.1.0 {
            return Ok(());
        }
        io::Write::flush(&mut self.0)?;
        crate::sio(self.0.sync_all())
    }
}

/// Whether a [`File`] was opened for writing. Only those are synced on
/// close; some platforms refuse to sync a read-only handle.
#[derive(Debug, Clone, Copy)]
struct Writable(bool);

/// A file that replaces its target when its scope succeeds, acquired with
/// [`CreateAtomic`].
///
/// Writes go to a temporary file in the same directory. When the scope
/// succeeds, it is synced and renamed over the target. Otherwise, including
/// on a panic, it is deleted.
#[derive(Debug)]
pub struct AtomicFile(std::fs::File, Pending);

crate::io_ops!(AtomicFile);

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

impl Provide<Open<'_, File>> for Std {
    fn provide(&mut self, op: Open<'_, File>, _: Token) -> seam_core::io::Result<File> {
        let o = op.options;
        crate::sio(
            std::fs::OpenOptions::new()
                .read(o.read)
                .write(o.write)
                .append(o.append)
                .truncate(o.truncate)
                .create(o.create)
                .create_new(o.create_new)
                .open(op.path)
                .map(|f| File(f, Writable(o.write || o.append))),
        )
    }

    fn close(&mut self, file: File) -> seam_core::io::Result<()> {
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

impl Inspect<ReadFile<'_>> for Std {
    fn inspect(&self, op: ReadFile<'_>) -> seam_core::io::Result<Vec<u8>> {
        crate::sio(std::fs::read(op.path))
    }
}

impl Inspect<ReadToString<'_>> for Std {
    fn inspect(&self, op: ReadToString<'_>) -> seam_core::io::Result<String> {
        crate::sio(std::fs::read_to_string(op.path))
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

impl Perform<WriteFile<'_>> for Std {
    fn perform(&mut self, op: WriteFile<'_>) -> seam_core::io::Result<()> {
        crate::sio(std::fs::write(op.path, op.contents))
    }
}

impl Perform<CreateDir<'_>> for Std {
    fn perform(&mut self, op: CreateDir<'_>) -> seam_core::io::Result<()> {
        if op.recursive {
            crate::sio(std::fs::create_dir_all(op.path))
        } else {
            crate::sio(std::fs::create_dir(op.path))
        }
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

impl Perform<SweepAtomic<'_>> for Std {
    fn perform(&mut self, op: SweepAtomic<'_>) -> seam_core::io::Result<u64> {
        let now = SystemTime::now();
        let mut removed = 0;
        for entry in std::fs::read_dir(op.dir)? {
            let entry = entry?;
            if !SweepAtomic::is_temp_name(&entry.file_name()) {
                continue;
            }
            let modified = entry.metadata()?.modified()?;
            let age = now.duration_since(modified).unwrap_or_default();
            if age >= op.older_than {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => removed += 1,
                    // Finished or cleaned up by its writer in the meantime.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(seam_core::io::Error::from(e)),
                }
            }
        }
        Ok(removed)
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
impl seam_core::Capability for File {}
