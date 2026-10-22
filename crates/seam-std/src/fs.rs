//! Filesystem operations, and the [`File`] handle.

use std::io;
use std::path::PathBuf;

use seam_core::ops::fs::{
    CopyFile, CreateDir, FileInfo, FileKind, Metadata, Open, ReadDir, ReadFile, ReadToString,
    RemoveDir, RemoveFile, Rename, WriteFile,
};
use seam_core::{Finalize, Inspect, Perform};

use crate::Std;

/// An open file. Read and write it with the `io` operations.
///
/// [`Finalize::close`] flushes and syncs it to disk; [`Finalize::abandon`]
/// drops it without syncing.
#[derive(Debug)]
pub struct File(std::fs::File);

crate::io_ops!(File);

impl Finalize for File {
    fn close(mut self) {
        // `close` has no error channel. A core that must observe sync
        // failures should perform `Flush` itself before the scope ends.
        let _ = io::Write::flush(&mut self.0);
        let _ = self.0.sync_all();
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

impl Perform<Open<File>> for Std {
    fn perform(&mut self, op: Open<File>) -> io::Result<File> {
        let o = op.options;
        std::fs::OpenOptions::new()
            .read(o.read)
            .write(o.write)
            .append(o.append)
            .truncate(o.truncate)
            .create(o.create)
            .create_new(o.create_new)
            .open(op.path)
            .map(File)
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
