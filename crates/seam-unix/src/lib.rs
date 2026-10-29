//! Unix effects for seam: what Unix programs do beyond files and sockets.
//!
//! | Operation | Does |
//! |---|---|
//! | [`Mount`], [`Unmount`] | mount and unmount filesystems, bind mounts included (Linux) |
//! | [`Chmod`], [`Chown`] | change a path's permissions and owner |
//! | [`Symlink`], [`ReadLink`] (query) | make and read symbolic links |
//! | [`Ids`] (query) | the process's user and group IDs |
//! | [`ProcessId`] (query) | the process's ID |
//! | [`Hostname`] (query) | the machine's name |
//! | [`Kill`] | send a signal to a process |
//! | [`SetUmask`] | set the file mode creation mask |
//!
//! This crate defines the operations, with their keys in [`effects`], and
//! [`SimUnix`], a simulated handler: a table of mounts, modes, owners and
//! links, a set of processes and a user, that checks permissions as Unix
//! does (only root mounts or changes owners). With the `test` feature it
//! reads and writes a `unix` world-file block. The real handlers are
//! `seam-rustix`'s `Rustix` and `seam-nix`'s `Nix`.
//!
//! ```
//! use seam_core::prelude::*;
//! use seam_unix::{Mount, SimUnix, Unmount};
//! use seam_test::Sim;
//!
//! /// Bind-mounts `from` over `to` for the length of `body`.
//! fn with_bind<C, R>(c: &mut C, from: &str, to: &str, body: impl FnOnce(&mut C) -> R)
//!     -> seam_core::io::Result<R>
//! where
//!     C: for<'a> Perform<Mount<'a>> + for<'a> Perform<Unmount<'a>>,
//! {
//!     c.perform(Mount::bind(from, to))?;
//!     let out = body(c);
//!     c.perform(Unmount::new(to))?;
//!     Ok(out)
//! }
//!
//! let mut sim = Sim::new();
//! sim.add_dir("/data").add_dir("/mnt");
//! let mut unix = SimUnix::new(sim.clone());
//! let mounted = with_bind(&mut unix, "/data", "/mnt", |u| u.mounts().len()).unwrap();
//! assert_eq!(mounted, 1);
//! assert!(unix.mounts().is_empty());
//!
//! // Only root can mount.
//! unix.set_user(1000, 1000);
//! let refused = unix.perform(Mount::bind("/data", "/mnt"));
//! assert_eq!(refused.unwrap_err().kind(), seam_core::io::ErrorKind::PermissionDenied);
//! ```
//! # Handlers
//!
//! | Handler | Crate | Use it for |
//! |---|---|---|
//! | `Rustix` | `seam-rustix` | a program, through rustix |
//! | `Nix` | `seam-nix` | a program, through nix (the same operations; pick the crate the program already uses) |
//! | [`SimUnix`] | this one | tests: permissions checked as Unix does, over a `Sim`'s filesystem |
//!
//! All are sync and return at once, so the lifts make them async for any
//! executor. In a root, each serves the keys of [`effects`] it is listed for.
//!
//! With the `test` feature, `SimUnix` reads and writes a `unix` world-file
//! block:
//!
//! ```text
//! unix {
//!     user uid=1000 gid=1000
//!     hostname "box"
//!     mount "/mnt" source="/data" fstype="ext4" flags="bind,rdonly"
//!     mode "/etc/app" 420
//! }
//! ```
//!
//! Through the `seam` crate, this crate is `seam::unix` (feature `unix`;
//! `rustix` and `nix` bring in the handlers).
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `test` | no | `World` for [`SimUnix`]: the `unix` world-file block |
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::io;
use seam_core::ops::fs::Metadata;
use seam_core::{Effect, Inspect, Operation, Perform, Query};

/// Flags for [`Mount`], as Linux's `MS_*`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MountFlags(u32);

impl MountFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// A bind mount: make `source`, already mounted somewhere, appear at
    /// `target` too.
    pub const BIND: Self = Self(1);
    /// Read-only.
    pub const RDONLY: Self = Self(1 << 1);
    /// Ignore set-user-ID and set-group-ID bits.
    pub const NOSUID: Self = Self(1 << 2);
    /// No device files.
    pub const NODEV: Self = Self(1 << 3);
    /// No executing programs.
    pub const NOEXEC: Self = Self(1 << 4);
    /// Recursively: with `BIND`, the mounts under `source` too.
    pub const REC: Self = Self(1 << 5);
    /// Change the flags of the mount at `target`.
    pub const REMOUNT: Self = Self(1 << 6);

    /// Every flag, with its name in world files.
    pub const NAMED: [(Self, &'static str); 7] = [
        (Self::BIND, "bind"),
        (Self::RDONLY, "rdonly"),
        (Self::NOSUID, "nosuid"),
        (Self::NODEV, "nodev"),
        (Self::NOEXEC, "noexec"),
        (Self::REC, "rec"),
        (Self::REMOUNT, "remount"),
    ];

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The flags as bits, in this crate's own numbering.
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl core::ops::BitOr for MountFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Mount a filesystem at `target`. Fails with `PermissionDenied` unless
/// the process may (usually: it is root).
#[derive(Debug, Clone, Copy)]
pub struct Mount<'a> {
    /// What to mount: a device, a directory to bind, or `None` (`proc`,
    /// `tmpfs` and a remount need none).
    pub source: Option<&'a Path>,
    /// Where.
    pub target: &'a Path,
    /// The filesystem type (`ext4`, `tmpfs`, ...); `None` for a bind
    /// mount or a remount.
    pub fstype: Option<&'a str>,
    /// How.
    pub flags: MountFlags,
    /// Options for the filesystem (`size=10m`).
    pub data: Option<&'a str>,
}

impl<'a> Mount<'a> {
    /// A bind mount of `source` at `target`.
    pub fn bind(
        source: &'a (impl AsRef<Path> + ?Sized),
        target: &'a (impl AsRef<Path> + ?Sized),
    ) -> Self {
        Self {
            source: Some(source.as_ref()),
            target: target.as_ref(),
            fstype: None,
            flags: MountFlags::BIND,
            data: None,
        }
    }

    /// Mounts `source`, a filesystem of type `fstype`, at `target`.
    pub fn new(
        source: &'a (impl AsRef<Path> + ?Sized),
        target: &'a (impl AsRef<Path> + ?Sized),
        fstype: &'a str,
    ) -> Self {
        Self {
            source: Some(source.as_ref()),
            target: target.as_ref(),
            fstype: Some(fstype),
            flags: MountFlags::NONE,
            data: None,
        }
    }

    /// Adds `flags`.
    #[must_use]
    pub fn flags(mut self, flags: MountFlags) -> Self {
        self.flags = self.flags | flags;
        self
    }
}

/// Unmount the filesystem mounted at `target`; with `lazy`, detach it now
/// and finish when it is no longer busy. Fails with `InvalidInput` if
/// nothing is mounted there.
#[derive(Debug, Clone, Copy)]
pub struct Unmount<'a> {
    /// Where it is mounted.
    pub target: &'a Path,
    /// Detach now, clean up when no longer busy (`MNT_DETACH`).
    pub lazy: bool,
}

impl<'a> Unmount<'a> {
    /// Unmounts what is at `target`.
    pub fn new(target: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            target: target.as_ref(),
            lazy: false,
        }
    }
}

/// Set `path`'s permission bits to `mode` (`0o644`, ...).
#[derive(Debug, Clone, Copy)]
pub struct Chmod<'a> {
    /// The path.
    pub path: &'a Path,
    /// The mode: permission bits, and the set-ID and sticky bits.
    pub mode: u32,
}

impl<'a> Chmod<'a> {
    /// Describes setting `path`'s mode.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized), mode: u32) -> Self {
        Self {
            path: path.as_ref(),
            mode,
        }
    }
}

/// Change `path`'s owner and group; `None` leaves one as it is.
#[derive(Debug, Clone, Copy)]
pub struct Chown<'a> {
    /// The path.
    pub path: &'a Path,
    /// The new owner.
    pub uid: Option<u32>,
    /// The new group.
    pub gid: Option<u32>,
}

impl<'a> Chown<'a> {
    /// Describes changing `path`'s owner and group.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized), uid: Option<u32>, gid: Option<u32>) -> Self {
        Self {
            path: path.as_ref(),
            uid,
            gid,
        }
    }
}

/// Make `link` a symbolic link to `original`. Fails with `AlreadyExists`
/// if `link` exists.
#[derive(Debug, Clone, Copy)]
pub struct Symlink<'a> {
    /// What the link points to. It need not exist.
    pub original: &'a Path,
    /// The link to make.
    pub link: &'a Path,
}

impl<'a> Symlink<'a> {
    /// Describes making `link` point to `original`.
    pub fn new(
        original: &'a (impl AsRef<Path> + ?Sized),
        link: &'a (impl AsRef<Path> + ?Sized),
    ) -> Self {
        Self {
            original: original.as_ref(),
            link: link.as_ref(),
        }
    }
}

/// Read where the symbolic link `path` points. Fails with `InvalidInput`
/// if it is not a link, `NotFound` if it does not exist.
#[derive(Debug, Clone, Copy)]
pub struct ReadLink<'a>(pub &'a Path);

impl<'a> ReadLink<'a> {
    /// Describes reading the link at `path`.
    pub fn new(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self(path.as_ref())
    }
}

/// The process's user and group IDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ids;

/// What [`Ids`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct UserIds {
    /// The real user ID.
    pub uid: u32,
    /// The real group ID.
    pub gid: u32,
    /// The effective user ID: the one permissions are checked against.
    pub euid: u32,
    /// The effective group ID.
    pub egid: u32,
}

impl UserIds {
    /// Whether the process runs as root.
    pub fn is_root(&self) -> bool {
        self.euid == 0
    }
}

/// The process's ID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessId;

/// The machine's host name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hostname;

/// Send `signal` to the process `pid`. A signal of 0 sends nothing and only
/// checks that the process exists. Fails with `NotFound` if it does not,
/// `PermissionDenied` if the process may not signal it. See [`signals`]
/// for the common numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kill {
    /// The process.
    pub pid: u32,
    /// The signal's number.
    pub signal: i32,
}

/// The common signals' numbers, as on Linux (and most Unixes).
pub mod signals {
    /// Hang up.
    pub const SIGHUP: i32 = 1;
    /// Interrupt (Ctrl-C).
    pub const SIGINT: i32 = 2;
    /// Quit.
    pub const SIGQUIT: i32 = 3;
    /// Kill: cannot be caught.
    pub const SIGKILL: i32 = 9;
    /// User-defined 1.
    pub const SIGUSR1: i32 = 10;
    /// User-defined 2.
    pub const SIGUSR2: i32 = 12;
    /// Terminate.
    pub const SIGTERM: i32 = 15;
}

/// Set the file mode creation mask to `0`, returning the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetUmask(pub u32);

macro_rules! outcomes {
    ($($(#[$q:ident])? $op:ty => $out:ty;)*) => {$(
        impl Operation for $op {
            type Outcome = $out;
        }
        $(outcomes!(@$q $op);)?
    )*};
    (@query $op:ty) => {
        impl Query for $op {}
    };
}

outcomes! {
    Mount<'_> => io::Result<()>;
    Unmount<'_> => io::Result<()>;
    Chmod<'_> => io::Result<()>;
    Chown<'_> => io::Result<()>;
    Symlink<'_> => io::Result<()>;
    #[query] ReadLink<'_> => io::Result<PathBuf>;
    #[query] Ids => UserIds;
    #[query] ProcessId => u32;
    #[query] Hostname => io::Result<String>;
    Kill => io::Result<()>;
    SetUmask => u32;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    macro_rules! keys {
        ($($name:ident),*) => {$(
            #[doc = concat!("Routes [`", stringify!($name), "`](crate::", stringify!($name), ").")]
            #[derive(Debug, Clone, Copy)]
            pub struct $name;
        )*};
    }
    keys!(
        Mount, Unmount, Chmod, Chown, Symlink, ReadLink, Ids, ProcessId, Hostname, Kill, SetUmask
    );
}

macro_rules! keyed {
    ($($op:ty => $key:ident;)*) => {$(
        impl Effect for $op {
            type Key = effects::$key;
        }
    )*};
}

keyed! {
    Mount<'_> => Mount;
    Unmount<'_> => Unmount;
    Chmod<'_> => Chmod;
    Chown<'_> => Chown;
    Symlink<'_> => Symlink;
    ReadLink<'_> => ReadLink;
    Ids => Ids;
    ProcessId => ProcessId;
    Hostname => Hostname;
    Kill => Kill;
    SetUmask => SetUmask;
}

/// A mount [`SimUnix`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounted {
    /// What was mounted.
    pub source: Option<PathBuf>,
    /// Where.
    pub target: PathBuf,
    /// Its filesystem type.
    pub fstype: Option<String>,
    /// Its flags (`REMOUNT` is not kept).
    pub flags: MountFlags,
}

#[derive(Debug)]
struct State {
    ids: UserIds,
    pid: u32,
    hostname: String,
    umask: u32,
    mounts: Vec<Mounted>,
    modes: BTreeMap<PathBuf, u32>,
    owners: BTreeMap<PathBuf, (u32, u32)>,
    links: BTreeMap<PathBuf, PathBuf>,
    processes: BTreeSet<u32>,
    signals: Vec<(u32, i32)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ids: UserIds::default(),
            pid: 1000,
            hostname: String::from("sim"),
            umask: 0o022,
            mounts: Vec::new(),
            modes: BTreeMap::new(),
            owners: BTreeMap::new(),
            links: BTreeMap::new(),
            processes: BTreeSet::new(),
            signals: Vec::new(),
        }
    }
}

/// A simulated Unix: a mount table, permissions and owners, symbolic
/// links, a user, a host name and a set of processes, over the filesystem
/// of `C` (which says what exists).
///
/// It starts as root (all IDs 0), as process 1000 on host `sim`, with
/// umask `0o022`. Permissions are checked as Unix does: only root mounts,
/// unmounts or changes owners, and only root or a path's owner changes
/// its mode (a path's owner is root until changed). Links live here, not
/// in `C`'s filesystem.
///
/// Every clone is a handle to the same state.
#[derive(Debug, Clone)]
pub struct SimUnix<C> {
    fs: C,
    state: Arc<Mutex<State>>,
}

impl<C> SimUnix<C> {
    /// A fresh Unix over the filesystem of `fs`.
    pub fn new(fs: C) -> Self {
        Self {
            fs,
            state: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs as user `uid` and group `gid` (real and effective).
    pub fn set_user(&mut self, uid: u32, gid: u32) -> &mut Self {
        self.lock().ids = UserIds {
            uid,
            gid,
            euid: uid,
            egid: gid,
        };
        self
    }

    /// Sets the host name.
    pub fn set_hostname(&mut self, name: &str) -> &mut Self {
        self.lock().hostname = name.into();
        self
    }

    /// Sets this process's ID.
    pub fn set_pid(&mut self, pid: u32) -> &mut Self {
        self.lock().pid = pid;
        self
    }

    /// Adds a process that signals can be sent to.
    pub fn add_process(&mut self, pid: u32) -> &mut Self {
        self.lock().processes.insert(pid);
        self
    }

    /// The mounts, oldest first.
    pub fn mounts(&self) -> Vec<Mounted> {
        self.lock().mounts.clone()
    }

    /// `path`'s mode, if one was set.
    pub fn mode(&self, path: impl AsRef<Path>) -> Option<u32> {
        self.lock().modes.get(path.as_ref()).copied()
    }

    /// `path`'s owner and group, if they were changed.
    pub fn owner(&self, path: impl AsRef<Path>) -> Option<(u32, u32)> {
        self.lock().owners.get(path.as_ref()).copied()
    }

    /// The signals sent, as `(pid, signal)`, in order. Signal 0 is not
    /// recorded.
    pub fn signals(&self) -> Vec<(u32, i32)> {
        self.lock().signals.clone()
    }
}

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "operation not permitted")
}

impl<C: for<'a> Inspect<Metadata<'a>>> SimUnix<C> {
    /// Whether `path` exists, as a file in `C` or a link here.
    fn exists(&self, state: &State, path: &Path) -> io::Result<bool> {
        if state.links.contains_key(path) {
            return Ok(true);
        }
        match self.fs.inspect(Metadata::new(path)) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn must_exist(&self, state: &State, path: &Path) -> io::Result<()> {
        if self.exists(state, path)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such file or directory",
            ))
        }
    }
}

impl<C: for<'a> Inspect<Metadata<'a>>> Perform<Mount<'_>> for SimUnix<C> {
    fn perform(&mut self, op: Mount<'_>) -> io::Result<()> {
        let mut state = self.lock();
        if !state.ids.is_root() {
            return Err(denied());
        }
        self.must_exist(&state, op.target)?;
        if op.flags.contains(MountFlags::REMOUNT) {
            let mount = state
                .mounts
                .iter_mut()
                .rev()
                .find(|m| m.target == op.target)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not mounted"))?;
            mount.flags =
                MountFlags(op.flags.0 & !MountFlags::REMOUNT.0) | (mount.flags & MountFlags::BIND);
            return Ok(());
        }
        if op.flags.contains(MountFlags::BIND) {
            let source = op.source.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "a bind mount needs a source")
            })?;
            self.must_exist(&state, source)?;
        }
        state.mounts.push(Mounted {
            source: op.source.map(Path::to_path_buf),
            target: op.target.to_path_buf(),
            fstype: op.fstype.map(String::from),
            flags: op.flags,
        });
        Ok(())
    }
}

impl core::ops::BitAnd for MountFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl<C> Perform<Unmount<'_>> for SimUnix<C> {
    fn perform(&mut self, op: Unmount<'_>) -> io::Result<()> {
        let mut state = self.lock();
        if !state.ids.is_root() {
            return Err(denied());
        }
        let i = state
            .mounts
            .iter()
            .rposition(|m| m.target == op.target)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not mounted"))?;
        state.mounts.remove(i);
        Ok(())
    }
}

impl<C: for<'a> Inspect<Metadata<'a>>> Perform<Chmod<'_>> for SimUnix<C> {
    fn perform(&mut self, op: Chmod<'_>) -> io::Result<()> {
        let mut state = self.lock();
        self.must_exist(&state, op.path)?;
        let owner = state.owners.get(op.path).map_or(0, |o| o.0);
        if !state.ids.is_root() && state.ids.euid != owner {
            return Err(denied());
        }
        state.modes.insert(op.path.to_path_buf(), op.mode & 0o7777);
        Ok(())
    }
}

impl<C: for<'a> Inspect<Metadata<'a>>> Perform<Chown<'_>> for SimUnix<C> {
    fn perform(&mut self, op: Chown<'_>) -> io::Result<()> {
        let mut state = self.lock();
        self.must_exist(&state, op.path)?;
        if !state.ids.is_root() {
            return Err(denied());
        }
        let (uid, gid) = state.owners.get(op.path).copied().unwrap_or((0, 0));
        state.owners.insert(
            op.path.to_path_buf(),
            (op.uid.unwrap_or(uid), op.gid.unwrap_or(gid)),
        );
        Ok(())
    }
}

impl<C: for<'a> Inspect<Metadata<'a>>> Perform<Symlink<'_>> for SimUnix<C> {
    fn perform(&mut self, op: Symlink<'_>) -> io::Result<()> {
        let mut state = self.lock();
        if self.exists(&state, op.link)? {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file exists"));
        }
        state
            .links
            .insert(op.link.to_path_buf(), op.original.to_path_buf());
        Ok(())
    }
}

impl<C: for<'a> Inspect<Metadata<'a>>> Inspect<ReadLink<'_>> for SimUnix<C> {
    fn inspect(&self, op: ReadLink<'_>) -> io::Result<PathBuf> {
        let state = self.lock();
        if let Some(original) = state.links.get(op.0) {
            return Ok(original.clone());
        }
        self.must_exist(&state, op.0)?;
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a symbolic link",
        ))
    }
}

impl<C> Inspect<Ids> for SimUnix<C> {
    fn inspect(&self, _: Ids) -> UserIds {
        self.lock().ids
    }
}

impl<C> Inspect<ProcessId> for SimUnix<C> {
    fn inspect(&self, _: ProcessId) -> u32 {
        self.lock().pid
    }
}

impl<C> Inspect<Hostname> for SimUnix<C> {
    fn inspect(&self, _: Hostname) -> io::Result<String> {
        Ok(self.lock().hostname.clone())
    }
}

impl<C> Perform<Kill> for SimUnix<C> {
    fn perform(&mut self, op: Kill) -> io::Result<()> {
        let mut state = self.lock();
        if op.pid != state.pid && !state.processes.contains(&op.pid) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such process"));
        }
        if op.signal != 0 {
            state.signals.push((op.pid, op.signal));
            if op.signal == signals::SIGKILL || op.signal == signals::SIGTERM {
                state.processes.remove(&op.pid);
            }
        }
        Ok(())
    }
}

impl<C> Perform<SetUmask> for SimUnix<C> {
    fn perform(&mut self, op: SetUmask) -> u32 {
        core::mem::replace(&mut self.lock().umask, op.0 & 0o777)
    }
}

impl<C> seam_core::Capability for SimUnix<C> {}

#[cfg(feature = "test")]
mod world {
    use seam_test::world_file::{self, Node};

    use super::*;

    fn flags_text(flags: MountFlags) -> String {
        let names: Vec<&str> = MountFlags::NAMED
            .iter()
            .filter(|(f, _)| flags.contains(*f))
            .map(|(_, n)| *n)
            .collect();
        names.join(",")
    }

    /// `unix { user uid=0 gid=0; hostname "box"; pid 1000; umask 18;
    /// process 4242; mount "/mnt" source="/data" fstype="ext4" flags="bind,rdonly";
    /// mode "/etc/app" 420; owner "/etc/app" uid=1 gid=1; link "/l" "/target";
    /// signal 4242 15 }`. Loading sets them up; written out, it lists
    /// them; checking finds each expected one.
    impl<C> seam_test::World for SimUnix<C>
    where
        C: Send + 'static,
    {
        const BLOCKS: &'static [&'static str] = &["unix"];

        fn load(&mut self, block: &Node) -> world_file::Result<()> {
            let mut state = self.lock();
            for child in block.block()? {
                child.no_children()?;
                match child.name() {
                    "user" => {
                        child.props_only(&["uid", "gid"])?;
                        child.arity(0, Some(0), "no arguments")?;
                        let get = |k| child.prop(k).map(|v| v.uint::<u32>(k)).transpose();
                        let uid = get("uid")?.unwrap_or(0);
                        let gid = get("gid")?.unwrap_or(uid);
                        state.ids = UserIds {
                            uid,
                            gid,
                            euid: uid,
                            egid: gid,
                        };
                    }
                    "hostname" => {
                        child.props_only(&[])?;
                        child.arity(1, Some(1), "the host name")?;
                        state.hostname = child.args()[0].string("the host name")?.into();
                    }
                    "pid" | "umask" | "process" => {
                        child.props_only(&[])?;
                        child.arity(1, Some(1), "a number")?;
                        let n = child.args()[0].uint::<u32>("the number")?;
                        match child.name() {
                            "pid" => state.pid = n,
                            "umask" => state.umask = n,
                            _ => drop(state.processes.insert(n)),
                        }
                    }
                    "mount" => {
                        child.props_only(&["source", "fstype", "flags"])?;
                        child.arity(1, Some(1), "the target")?;
                        let target = PathBuf::from(child.args()[0].string("the target")?);
                        let text = |k| {
                            child
                                .prop(k)
                                .map(|v| v.string(k).map(String::from))
                                .transpose()
                        };
                        let mut flags = MountFlags::NONE;
                        if let Some(v) = child.prop("flags") {
                            for name in v.string("the flags")?.split(',').filter(|n| !n.is_empty())
                            {
                                let flag = MountFlags::NAMED
                                    .iter()
                                    .find(|(_, n)| *n == name)
                                    .ok_or_else(|| {
                                        v.error(std::format!("`{name}` is not a mount flag"))
                                    })?;
                                flags = flags | flag.0;
                            }
                        }
                        state.mounts.push(Mounted {
                            source: text("source")?.map(PathBuf::from),
                            target,
                            fstype: text("fstype")?,
                            flags,
                        });
                    }
                    "mode" => {
                        child.props_only(&[])?;
                        child.arity(2, Some(2), "a path and a mode")?;
                        let path = PathBuf::from(child.args()[0].string("the path")?);
                        state.modes.insert(path, child.args()[1].uint("the mode")?);
                    }
                    "owner" => {
                        child.props_only(&["uid", "gid"])?;
                        child.arity(1, Some(1), "a path")?;
                        let path = PathBuf::from(child.args()[0].string("the path")?);
                        let get = |k| child.prop(k).map(|v| v.uint::<u32>(k)).transpose();
                        state
                            .owners
                            .insert(path, (get("uid")?.unwrap_or(0), get("gid")?.unwrap_or(0)));
                    }
                    "link" => {
                        child.props_only(&[])?;
                        child.arity(2, Some(2), "a link and what it points to")?;
                        let link = PathBuf::from(child.args()[0].string("the link")?);
                        let original = PathBuf::from(child.args()[1].string("the original")?);
                        state.links.insert(link, original);
                    }
                    "signal" => {
                        child.props_only(&[])?;
                        child.arity(2, Some(2), "a pid and a signal")?;
                        let pid = child.args()[0].uint("the pid")?;
                        let signal = child.args()[1].uint::<i32>("the signal")?;
                        state.signals.push((pid, signal));
                    }
                    _ => {
                        return Err(child.unknown(
                            "`unix`",
                            &[
                                "user", "hostname", "pid", "umask", "process", "mount", "mode",
                                "owner", "link", "signal",
                            ],
                        ));
                    }
                }
            }
            Ok(())
        }

        fn to_world(&self) -> Vec<Node> {
            let state = self.lock();
            let mut block = Node::new("unix")
                .with_child(
                    Node::new("user")
                        .with_prop("uid", state.ids.uid)
                        .with_prop("gid", state.ids.gid),
                )
                .with_child(Node::new("hostname").with_arg(state.hostname.as_str()))
                .with_child(Node::new("pid").with_arg(state.pid))
                .with_child(Node::new("umask").with_arg(state.umask));
            for pid in &state.processes {
                block = block.with_child(Node::new("process").with_arg(*pid));
            }
            for m in &state.mounts {
                let mut node = Node::new("mount").with_arg(m.target.to_string_lossy().into_owned());
                if let Some(source) = &m.source {
                    node = node.with_prop("source", source.to_string_lossy().into_owned());
                }
                if let Some(fstype) = &m.fstype {
                    node = node.with_prop("fstype", fstype.as_str());
                }
                if m.flags != MountFlags::NONE {
                    node = node.with_prop("flags", flags_text(m.flags));
                }
                block = block.with_child(node);
            }
            for (path, mode) in &state.modes {
                block = block.with_child(
                    Node::new("mode")
                        .with_arg(path.to_string_lossy().into_owned())
                        .with_arg(*mode),
                );
            }
            for (path, (uid, gid)) in &state.owners {
                block = block.with_child(
                    Node::new("owner")
                        .with_arg(path.to_string_lossy().into_owned())
                        .with_prop("uid", *uid)
                        .with_prop("gid", *gid),
                );
            }
            for (link, original) in &state.links {
                block = block.with_child(
                    Node::new("link")
                        .with_arg(link.to_string_lossy().into_owned())
                        .with_arg(original.to_string_lossy().into_owned()),
                );
            }
            for (pid, signal) in &state.signals {
                block = block.with_child(Node::new("signal").with_arg(*pid).with_arg(*signal));
            }
            std::vec![block]
        }
    }
}
