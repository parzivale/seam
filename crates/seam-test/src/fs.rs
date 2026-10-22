//! The in-memory filesystem, and the [`File`] and [`AtomicFile`] handles.
//!
//! Relative paths are resolved against the simulated current directory.
//! `..` steps out of a directory only if it exists, as on a real system.
//! There are no symlinks or permissions. Files are inodes, so an open handle keeps working after its
//! path is renamed or removed, as on Unix.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::string::String;
use std::time::SystemTime;
use std::vec::Vec;

use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Open, ReadDir, ReadFile,
    ReadToString, RemoveDir, RemoveFile, Rename, SweepAtomic, WriteFile,
};
use seam_core::ops::io::{Flush, Read, ReadToEnd, Write, WriteAll};
use seam_core::{Inspect, Perform, Provide, Token};

use crate::world::{Shared, World};
use crate::{Sim, error};

/// A resolved path: its normal components from the root.
type Key = Vec<OsString>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    Dir { modified: SystemTime },
    File(usize),
}

#[derive(Debug)]
struct Inode {
    bytes: Vec<u8>,
    modified: SystemTime,
}

#[derive(Debug)]
pub(crate) struct Fs {
    nodes: BTreeMap<Key, Node>,
    inodes: Vec<Inode>,
    /// Numbers temporary files, so their names are deterministic.
    next_temp: u64,
}

impl Default for Fs {
    fn default() -> Self {
        let root = Node::Dir {
            modified: std::time::UNIX_EPOCH,
        };
        Self {
            nodes: BTreeMap::from([(Key::new(), root)]),
            inodes: Vec::new(),
            next_temp: 0,
        }
    }
}

fn parent(key: &Key) -> &[OsString] {
    &key[..key.len().saturating_sub(1)]
}

impl Fs {
    /// Resolves `path` against `cwd` to its components from the root.
    fn resolve(&self, cwd: &Path, path: &Path) -> io::Result<Key> {
        let mut key = Key::new();
        let base = if path.has_root() { None } else { Some(cwd) };
        for c in base
            .into_iter()
            .flat_map(Path::components)
            .chain(path.components())
        {
            match c {
                Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    self.require_dir(&key)?;
                    key.pop();
                }
                Component::Normal(name) => key.push(name.to_os_string()),
            }
        }
        Ok(key)
    }

    /// The error for a path that does not exist: `NotADirectory` if some
    /// ancestor is a file, as on a real system.
    fn missing(&self, key: &[OsString]) -> io::Error {
        let file_ancestor =
            (0..key.len()).any(|i| matches!(self.nodes.get(&key[..i]), Some(Node::File(_))));
        if file_ancestor {
            error(ErrorKind::NotADirectory, "a path component is a file")
        } else {
            error(ErrorKind::NotFound, "no such file or directory")
        }
    }

    fn node(&self, key: &[OsString]) -> io::Result<Node> {
        self.nodes
            .get(key)
            .copied()
            .ok_or_else(|| self.missing(key))
    }

    fn file(&self, key: &[OsString]) -> io::Result<usize> {
        match self.node(key)? {
            Node::File(inode) => Ok(inode),
            Node::Dir { .. } => Err(error(ErrorKind::IsADirectory, "is a directory")),
        }
    }

    fn require_dir(&self, key: &[OsString]) -> io::Result<()> {
        match self.node(key)? {
            Node::Dir { .. } => Ok(()),
            Node::File(_) => Err(error(ErrorKind::NotADirectory, "not a directory")),
        }
    }

    fn children(&self, key: &[OsString]) -> impl Iterator<Item = &Key> {
        self.nodes
            .range::<[OsString], _>((core::ops::Bound::Excluded(key), core::ops::Bound::Unbounded))
            .map(|(k, _)| k)
            .take_while(move |k| k.starts_with(key))
    }

    fn touch_parent(&mut self, key: &Key, now: SystemTime) {
        if let Some(Node::Dir { modified }) = self.nodes.get_mut(parent(key)) {
            *modified = now;
        }
    }

    /// Writes `bytes` as the whole of the file at `key`, creating it if the
    /// parent is a directory.
    fn write(&mut self, key: &Key, bytes: Vec<u8>, now: SystemTime) -> io::Result<usize> {
        match self.nodes.get(key) {
            Some(Node::File(inode)) => {
                let inode = &mut self.inodes[*inode];
                inode.bytes = bytes;
                inode.modified = now;
                return Ok(0);
            }
            Some(Node::Dir { .. }) => return Err(error(ErrorKind::IsADirectory, "is a directory")),
            None => {}
        }
        if key.is_empty() {
            return Err(error(ErrorKind::IsADirectory, "is a directory"));
        }
        self.require_dir(parent(key))?;
        self.inodes.push(Inode {
            bytes,
            modified: now,
        });
        let inode = self.inodes.len() - 1;
        self.nodes.insert(key.clone(), Node::File(inode));
        self.touch_parent(key, now);
        Ok(inode)
    }

    fn create_dir(&mut self, key: &Key, recursive: bool, now: SystemTime) -> io::Result<()> {
        if recursive {
            for i in 1..=key.len() {
                match self.nodes.get(&key[..i]) {
                    Some(Node::Dir { .. }) => {}
                    Some(Node::File(_)) => {
                        return Err(error(
                            ErrorKind::NotADirectory,
                            "a path component is a file",
                        ));
                    }
                    None => {
                        self.nodes
                            .insert(key[..i].to_vec(), Node::Dir { modified: now });
                        self.touch_parent(&key[..i].to_vec(), now);
                    }
                }
            }
            return Ok(());
        }
        if self.nodes.contains_key(key) {
            return Err(error(ErrorKind::AlreadyExists, "already exists"));
        }
        self.require_dir(parent(key))?;
        self.nodes.insert(key.clone(), Node::Dir { modified: now });
        self.touch_parent(key, now);
        Ok(())
    }

    fn info(&self, key: &[OsString]) -> io::Result<FileInfo> {
        Ok(match self.node(key)? {
            Node::Dir { modified } => FileInfo {
                kind: FileKind::Dir,
                len: 0,
                readonly: false,
                modified: Some(modified),
            },
            Node::File(inode) => {
                let inode = &self.inodes[inode];
                FileInfo {
                    kind: FileKind::File,
                    len: inode.bytes.len() as u64,
                    readonly: false,
                    modified: Some(inode.modified),
                }
            }
        })
    }
}

impl Sim {
    fn fs_op<R>(
        &self,
        path: &Path,
        f: impl FnOnce(&mut World, Key) -> io::Result<R>,
    ) -> io::Result<R> {
        let mut world = self.world.lock();
        let key = world.fs.resolve(&self.cwd, path)?;
        f(&mut world, key)
    }

    /// Creates a file with `contents`, and any missing parent directories.
    /// For setting up a test; replaces an existing file.
    ///
    /// # Panics
    ///
    /// If a parent is a file, or the path is a directory.
    pub fn add_file(&mut self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) -> &mut Self {
        let path = path.as_ref();
        self.fs_op(path, |w, key| {
            let now = w.wall;
            w.fs.create_dir(&parent(&key).to_vec(), true, now)?;
            w.fs.write(&key, contents.into(), now)
        })
        .unwrap_or_else(|e| panic!("seam-test: cannot add file {}: {e}", path.display()));
        self
    }

    /// Creates a directory, and any missing parents. For setting up a test.
    ///
    /// # Panics
    ///
    /// If a component is a file.
    pub fn add_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        let path = path.as_ref();
        self.fs_op(path, |w, key| {
            let now = w.wall;
            w.fs.create_dir(&key, true, now)
        })
        .unwrap_or_else(|e| panic!("seam-test: cannot add dir {}: {e}", path.display()));
        self
    }

    /// The contents of the file at `path`, if it is one. For assertions.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        self.fs_op(path.as_ref(), |w, key| {
            Ok(w.fs.inodes[w.fs.file(&key)?].bytes.clone())
        })
        .ok()
    }

    /// Whether anything exists at `path`. For assertions.
    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.fs_op(path.as_ref(), |w, key| Ok(w.fs.nodes.contains_key(&key)))
            .unwrap_or(false)
    }
}

impl Inspect<ReadFile> for Sim {
    fn inspect(&self, op: ReadFile) -> io::Result<Vec<u8>> {
        self.fs_op(&op.path, |w, key| {
            Ok(w.fs.inodes[w.fs.file(&key)?].bytes.clone())
        })
    }
}

impl Inspect<ReadToString> for Sim {
    fn inspect(&self, op: ReadToString) -> io::Result<String> {
        let bytes = self.inspect(ReadFile { path: op.path })?;
        String::from_utf8(bytes)
            .map_err(|_| error(ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
    }
}

impl Inspect<Metadata> for Sim {
    fn inspect(&self, op: Metadata) -> io::Result<FileInfo> {
        self.fs_op(&op.path, |w, key| w.fs.info(&key))
    }
}

impl Inspect<ReadDir> for Sim {
    fn inspect(&self, op: ReadDir) -> io::Result<Vec<PathBuf>> {
        self.fs_op(&op.path, |w, key| {
            w.fs.require_dir(&key)?;
            Ok(w.fs
                .children(&key)
                .filter(|k| k.len() == key.len() + 1)
                .map(|k| op.path.join(&k[key.len()]))
                .collect())
        })
    }
}

impl Perform<WriteFile> for Sim {
    fn perform(&mut self, op: WriteFile) -> io::Result<()> {
        self.fs_op(&op.path, |w, key| {
            let now = w.wall;
            w.fs.write(&key, op.contents, now).map(drop)
        })
    }
}

impl Perform<CreateDir> for Sim {
    fn perform(&mut self, op: CreateDir) -> io::Result<()> {
        self.fs_op(&op.path, |w, key| {
            let now = w.wall;
            w.fs.create_dir(&key, op.recursive, now)
        })
    }
}

impl Perform<RemoveFile> for Sim {
    fn perform(&mut self, op: RemoveFile) -> io::Result<()> {
        self.fs_op(&op.path, |w, key| {
            w.fs.file(&key)?;
            w.fs.nodes.remove(&key);
            let now = w.wall;
            w.fs.touch_parent(&key, now);
            Ok(())
        })
    }
}

impl Perform<RemoveDir> for Sim {
    fn perform(&mut self, op: RemoveDir) -> io::Result<()> {
        self.fs_op(&op.path, |w, key| {
            w.fs.require_dir(&key)?;
            if key.is_empty() {
                return Err(error(ErrorKind::PermissionDenied, "cannot remove the root"));
            }
            let doomed: Vec<Key> = w.fs.children(&key).cloned().collect();
            if !op.recursive && !doomed.is_empty() {
                return Err(error(ErrorKind::DirectoryNotEmpty, "directory not empty"));
            }
            for k in doomed {
                w.fs.nodes.remove(&k);
            }
            w.fs.nodes.remove(&key);
            let now = w.wall;
            w.fs.touch_parent(&key, now);
            Ok(())
        })
    }
}

impl Perform<Rename> for Sim {
    fn perform(&mut self, op: Rename) -> io::Result<()> {
        self.fs_op(&op.from, |w, from| {
            let to = w.fs.resolve(&self.cwd, &op.to)?;
            let fs = &mut w.fs;
            let node = fs.node(&from)?;
            if from == to {
                return Ok(());
            }
            if to.starts_with(&from) {
                return Err(error(
                    ErrorKind::InvalidInput,
                    "cannot move a directory into itself",
                ));
            }
            fs.require_dir(parent(&to))?;
            match (node, fs.nodes.get(&to)) {
                (_, None) => {}
                (Node::File(_), Some(Node::File(_))) => {}
                (Node::Dir { .. }, Some(Node::Dir { .. })) => {
                    if fs.children(&to).next().is_some() {
                        return Err(error(ErrorKind::DirectoryNotEmpty, "directory not empty"));
                    }
                }
                (Node::File(_), Some(Node::Dir { .. })) => {
                    return Err(error(ErrorKind::IsADirectory, "is a directory"));
                }
                (Node::Dir { .. }, Some(Node::File(_))) => {
                    return Err(error(ErrorKind::NotADirectory, "not a directory"));
                }
            }
            let moved: Vec<(Key, Node)> = core::iter::once(from.clone())
                .chain(fs.children(&from).cloned())
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|k| fs.nodes.remove(&k).map(|n| (k, n)))
                .collect();
            for (k, n) in moved {
                let mut new = to.clone();
                new.extend_from_slice(&k[from.len()..]);
                fs.nodes.insert(new, n);
            }
            let now = w.wall;
            w.fs.touch_parent(&from, now);
            w.fs.touch_parent(&to, now);
            Ok(())
        })
    }
}

impl Perform<SweepAtomic> for Sim {
    fn perform(&mut self, op: SweepAtomic) -> io::Result<u64> {
        self.fs_op(&op.dir, |w, dir| {
            w.fs.require_dir(&dir)?;
            let now = w.wall;
            let doomed: Vec<Key> = w
                .fs
                .children(&dir)
                .filter(|k| k.len() == dir.len() + 1 && SweepAtomic::is_temp_name(&k[dir.len()]))
                .filter(|k| match w.fs.nodes[*k] {
                    Node::File(inode) => {
                        let modified = w.fs.inodes[inode].modified;
                        now.duration_since(modified).unwrap_or_default() >= op.older_than
                    }
                    Node::Dir { .. } => false,
                })
                .cloned()
                .collect();
            for k in &doomed {
                w.fs.nodes.remove(k);
            }
            Ok(doomed.len() as u64)
        })
    }
}

impl Perform<CopyFile> for Sim {
    fn perform(&mut self, op: CopyFile) -> io::Result<u64> {
        self.fs_op(&op.from, |w, from| {
            let to = w.fs.resolve(&self.cwd, &op.to)?;
            let bytes = w.fs.inodes[w.fs.file(&from)?].bytes.clone();
            let len = bytes.len() as u64;
            let now = w.wall;
            w.fs.write(&to, bytes, now)?;
            Ok(len)
        })
    }
}

/// Writes land immediately, so closing has nothing left to do.
impl Provide<Open<File>> for Sim {
    fn close(&mut self, _: File) -> io::Result<()> {
        Ok(())
    }

    fn provide(&mut self, op: Open<File>, _: Token) -> io::Result<File> {
        let o = op.options;
        let writable = o.write || o.append;
        if !(o.read || writable) || ((o.truncate || o.create || o.create_new) && !writable) {
            return Err(error(ErrorKind::InvalidInput, "invalid open options"));
        }
        if o.truncate && o.append {
            return Err(error(
                ErrorKind::InvalidInput,
                "cannot truncate in append mode",
            ));
        }
        let inode = self.fs_op(&op.path, |w, key| {
            let now = w.wall;
            match w.fs.nodes.get(&key) {
                Some(_) if o.create_new => Err(error(ErrorKind::AlreadyExists, "already exists")),
                Some(_) => {
                    let inode = w.fs.file(&key)?;
                    if o.truncate {
                        w.fs.inodes[inode].bytes.clear();
                        w.fs.inodes[inode].modified = now;
                    }
                    Ok(inode)
                }
                None if o.create || o.create_new => w.fs.write(&key, Vec::new(), now),
                None => Err(w.fs.missing(&key)),
            }
        })?;
        Ok(File {
            world: self.world.clone(),
            inode,
            pos: 0,
            read: o.read,
            write: writable,
            append: o.append,
        })
    }
}

/// An open in-memory file. Read and write it with the `io` operations.
#[derive(Debug)]
pub struct File {
    world: Shared,
    inode: usize,
    pos: usize,
    read: bool,
    write: bool,
    append: bool,
}

impl File {
    fn readable(&self) -> io::Result<()> {
        if self.read {
            Ok(())
        } else {
            Err(error(ErrorKind::PermissionDenied, "not open for reading"))
        }
    }

    fn write_bytes(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.write {
            return Err(error(ErrorKind::PermissionDenied, "not open for writing"));
        }
        let mut world = self.world.lock();
        let now = world.wall;
        let inode = &mut world.fs.inodes[self.inode];
        if self.append {
            self.pos = inode.bytes.len();
        }
        let end = self.pos + buf.len();
        if inode.bytes.len() < end {
            inode.bytes.resize(end, 0);
        }
        inode.bytes[self.pos..end].copy_from_slice(buf);
        inode.modified = now;
        self.pos = end;
        Ok(buf.len())
    }
}

impl Perform<Read<'_>> for File {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        self.readable()?;
        let world = self.world.lock();
        let rest = world.fs.inodes[self.inode]
            .bytes
            .get(self.pos..)
            .unwrap_or_default();
        let n = rest.len().min(op.0.len());
        op.0[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl Perform<ReadToEnd<'_>> for File {
    fn perform(&mut self, op: ReadToEnd<'_>) -> io::Result<usize> {
        self.readable()?;
        let world = self.world.lock();
        let rest = world.fs.inodes[self.inode]
            .bytes
            .get(self.pos..)
            .unwrap_or_default();
        op.0.extend_from_slice(rest);
        self.pos += rest.len();
        Ok(rest.len())
    }
}

impl Perform<Write<'_>> for File {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.write_bytes(op.0)
    }
}

impl Perform<WriteAll<'_>> for File {
    fn perform(&mut self, op: WriteAll<'_>) -> io::Result<()> {
        self.write_bytes(op.0).map(drop)
    }
}

/// Writes land immediately, so there is nothing to flush.
impl Perform<Flush> for File {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

/// A file that replaces its target when its scope succeeds, acquired with
/// [`CreateAtomic`].
///
/// While it is open, its contents live in a temporary file beside the
/// target, visible in the simulated filesystem as `.<name>.seam-<n>.tmp`.
/// When the scope succeeds it is renamed over the target; otherwise the
/// temporary file is removed.
#[derive(Debug)]
pub struct AtomicFile {
    file: File,
    temp: Key,
    target: Key,
    committed: bool,
}

impl Provide<CreateAtomic<AtomicFile>> for Sim {
    fn close(&mut self, file: AtomicFile) -> io::Result<()> {
        file.commit()
    }

    fn provide(&mut self, op: CreateAtomic<AtomicFile>, _: Token) -> io::Result<AtomicFile> {
        let (temp, target, inode) = self.fs_op(&op.path, |w, target| {
            let Some(name) = target.last() else {
                return Err(error(ErrorKind::IsADirectory, "is a directory"));
            };
            if let Some(Node::Dir { .. }) = w.fs.nodes.get(&target) {
                return Err(error(ErrorKind::IsADirectory, "is a directory"));
            }
            let mut temp = parent(&target).to_vec();
            let n = w.fs.next_temp;
            w.fs.next_temp += 1;
            let mut temp_name = OsString::from(".");
            temp_name.push(name);
            temp_name.push(std::format!(".seam-{n}.tmp"));
            temp.push(temp_name);
            let now = w.wall;
            let inode = w.fs.write(&temp, Vec::new(), now)?;
            Ok((temp, target, inode))
        })?;
        let file = File {
            world: self.world.clone(),
            inode,
            pos: 0,
            read: false,
            write: true,
            append: false,
        };
        Ok(AtomicFile {
            file,
            temp,
            target,
            committed: false,
        })
    }
}

impl AtomicFile {
    fn commit(mut self) -> io::Result<()> {
        let mut world = self.file.world.lock();
        let now = world.wall;
        let fs = &mut world.fs;
        if fs.nodes.get(&self.temp) != Some(&Node::File(self.file.inode)) {
            return Err(error(ErrorKind::NotFound, "the temporary file was removed"));
        }
        fs.require_dir(parent(&self.target))?;
        if let Some(Node::Dir { .. }) = fs.nodes.get(&self.target) {
            return Err(error(ErrorKind::IsADirectory, "is a directory"));
        }
        let node = fs.nodes.remove(&self.temp).expect("checked above");
        fs.nodes.insert(self.target.clone(), node);
        fs.touch_parent(&self.target, now);
        drop(world);
        self.committed = true;
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut world = self.file.world.lock();
        if world.fs.nodes.get(&self.temp) == Some(&Node::File(self.file.inode)) {
            world.fs.nodes.remove(&self.temp);
        }
    }
}

macro_rules! delegate_io {
    ($($op:ty => $out:ty),*) => {$(
        impl Perform<$op> for AtomicFile {
            fn perform(&mut self, op: $op) -> $out {
                self.file.perform(op)
            }
        }
    )*};
}

delegate_io! {
    Write<'_> => io::Result<usize>,
    WriteAll<'_> => io::Result<()>,
    Flush => io::Result<()>
}
