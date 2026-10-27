//! The in-memory filesystem, and the [`File`], [`AtomicFile`] and [`Dir`]
//! handles.
//!
//! Relative paths are resolved against the simulated current directory.
//! `..` steps out of a directory only if it exists, as on a real system.
//! There are no symlinks or permissions. Files are inodes, so an open
//! handle keeps working after its path is renamed or removed, as on Unix.

use seam_core::io::{self, ErrorKind};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;
use std::vec::Vec;

use core::marker::PhantomData;
use seam_core::ops::fs::{
    CopyFile, CreateAtomic, CreateDir, FileInfo, FileKind, Metadata, Mode, NextEntry, Open, ReadAt,
    ReadDir, Readable, RemoveDir, RemoveFile, Rename, SetLen, Stat, SyncAll, SyncData, View,
    Writable,
};
use seam_core::ops::io::{Flush, Read, Seek, SeekFrom, Write};
use seam_core::{Closing, Inspect, Opening, Perform};

use seam_core::effects as fx;

use crate::error;
use crate::sim::{Inner, Shared, SimState, Simulate, Simulated, States, World};
use crate::world_file::{self, Failure, Node as WorldNode, show};

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

/// The simulated filesystem: an in-memory tree of directories and files.
/// See the [module docs](self).
#[derive(Debug)]
pub struct Files {
    nodes: BTreeMap<Key, Node>,
    inodes: Vec<Inode>,
    /// Numbers temporary files, so their names are deterministic.
    next_temp: u64,
}

/// A fresh filesystem holds an empty `/` and `/tmp`.
impl Default for Files {
    fn default() -> Self {
        let dir = Node::Dir {
            modified: std::time::UNIX_EPOCH,
        };
        Self {
            nodes: BTreeMap::from([(Key::new(), dir), (Key::from([OsString::from("tmp")]), dir)]),
            inodes: Vec::new(),
            next_temp: 0,
        }
    }
}

fn parent(key: &Key) -> &[OsString] {
    &key[..key.len().saturating_sub(1)]
}

impl Files {
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

/// Resolves `path` against the current directory, and runs `f` on the
/// states with it.
fn fs_op<R>(
    states: &mut Inner,
    path: &Path,
    f: impl FnOnce(&mut Inner, Key) -> io::Result<R>,
) -> io::Result<R> {
    let cwd = states.cwd();
    let key = states.fs().resolve(&cwd, path)?;
    f(states, key)
}

impl Inner {
    /// The filesystem.
    pub(crate) fn fs(&mut self) -> &mut Files {
        self.state::<Files>()
    }
}

#[cfg(not(feature = "env"))]
impl Inner {
    /// Without `env`, the current directory is always `/`.
    pub(crate) fn cwd(&mut self) -> PathBuf {
        PathBuf::from("/")
    }
}

impl<G> World<G> {
    /// Creates a file with `contents`, and any missing parent directories.
    /// For setting up a test; replaces an existing file.
    ///
    /// # Panics
    ///
    /// If a parent is a file, or the path is a directory.
    pub fn add_file(&mut self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) -> &mut Self {
        let path = path.as_ref();
        fs_op(&mut self.lock(), path, |w, key| {
            let now = w.wall();
            w.fs().create_dir(&parent(&key).to_vec(), true, now)?;
            w.fs().write(&key, contents.into(), now)
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
        fs_op(&mut self.lock(), path, |w, key| {
            let now = w.wall();
            w.fs().create_dir(&key, true, now)
        })
        .unwrap_or_else(|e| panic!("seam-test: cannot add dir {}: {e}", path.display()));
        self
    }

    /// The contents of the file at `path`, if it is one. For assertions.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        fs_op(&mut self.lock(), path.as_ref(), |w, key| {
            let fs = w.fs();
            Ok(fs.inodes[fs.file(&key)?].bytes.clone())
        })
        .ok()
    }

    /// Whether anything exists at `path`. For assertions.
    pub fn exists(&self, path: impl AsRef<Path>) -> bool {
        fs_op(&mut self.lock(), path.as_ref(), |w, key| {
            Ok(w.fs().nodes.contains_key(&key))
        })
        .unwrap_or(false)
    }
}

impl Simulate<Metadata<'_>> for fx::Metadata {
    fn run(states: &mut States<'_>, op: Metadata<'_>) -> io::Result<FileInfo> {
        fs_op(states.inner(), op.path, |w, key| w.fs().info(&key))
    }
}

/// A directory being listed, acquired with [`ReadDir`]: the paths in it
/// when it was opened, in sorted order. Perform [`NextEntry`] on it for
/// each.
#[derive(Debug)]
pub struct Dir(std::vec::IntoIter<PathBuf>);

impl Simulate<Opening<ReadDir<'_, Dir>>> for fx::ReadDir {
    fn run(states: &mut States<'_>, Opening(op, _): Opening<ReadDir<'_, Dir>>) -> io::Result<Dir> {
        let paths: Vec<PathBuf> = fs_op(states.inner(), op.path, |w, key| {
            w.fs().require_dir(&key)?;
            Ok(w.fs()
                .children(&key)
                .filter(|k| k.len() == key.len() + 1)
                .map(|k| op.path.join(&k[key.len()]))
                .collect())
        })?;
        Ok(Dir(paths.into_iter()))
    }
}

impl Simulate<Closing<ReadDir<'_, Dir>>> for fx::ReadDir {
    fn run(_: &mut States<'_>, Closing(_, _): Closing<ReadDir<'_, Dir>>) -> io::Result<()> {
        Ok(())
    }
}

impl Perform<NextEntry> for Dir {
    fn perform(&mut self, _: NextEntry) -> io::Result<Option<PathBuf>> {
        Ok(self.0.next())
    }
}

impl Simulate<CreateDir<'_>> for fx::CreateDir {
    fn run(states: &mut States<'_>, op: CreateDir<'_>) -> io::Result<()> {
        fs_op(states.inner(), op.path, |w, key| {
            let now = w.wall();
            w.fs().create_dir(&key, false, now)
        })
    }
}

impl Simulate<RemoveFile<'_>> for fx::RemoveFile {
    fn run(states: &mut States<'_>, op: RemoveFile<'_>) -> io::Result<()> {
        fs_op(states.inner(), op.path, |w, key| {
            w.fs().file(&key)?;
            w.fs().nodes.remove(&key);
            let now = w.wall();
            w.fs().touch_parent(&key, now);
            Ok(())
        })
    }
}

impl Simulate<RemoveDir<'_>> for fx::RemoveDir {
    fn run(states: &mut States<'_>, op: RemoveDir<'_>) -> io::Result<()> {
        fs_op(states.inner(), op.path, |w, key| {
            w.fs().require_dir(&key)?;
            if key.is_empty() {
                return Err(error(ErrorKind::PermissionDenied, "cannot remove the root"));
            }
            let doomed: Vec<Key> = w.fs().children(&key).cloned().collect();
            if !op.recursive && !doomed.is_empty() {
                return Err(error(ErrorKind::DirectoryNotEmpty, "directory not empty"));
            }
            for k in doomed {
                w.fs().nodes.remove(&k);
            }
            w.fs().nodes.remove(&key);
            let now = w.wall();
            w.fs().touch_parent(&key, now);
            Ok(())
        })
    }
}

impl Simulate<Rename<'_>> for fx::Rename {
    fn run(states: &mut States<'_>, op: Rename<'_>) -> io::Result<()> {
        fs_op(states.inner(), op.from, |w, from| {
            let to = {
                let cwd = w.cwd();
                w.fs().resolve(&cwd, op.to)?
            };
            let fs = w.fs();
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
            let now = w.wall();
            w.fs().touch_parent(&from, now);
            w.fs().touch_parent(&to, now);
            Ok(())
        })
    }
}

impl Simulate<CopyFile<'_>> for fx::CopyFile {
    fn run(states: &mut States<'_>, op: CopyFile<'_>) -> io::Result<u64> {
        fs_op(states.inner(), op.from, |w, from| {
            let to = {
                let cwd = w.cwd();
                w.fs().resolve(&cwd, op.to)?
            };
            let fs = w.fs();
            let bytes = fs.inodes[fs.file(&from)?].bytes.clone();
            let len = bytes.len() as u64;
            let now = w.wall();
            w.fs().write(&to, bytes, now)?;
            Ok(len)
        })
    }
}

impl<M: Mode> Simulate<Opening<Open<'_, M, File<M>>>> for fx::Open<M> {
    fn run(
        states: &mut States,
        Opening(op, _): Opening<Open<'_, M, File<M>>>,
    ) -> io::Result<File<M>> {
        let o = op.options;
        if (o.truncate || o.create || o.create_new) && !M::WRITE {
            return Err(error(ErrorKind::InvalidInput, "invalid open options"));
        }
        if o.truncate && M::APPEND {
            return Err(error(
                ErrorKind::InvalidInput,
                "cannot truncate in append mode",
            ));
        }
        let inode = fs_op(states.inner(), op.path, |w, key| {
            let now = w.wall();
            match w.fs().nodes.get(&key) {
                Some(_) if o.create_new => Err(error(ErrorKind::AlreadyExists, "already exists")),
                Some(_) => {
                    let inode = w.fs().file(&key)?;
                    if o.truncate {
                        w.fs().inodes[inode].bytes.clear();
                        w.fs().inodes[inode].modified = now;
                    }
                    Ok(inode)
                }
                None if o.create || o.create_new => w.fs().write(&key, Vec::new(), now),
                None => Err(w.fs().missing(&key)),
            }
        })?;
        let raw = RawFile {
            world: states.shared(),
            inode,
            pos: 0,
            read: M::READ,
            write: M::WRITE,
            append: M::APPEND,
        };
        Ok(File(raw, PhantomData))
    }
}

/// Writes land immediately, so closing has nothing left to do.
impl<M: Mode> Simulate<Closing<Open<'_, M, File<M>>>> for fx::Open<M> {
    fn run(_: &mut States<'_>, Closing(_, _): Closing<Open<'_, M, File<M>>>) -> io::Result<()> {
        Ok(())
    }
}

/// An open in-memory file in mode `M`, acquired with [`Open`]. It performs
/// `Read` if `M` is readable, and `Write`, `Flush`, `SetLen`, `SyncAll` and
/// `SyncData` if `M` is writable; `Seek` and `Stat` in any mode.
#[derive(Debug)]
pub struct File<M>(RawFile, PhantomData<fn() -> M>);

macro_rules! file_ops {
    ($([$($g:tt)*] $op:ty => $out:ty;)*) => {$(
        impl<$($g)*> Perform<$op> for File<M> {
            fn perform(&mut self, op: $op) -> $out {
                self.0.perform(op)
            }
        }
    )*};
}

file_ops! {
    [M: Readable] Read<'_> => io::Result<usize>;
    [M: Writable] Write<'_> => io::Result<usize>;
    [M: Writable] Flush => io::Result<()>;
    [M: Writable] SetLen => io::Result<()>;
    [M: Writable] SyncAll => io::Result<()>;
    [M: Writable] SyncData => io::Result<()>;
    [M] Seek => io::Result<u64>;
}

impl<M> Inspect<Stat> for File<M> {
    fn inspect(&self, op: Stat) -> io::Result<FileInfo> {
        self.0.inspect(op)
    }
}

/// An in-memory regular file open to read at offsets, lent with [`View`].
/// It inspects [`ReadAt`] and [`Stat`].
#[derive(Debug)]
pub struct FileView(RawFile);

impl Simulate<Opening<View<'_, FileView>>> for fx::View {
    fn run(
        states: &mut States,
        Opening(op, _): Opening<View<'_, FileView>>,
    ) -> io::Result<FileView> {
        let inode = fs_op(states.inner(), op.path, |w, key| w.fs().file(&key))?;
        Ok(FileView(RawFile {
            world: states.shared(),
            inode,
            pos: 0,
            read: true,
            write: false,
            append: false,
        }))
    }
}

impl Simulate<Closing<View<'_, FileView>>> for fx::View {
    fn run(_: &mut States<'_>, _: Closing<View<'_, FileView>>) -> io::Result<()> {
        Ok(())
    }
}

impl Inspect<ReadAt<'_>> for FileView {
    fn inspect(&self, op: ReadAt<'_>) -> io::Result<usize> {
        let mut world = self.0.world.lock();
        let bytes = &world.fs().inodes[self.0.inode].bytes;
        let rest = usize::try_from(op.offset)
            .ok()
            .and_then(|at| bytes.get(at..))
            .unwrap_or_default();
        let n = rest.len().min(op.buf.len());
        op.buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

impl Inspect<Stat> for FileView {
    fn inspect(&self, op: Stat) -> io::Result<FileInfo> {
        self.0.inspect(op)
    }
}

/// An open file, whatever its mode: what `File` and `AtomicFile` hand
/// their operations to.
#[derive(Debug)]
pub(crate) struct RawFile {
    world: Shared,
    inode: usize,
    pos: usize,
    read: bool,
    write: bool,
    append: bool,
}

impl RawFile {
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
        let now = world.wall();
        let inode = &mut world.fs().inodes[self.inode];
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

impl Perform<Read<'_>> for RawFile {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        self.readable()?;
        let mut world = self.world.lock();
        let rest = world.fs().inodes[self.inode]
            .bytes
            .get(self.pos..)
            .unwrap_or_default();
        let n = rest.len().min(op.0.len());
        op.0[..n].copy_from_slice(&rest[..n]);
        self.pos += n;
        Ok(n)
    }
}

impl Perform<Write<'_>> for RawFile {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.write_bytes(op.0)
    }
}

/// Writes land immediately, so there is nothing to flush.
impl Perform<Flush> for RawFile {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

impl Perform<Seek> for RawFile {
    fn perform(&mut self, op: Seek) -> io::Result<u64> {
        let len = self.world.lock().fs().inodes[self.inode].bytes.len() as u64;
        let pos = match op.0 {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => len.checked_add_signed(d),
            SeekFrom::Current(d) => (self.pos as u64).checked_add_signed(d),
        }
        .ok_or_else(|| {
            error(
                ErrorKind::InvalidInput,
                "seek to before the start of the file",
            )
        })?;
        self.pos = usize::try_from(pos)
            .map_err(|_| error(ErrorKind::InvalidInput, "seek past what memory can hold"))?;
        Ok(pos)
    }
}

/// Fails with `InvalidInput` unless the file is open for writing, as on
/// Linux.
impl Perform<SetLen> for RawFile {
    fn perform(&mut self, op: SetLen) -> io::Result<()> {
        if !self.write {
            return Err(error(ErrorKind::InvalidInput, "not open for writing"));
        }
        let len = usize::try_from(op.0)
            .map_err(|_| error(ErrorKind::InvalidInput, "longer than memory can hold"))?;
        let mut world = self.world.lock();
        let now = world.wall();
        let inode = &mut world.fs().inodes[self.inode];
        inode.bytes.resize(len, 0);
        inode.modified = now;
        Ok(())
    }
}

/// Nothing in the simulation is lost in a crash, so there is nothing to
/// sync.
impl Perform<SyncAll> for RawFile {
    fn perform(&mut self, _: SyncAll) -> io::Result<()> {
        Ok(())
    }
}

/// As for `SyncAll`.
impl Perform<SyncData> for RawFile {
    fn perform(&mut self, _: SyncData) -> io::Result<()> {
        Ok(())
    }
}

/// The file's metadata, even once its path is renamed or removed.
impl Inspect<Stat> for RawFile {
    fn inspect(&self, _: Stat) -> io::Result<FileInfo> {
        let mut world = self.world.lock();
        let inode = &world.fs().inodes[self.inode];
        Ok(FileInfo {
            kind: FileKind::File,
            len: inode.bytes.len() as u64,
            readonly: false,
            modified: Some(inode.modified),
        })
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
    file: RawFile,
    temp: Key,
    target: Key,
    committed: bool,
}

impl Simulate<Opening<CreateAtomic<'_, AtomicFile>>> for fx::CreateAtomic {
    fn run(
        states: &mut States,
        Opening(op, _): Opening<CreateAtomic<'_, AtomicFile>>,
    ) -> io::Result<AtomicFile> {
        let (temp, target, inode) = fs_op(states.inner(), op.path, |w, target| {
            let Some(name) = target.last() else {
                return Err(error(ErrorKind::IsADirectory, "is a directory"));
            };
            if let Some(Node::Dir { .. }) = w.fs().nodes.get(&target) {
                return Err(error(ErrorKind::IsADirectory, "is a directory"));
            }
            let mut temp = parent(&target).to_vec();
            let n = w.fs().next_temp;
            w.fs().next_temp += 1;
            let mut temp_name = OsString::from(".");
            temp_name.push(name);
            temp_name.push(std::format!(".seam-{n}.tmp"));
            temp.push(temp_name);
            let now = w.wall();
            let inode = w.fs().write(&temp, Vec::new(), now)?;
            Ok((temp, target, inode))
        })?;
        let file = RawFile {
            world: states.shared(),
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

impl Simulate<Closing<CreateAtomic<'_, AtomicFile>>> for fx::CreateAtomic {
    fn run(
        _: &mut States,
        Closing(file, _): Closing<CreateAtomic<'_, AtomicFile>>,
    ) -> io::Result<()> {
        file.commit()
    }
}

impl AtomicFile {
    fn commit(mut self) -> io::Result<()> {
        let mut world = self.file.world.lock();
        let now = world.wall();
        let fs = world.fs();
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
        if world.fs().nodes.get(&self.temp) == Some(&Node::File(self.file.inode)) {
            world.fs().nodes.remove(&self.temp);
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
    Flush => io::Result<()>,
    Seek => io::Result<u64>,
    SetLen => io::Result<()>,
    SyncAll => io::Result<()>,
    SyncData => io::Result<()>
}

impl Inspect<Stat> for AtomicFile {
    fn inspect(&self, op: Stat) -> io::Result<FileInfo> {
        self.file.inspect(op)
    }
}

macro_rules! fs_keys {
    ($($key:ident),*) => {$(
        impl Simulated for fx::$key {
            type State = Files;
        }
    )*};
}
fs_keys!(
    Metadata,
    CreateDir,
    RemoveFile,
    RemoveDir,
    Rename,
    CopyFile,
    CreateAtomic,
    View,
    ReadDir
);

impl<M> Simulated for fx::Open<M> {
    type State = Files;
}

// ---- world files ----

/// A path in an `fs` block: absolute at the top, relative inside a `dir`.
fn path_in(node: &WorldNode, parent: Option<&Path>) -> world_file::Result<PathBuf> {
    let arg = node
        .args()
        .first()
        .ok_or_else(|| node.error(std::format!("`{}` takes a path first", node.name())))?;
    let path = Path::new(arg.string("a path")?);
    match parent {
        None => Ok(path.to_path_buf()),
        Some(_) if path.is_absolute() => Err(arg.error("inside a `dir`, paths are relative to it")),
        Some(parent) => Ok(parent.join(path)),
    }
}

/// A key as an absolute path.
fn path_of(key: &[OsString]) -> PathBuf {
    let mut path = PathBuf::from("/");
    path.extend(key);
    path
}

impl Files {
    /// Resolves an absolute path from a world file.
    fn key(&self, path: &Path) -> io::Result<Key> {
        self.resolve(Path::new("/"), path)
    }

    fn load_node(&mut self, node: &WorldNode, within: Option<&Path>) -> world_file::Result<()> {
        let path = match node.name() {
            "dir" | "file" => path_in(node, within)?,
            _ => return Err(node.unknown("`fs`", &["dir", "file"])),
        };
        let failed =
            |e: io::Error| node.error(std::format!("cannot create `{}`: {e}", path.display()));
        let now = std::time::UNIX_EPOCH;
        let key = self.key(&path).map_err(failed)?;
        if node.name() == "dir" {
            node.props_only(&[])?;
            node.arity(1, Some(1), "a path, then a block of its contents")?;
            self.create_dir(&key, true, now).map_err(failed)?;
            for child in node.children() {
                self.load_node(child, Some(&path))?;
            }
        } else {
            let bytes = node.contents(1, "a path and its contents, or a path and `hex=`")?;
            self.create_dir(&parent(&key).to_vec(), true, now)
                .map_err(failed)?;
            self.write(&key, bytes, now).map_err(failed)?;
        }
        Ok(())
    }

    fn check_node(
        &self,
        node: &WorldNode,
        parent: Option<&Path>,
        failures: &mut Vec<Failure>,
    ) -> world_file::Result<()> {
        let path = match node.name() {
            "dir" | "file" | "absent" => path_in(node, parent)?,
            _ => {
                return Err(node.unknown("`fs` expectations", &["dir", "file", "absent"]));
            }
        };
        let mut fail = |message: std::string::String| {
            failures.push(node.failure(std::format!("`{}`: {message}", path.display())));
        };
        let found = self
            .key(&path)
            .ok()
            .and_then(|k| self.nodes.get(&k).copied());
        match node.name() {
            "absent" => {
                node.no_children()?;
                node.props_only(&[])?;
                node.arity(1, Some(1), "a path")?;
                if found.is_some() {
                    fail("expected nothing there, but it exists".into());
                }
            }
            "dir" => {
                node.props_only(&[])?;
                node.arity(1, Some(1), "a path, then a block of what it holds")?;
                match found {
                    Some(Node::Dir { .. }) => {
                        for child in node.children() {
                            self.check_node(child, Some(&path), failures)?;
                        }
                    }
                    Some(_) => fail("expected a directory, but it is a file".into()),
                    None => fail("expected a directory, but nothing is there".into()),
                }
            }
            _ => {
                let want = node.contents(1, "a path and its expected contents, or `hex=`")?;
                match found {
                    Some(Node::File(inode)) if self.inodes[inode].bytes == want => {}
                    Some(Node::File(inode)) => fail(std::format!(
                        "expected {}, found {}",
                        show(&want),
                        show(&self.inodes[inode].bytes)
                    )),
                    Some(Node::Dir { .. }) => fail("expected a file, but it is a directory".into()),
                    None => fail("expected a file, but nothing is there".into()),
                }
            }
        }
        Ok(())
    }
}

/// Its block is `fs { dir P { .. }; file P CONTENTS; file P hex=".." }`.
/// Written out, every directory and file is listed by its absolute path.
/// As expectations, `absent P` expects nothing there, and a `dir`'s block
/// holds expectations about what is in it.
impl SimState for Files {
    const BLOCKS: &'static [&'static str] = &["fs"];

    fn load(&mut self, block: &WorldNode) -> world_file::Result<()> {
        for child in block.block()? {
            self.load_node(child, None)?;
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<WorldNode> {
        let mut block = WorldNode::new("fs");
        for (key, node) in self.nodes.iter().filter(|(k, _)| !k.is_empty()) {
            let path = path_of(key).to_string_lossy().into_owned();
            block = block.with_child(match node {
                Node::Dir { .. } => WorldNode::new("dir").with_arg(path),
                Node::File(inode) => WorldNode::new("file")
                    .with_arg(path)
                    .with_bytes(&self.inodes[*inode].bytes),
            });
        }
        std::vec![block]
    }

    fn check(&self, block: &WorldNode, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        for child in block.block()? {
            self.check_node(child, None, failures)?;
        }
        Ok(())
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for AtomicFile {}
impl<M> seam_core::Capability for File<M> {}
impl seam_core::Capability for FileView {}
impl seam_core::Capability for Dir {}
