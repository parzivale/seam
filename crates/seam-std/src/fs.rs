//! Filesystem operations, and the [`File`] and [`AtomicFile`] handles.

use std::io;
use std::path::{Path, PathBuf};

use std::time::SystemTime;

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Open, ReadDir, ReadFile,
    ReadToString, RemoveDir, RemoveFile, Rename, SweepAtomic, WriteFile,
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
    fn commit(mut self) -> io::Result<()> {
        if !self.1.0 {
            return Ok(());
        }
        io::Write::flush(&mut self.0)?;
        self.0.sync_all()
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
    fn commit(mut self) -> io::Result<()> {
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

impl Provide<Open<File>> for Std {
    fn provide(&mut self, op: Open<File>, _: Token) -> io::Result<File> {
        let o = op.options;
        std::fs::OpenOptions::new()
            .read(o.read)
            .write(o.write)
            .append(o.append)
            .truncate(o.truncate)
            .create(o.create)
            .create_new(o.create_new)
            .open(op.path)
            .map(|f| File(f, Writable(o.write || o.append)))
    }

    fn close(&mut self, file: File) -> io::Result<()> {
        file.commit()
    }
}

impl Provide<CreateAtomic<AtomicFile>> for Std {
    fn close(&mut self, file: AtomicFile) -> io::Result<()> {
        file.commit()
    }

    fn provide(&mut self, op: CreateAtomic<AtomicFile>, _: Token) -> io::Result<AtomicFile> {
        let temp = temp_path(&op.path);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        Ok(AtomicFile(
            file,
            Pending {
                temp: Some(temp),
                target: op.path,
            },
        ))
    }
}

impl Inspect<ReadFile> for Std {
    fn inspect(&self, op: ReadFile) -> io::Result<Vec<u8>> {
        std::fs::read(op.path)
    }
}

impl Inspect<ReadToString> for Std {
    fn inspect(&self, op: ReadToString) -> io::Result<String> {
        std::fs::read_to_string(op.path)
    }
}

impl Inspect<Metadata> for Std {
    fn inspect(&self, op: Metadata) -> io::Result<FileInfo> {
        std::fs::metadata(op.path).map(file_info)
    }
}

impl Inspect<ReadDir> for Std {
    fn inspect(&self, op: ReadDir) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(op.path)?
            .map(|entry| entry.map(|e| e.path()))
            .collect()
    }
}

impl Perform<WriteFile> for Std {
    fn perform(&mut self, op: WriteFile) -> io::Result<()> {
        std::fs::write(op.path, op.contents)
    }
}

impl Perform<CreateDir> for Std {
    fn perform(&mut self, op: CreateDir) -> io::Result<()> {
        if op.recursive {
            std::fs::create_dir_all(op.path)
        } else {
            std::fs::create_dir(op.path)
        }
    }
}

impl Perform<RemoveFile> for Std {
    fn perform(&mut self, op: RemoveFile) -> io::Result<()> {
        std::fs::remove_file(op.path)
    }
}

impl Perform<RemoveDir> for Std {
    fn perform(&mut self, op: RemoveDir) -> io::Result<()> {
        if op.recursive {
            std::fs::remove_dir_all(op.path)
        } else {
            std::fs::remove_dir(op.path)
        }
    }
}

impl Perform<Rename> for Std {
    fn perform(&mut self, op: Rename) -> io::Result<()> {
        std::fs::rename(op.from, op.to)
    }
}

impl Perform<CopyFile> for Std {
    fn perform(&mut self, op: CopyFile) -> io::Result<u64> {
        std::fs::copy(op.from, op.to)
    }
}

impl Perform<SweepAtomic> for Std {
    fn perform(&mut self, op: SweepAtomic) -> io::Result<u64> {
        let now = SystemTime::now();
        let mut removed = 0;
        for entry in std::fs::read_dir(&op.dir)? {
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
                    Err(e) => return Err(e),
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
impl seam_core::Capability for File {}
