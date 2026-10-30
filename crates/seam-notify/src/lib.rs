//! File watching for seam.
//!
//! [`Watch`] is a scope: it lends a watcher for a path, and [`NextEvent`]
//! on the watcher waits for the next change under it (with
//! `Timeout<NextEvent>`, for at most a while). Two handlers provide it:
//!
//! - [`Notify`] (feature `notify`, on by default): the operating system's
//!   own events (inotify, FSEvents, ReadDirectoryChanges), through the
//!   notify crate.
//! - [`Polling`]: rescans the tree through the filesystem of the
//!   capability it holds, every so often, and reports what changed. Over a
//!   `Sim` it watches the simulated filesystem, on the virtual clock, so a
//!   test drives a watcher by changing files.
//!
//! Events are notify's own ([`Event`], [`EventKind`] and its kinds), from
//! every handler: creations and removals of files and folders, data and
//! metadata changes, renames, and (asked for with [`Watch::kinds`]) access.
//! A watch reports [`EventKindMask::CORE`] (all but access) unless told
//! otherwise; [`Notify::with_config`] takes notify's [`Config`].
//!
//! | Handler | Feature | Sync | Async |
//! |---|---|---|---|
//! | [`Notify`] | `notify` (default) | blocks until an event | waits without blocking, under any executor: notify's own thread wakes the task |
//! | [`Polling`] | none | sleeps between scans, through its capability | through the lifts: the same sleeps, so it blocks a real executor |
//!
//! So in a program use `Notify`, sync or async. `Polling` is for tests over
//! a `Sim` (where its sleeps are on the virtual clock), and for sync
//! programs on filesystems the OS does not report (some network mounts),
//! over `Std`.
//!
//! # In tests
//! ```
//! use core::time::Duration;
//! use seam_core::prelude::*;
//! use seam_core::effects as fx;
//! use seam_core::ops::fs::write_file;
//! use seam_core::ops::time::Timeout;
//! use seam_notify::{CreateKind, EventKind, NextEvent, Polling, Watch};
//! use seam_test::Sim;
//!
//! let mut sim = Sim::new();
//! sim.add_dir("/config");
//! let mut root = seam_core::root![
//!     Sim<fx::View, fx::Open<seam_core::ops::fs::WriteOnly>, fx::Metadata>::clone(&sim),
//!     Polling<seam_notify::effects::Watch>::new(sim.clone()),
//! ];
//! root.acquire(Watch::recursive("/config"), |root, watcher| {
//!     write_file(root, "/config/app.toml", "debug = true")?;
//!     let event = watcher.perform(NextEvent)?;
//!     assert_eq!(event.kind, EventKind::Create(CreateKind::File));
//!     // Nothing else changes: give up after a minute of virtual time.
//!     let quiet = watcher.perform(Timeout::new(Duration::from_secs(60), NextEvent));
//!     assert_eq!(quiet.unwrap_err().kind(), seam_core::io::ErrorKind::TimedOut);
//!     Ok::<_, seam_core::io::Error>(())
//! }).unwrap();
//! ```
//! # In a program
//!
//! ```no_run
//! # #[cfg(feature = "notify")] {
//! use seam_core::io;
//! use seam_core::ops::time::Timeout;
//! use seam_core::prelude::*;
//! use seam_notify::{Event, NextEvent, Notify, Watch};
//! use std::time::Duration;
//!
//! /// The core: waits for the config to change, for up to a minute.
//! async fn config_changed<C, W>(c: &mut C) -> io::Result<Option<Event>>
//! where
//!     C: for<'a> AsyncProvide<Watch<'a, W>>,
//!     W: SendPerform<Timeout<NextEvent>>,
//! {
//!     c.acquire_async(Watch::flat("/etc/myapp"), async |_, watcher| {
//!         match watcher.perform_send(Timeout::new(Duration::from_secs(60), NextEvent)).await {
//!             Ok(event) => Ok(Some(event)),
//!             Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(None),
//!             Err(e) => Err(e),
//!         }
//!     })
//!     .await
//! }
//!
//! // Any executor: Tokio here; smol, or futures-rs', the same.
//! let changed = seam_tokio::block_on(config_changed::<_, seam_notify::NotifyWatcher>(&mut Notify::new()))?;
//! println!("{changed:?}");
//! # }
//! # Ok::<(), seam_core::io::Error>(())
//! ```
//!
//! Through the `seam` crate, this crate is `seam::notify` (feature `notify`,
//! which brings in [`Notify`]).
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `notify` | yes | [`Notify`] and [`Config`]: the operating system's events, through the notify crate |
//!
#![cfg_attr(
    not(feature = "notify"),
    doc = "[`Notify`]: https://docs.rs/seam-rs-notify/latest/seam_notify/struct.Notify.html\n\
           [`Notify::with_config`]: https://docs.rs/seam-rs-notify/latest/seam_notify/struct.Notify.html#method.with_config\n\
           [`Config`]: https://docs.rs/seam-rs-notify/latest/seam_notify/struct.Config.html"
)]
#![forbid(unsafe_code)]

use core::marker::PhantomData;
use core::time::Duration;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::vec::Vec;

use seam_core::io;
use seam_core::ops::fs::{FileInfo, Metadata, NextEntry, ReadAt, ReadDir, View, read_file};
use seam_core::ops::time::{Now, Sleep, Timeout, TimesOut};
use seam_core::{
    Acquisition, Closing, Effect, Inspect, Lend, Opening, Operation, Perform, Provide,
};

/// Watch `path` (and, if `recursive`, everything under it), lending a
/// watcher of type `W` for the scope. Fails with `NotFound` if `path` does
/// not exist.
pub struct Watch<'a, W> {
    /// What to watch.
    pub path: &'a Path,
    /// Whether to watch the whole tree under a directory, or only its
    /// entries.
    pub recursive: bool,
    /// Which kinds of event to report: [`EventKindMask::CORE`] (all but
    /// access events, which are many) unless set.
    pub kinds: EventKindMask,
    watcher: PhantomData<fn() -> W>,
}

impl<'a, W> Watch<'a, W> {
    /// Watches `path` and every directory under it.
    pub fn recursive(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: true,
            kinds: EventKindMask::CORE,
            watcher: PhantomData,
        }
    }

    /// Watches `path` and, if it is a directory, its entries only.
    pub fn flat(path: &'a (impl AsRef<Path> + ?Sized)) -> Self {
        Self {
            path: path.as_ref(),
            recursive: false,
            kinds: EventKindMask::CORE,
            watcher: PhantomData,
        }
    }

    /// Reports only these kinds of event: `EventKindMask::ALL` for access
    /// events too.
    #[must_use]
    pub fn kinds(mut self, kinds: EventKindMask) -> Self {
        self.kinds = kinds;
        self
    }
}

impl<W> core::fmt::Debug for Watch<'_, W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Watch")
            .field("path", &self.path)
            .field("recursive", &self.recursive)
            .field("kinds", &self.kinds)
            .finish()
    }
}

impl<W> Acquisition for Watch<'_, W> {
    type Resource = W;
    type Error = io::Error;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    /// Routes [`Watch`](crate::Watch).
    #[derive(Debug, Clone, Copy)]
    pub struct Watch;
}

impl<W> Effect for Watch<'_, W> {
    type Key = effects::Watch;
}

/// Wait for the next change under a watched path. Performed on the
/// watcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NextEvent;

impl Operation for NextEvent {
    type Outcome = io::Result<Event>;
}

/// Gives up with `io::ErrorKind::TimedOut`; the change, if one comes
/// later, is reported by the next `NextEvent`.
impl TimesOut for NextEvent {
    type Expired = io::Result<Event>;
    fn timed_out(self) -> io::Result<Event> {
        Err(io::ErrorKind::TimedOut.into())
    }
}

// notify's events, as every handler reports them.
pub use notify_types::event::{
    AccessKind, AccessMode, CreateKind, DataChange, Event, EventAttributes, EventKind,
    EventKindMask, Flag, MetadataKind, ModifyKind, RemoveKind, RenameMode,
};

/// Watches by rescanning: built over a capability `C` with a filesystem
/// and a clock, it lends [`PollWatcher`]s that compare the tree (each
/// path's kind, size and modification time) every `interval`, sleeping in
/// between through `C`.
///
/// Size and modification time miss a rewrite that keeps the size within
/// the clock's resolution, as notify's own poll watcher does by default
/// (and a `Sim`'s clock does not move unless told to, so there they miss
/// every same-size rewrite). [`compare_contents`](Self::compare_contents)
/// compares each file's contents too, as notify's `with_compare_contents`
/// does, so every change to a file is seen.
///
/// Over a `Sim` the sleeps are virtual and nothing else changes the files
/// while one waits, so a `NextEvent` with nothing to report never returns:
/// change files before waiting, or wait with a `Timeout`.
#[derive(Debug, Clone)]
pub struct Polling<C, D, K = MetadataOnly> {
    fs: C,
    interval: Duration,
    dir: PhantomData<fn() -> (D, K)>,
}

/// What [`Polling`] compares a file by: implemented by [`MetadataOnly`]
/// (kind, size and modification time) and [`Contents`] (those, and the
/// contents). Sealed.
pub trait Compare<C>: sealed::Sealed {
    /// A digest of `path`'s contents, if this compares them.
    #[doc(hidden)]
    fn digest(fs: &C, path: &Path) -> Option<u64>;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::MetadataOnly {}
    impl<F> Sealed for super::Contents<F> {}
}

/// Compare files by kind, size and modification time: [`Polling`]'s
/// default.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetadataOnly;

impl<C> Compare<C> for MetadataOnly {
    fn digest(_: &C, _: &Path) -> Option<u64> {
        None
    }
}

/// Compare files by their contents too, read through views of type `F`
/// (`seam_test::fs::FileView` over a `Sim`, `seam_std::fs::FileView` over
/// `Std`): [`Polling::compare_contents`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Contents<F>(PhantomData<fn() -> F>);

impl<C, F> Compare<C> for Contents<F>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    fn digest(fs: &C, path: &Path) -> Option<u64> {
        // FNV-1a: enough to tell one version of a file from the next.
        let bytes = read_file(fs, path).ok()?;
        Some(bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
        }))
    }
}

impl<C, D> Polling<C, D>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>>,
    D: Perform<NextEntry>,
{
    /// Rescans through `fs` every second.
    pub fn new(fs: C) -> Self {
        Self::every(fs, Duration::from_secs(1))
    }

    /// Rescans through `fs` every `interval`.
    pub fn every(fs: C, interval: Duration) -> Self {
        Self {
            fs,
            interval,
            dir: PhantomData,
        }
    }
}

impl<C, D> Polling<C, D>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>>,
    D: Perform<NextEntry>,
{
    /// Rescans through `fs` every second, comparing each file's contents
    /// too: [`compare_contents`](Self::compare_contents). The view type is
    /// inferred from `fs`.
    ///
    /// ```
    /// use seam_notify::{Polling, effects as notify};
    /// use seam_test::Sim;
    ///
    /// let sim = Sim::new();
    /// let root = seam_core::root! {
    ///     Polling<notify::Watch>::comparing_contents(sim.clone()),
    /// };
    /// # let _ = root;
    /// ```
    pub fn comparing_contents<F>(fs: C) -> Polling<C, D, Contents<F>>
    where
        C: for<'a> Lend<View<'a, F>>,
        F: for<'a> Inspect<ReadAt<'a>>,
    {
        Self::new(fs).compare_contents()
    }
}

impl<C, D, K> Polling<C, D, K> {
    /// Compares each file's contents too, read through `C`'s views (of
    /// type `F`, inferred), so a rewrite that keeps the size and time is
    /// seen. Each rescan reads every watched file.
    pub fn compare_contents<F>(self) -> Polling<C, D, Contents<F>>
    where
        C: for<'a> Lend<View<'a, F>>,
        F: for<'a> Inspect<ReadAt<'a>>,
    {
        Polling {
            fs: self.fs,
            interval: self.interval,
            dir: PhantomData,
        }
    }
}

/// What a scan saw of each path: its metadata, and a digest of its
/// contents if they are compared.
type Seen = BTreeMap<PathBuf, (FileInfo, Option<u64>)>;

/// A watcher from [`Polling`].
#[derive(Debug)]
pub struct PollWatcher<C, D, K = MetadataOnly> {
    fs: C,
    root: PathBuf,
    recursive: bool,
    kinds: EventKindMask,
    interval: Duration,
    seen: Seen,
    pending: VecDeque<Event>,
    dir: PhantomData<fn() -> (D, K)>,
}

/// Everything under `root` (and `root` itself), as `fs` sees it now.
fn scan<C, D, K>(fs: &mut C, root: &Path, recursive: bool) -> io::Result<Seen>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>>,
    D: Perform<NextEntry>,
    K: Compare<C>,
{
    let mut seen = Seen::new();
    let top = fs.inspect(Metadata::new(root))?;
    let digest = if top.is_dir() {
        None
    } else {
        K::digest(fs, root)
    };
    seen.insert(root.to_path_buf(), (top, digest));
    if !top.is_dir() {
        return Ok(seen);
    }
    let mut dirs = Vec::from([root.to_path_buf()]);
    while let Some(dir) = dirs.pop() {
        let entries = fs.acquire(ReadDir::new(&dir), |_, entries| {
            let mut paths = Vec::new();
            while let Some(path) = entries.perform(NextEntry)? {
                paths.push(path);
            }
            io::Result::Ok(paths)
        });
        let entries = match entries {
            Ok(entries) => entries,
            // Removed between listing its parent and listing it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for path in entries {
            let Ok(info) = fs.inspect(Metadata::new(&path)) else {
                continue;
            };
            if info.is_dir() && recursive {
                dirs.push(path.clone());
            }
            let digest = if info.is_dir() {
                None
            } else {
                K::digest(fs, &path)
            };
            seen.insert(path, (info, digest));
        }
    }
    Ok(seen)
}

/// The changes from `old` to `new`, as notify reports them: an event per
/// path, created first, then modified, then removed.
fn diff(old: &Seen, new: &Seen) -> Vec<Event> {
    let mut created = Vec::new();
    let mut modified = Vec::new();
    let mut removed = Vec::new();
    for (path, (info, digest)) in new {
        match old.get(path) {
            None => {
                let kind = if info.is_dir() {
                    CreateKind::Folder
                } else {
                    CreateKind::File
                };
                created.push(Event::new(EventKind::Create(kind)).add_path(path.clone()));
            }
            Some((before, was)) if (before != info || was != digest) && !info.is_dir() => {
                let kind = if before.len != info.len
                    || before.modified != info.modified
                    || was != digest
                {
                    ModifyKind::Data(DataChange::Any)
                } else {
                    ModifyKind::Metadata(MetadataKind::Any)
                };
                modified.push(Event::new(EventKind::Modify(kind)).add_path(path.clone()));
            }
            Some(_) => {}
        }
    }
    for (path, (info, _)) in old {
        if !new.contains_key(path) {
            let kind = if info.is_dir() {
                RemoveKind::Folder
            } else {
                RemoveKind::File
            };
            removed.push(Event::new(EventKind::Remove(kind)).add_path(path.clone()));
        }
    }
    created.into_iter().chain(modified).chain(removed).collect()
}

impl<C, D, K> PollWatcher<C, D, K>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>>,
    D: Perform<NextEntry>,
    K: Compare<C>,
{
    /// Rescans, queueing what changed. Whether anything did.
    fn poll(&mut self) -> io::Result<bool> {
        let now = match scan::<C, D, K>(&mut self.fs, &self.root, self.recursive) {
            Ok(now) => now,
            // The watched path itself is gone: everything under it was
            // removed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Seen::new(),
            Err(e) => return Err(e),
        };
        let kinds = self.kinds;
        let events: Vec<Event> = diff(&self.seen, &now)
            .into_iter()
            .filter(|e| kinds.matches(&e.kind))
            .collect();
        self.seen = now;
        let changed = !events.is_empty();
        self.pending.extend(events);
        Ok(changed)
    }
}

impl<'w, C, D, K> Perform<Opening<Watch<'w, PollWatcher<C, D, K>>>> for Polling<C, D, K>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>> + Clone,
    D: Perform<NextEntry>,
    K: Compare<C>,
{
    fn perform(
        &mut self,
        Opening(op, _): Opening<Watch<'w, PollWatcher<C, D, K>>>,
    ) -> io::Result<PollWatcher<C, D, K>> {
        let mut fs = self.fs.clone();
        let seen = scan::<C, D, K>(&mut fs, op.path, op.recursive)?;
        Ok(PollWatcher {
            fs,
            root: op.path.to_path_buf(),
            recursive: op.recursive,
            kinds: op.kinds,
            interval: self.interval,
            seen,
            pending: VecDeque::new(),
            dir: PhantomData,
        })
    }
}

impl<'w, C, D, K> Perform<Closing<Watch<'w, PollWatcher<C, D, K>>>> for Polling<C, D, K> {
    fn perform(&mut self, _: Closing<Watch<'w, PollWatcher<C, D, K>>>) -> io::Result<()> {
        Ok(())
    }
}

impl<C, D, K> Perform<NextEvent> for PollWatcher<C, D, K>
where
    C: for<'a> Provide<ReadDir<'a, D>> + for<'a> Inspect<Metadata<'a>> + Perform<Sleep>,
    D: Perform<NextEntry>,
    K: Compare<C>,
{
    fn perform(&mut self, _: NextEvent) -> io::Result<Event> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            if !self.poll()? {
                self.fs.perform(Sleep(self.interval));
            }
        }
    }
}

/// Rescans until something changes or the clock of `C` passes the
/// deadline.
impl<C, D, K> Perform<Timeout<NextEvent>> for PollWatcher<C, D, K>
where
    C: for<'a> Provide<ReadDir<'a, D>>
        + for<'a> Inspect<Metadata<'a>>
        + Perform<Sleep>
        + Inspect<Now>,
    D: Perform<NextEntry>,
    K: Compare<C>,
{
    fn perform(&mut self, op: Timeout<NextEvent>) -> io::Result<Event> {
        let deadline = self.fs.inspect(Now) + op.after;
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            if self.poll()? {
                continue;
            }
            let now = self.fs.inspect(Now);
            let Some(left) = deadline
                .checked_duration_since(now)
                .filter(|d| !d.is_zero())
            else {
                return op.op.timed_out();
            };
            self.fs.perform(Sleep(left.min(self.interval)));
        }
    }
}

// The lifts let async cores use them too.
impl<C, D, K> seam_core::Capability for Polling<C, D, K> {}
impl<C, D, K> seam_core::Capability for PollWatcher<C, D, K> {}

#[cfg(feature = "notify")]
pub use os::{Notify, NotifyWatcher};

/// notify's configuration for [`Notify`].
#[cfg(feature = "notify")]
pub use notify::Config;

#[cfg(feature = "notify")]
mod os {
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use std::collections::VecDeque;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
    use std::time::Instant;

    use notify::Watcher as _;

    use super::*;

    /// Watches with the operating system's own file events, through
    /// notify's recommended watcher, configured by a [`Config`].
    #[derive(Debug, Default, Clone, Copy)]
    pub struct Notify {
        config: Config,
    }

    impl Notify {
        /// notify's defaults.
        pub fn new() -> Self {
            Self::default()
        }

        /// Watches with `config` (following symbolic links or not, and,
        /// where the platform falls back to polling, how often and
        /// whether by contents).
        pub fn with_config(config: Config) -> Self {
            Self { config }
        }
    }

    /// A watcher from [`Notify`]. Access events (a file opened or closed)
    /// are not reported.
    ///
    /// Waiting for an event blocks in the sync form; the async forms wait
    /// without blocking, under any executor (notify's own thread wakes
    /// them), and a `Timeout` on them is kept by a thread of the
    /// watcher's, started on the first async timeout.
    pub struct NotifyWatcher {
        // Kept alive for the scope: dropping it stops the events.
        _watcher: notify::RecommendedWatcher,
        shared: Arc<Shared>,
        kinds: EventKindMask,
    }

    impl core::fmt::Debug for NotifyWatcher {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("NotifyWatcher").finish_non_exhaustive()
        }
    }

    /// What notify's thread, the timer thread and the waiter share.
    #[derive(Default)]
    struct Shared {
        state: Mutex<State>,
        /// Signalled on each event, a new deadline, and the end.
        changed: Condvar,
    }

    #[derive(Default)]
    struct State {
        events: VecDeque<notify::Result<notify::Event>>,
        /// The async waiter, woken on an event or its deadline.
        waker: Option<Waker>,
        /// An async waiter's deadline, kept by the timer thread.
        deadline: Option<Instant>,
        expired: bool,
        timer: bool,
        closed: bool,
    }

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl Drop for NotifyWatcher {
        fn drop(&mut self) {
            self.shared.lock().closed = true;
            self.shared.changed.notify_all();
        }
    }

    fn error(e: notify::Error) -> io::Error {
        match e.kind {
            notify::ErrorKind::Io(e) => e.into(),
            notify::ErrorKind::PathNotFound => io::ErrorKind::NotFound.into(),
            _ => io::Error::from(std::io::Error::other(e)),
        }
    }

    /// The next reportable event queued, if any.
    fn take(state: &mut State, kinds: EventKindMask) -> Option<io::Result<Event>> {
        while let Some(received) = state.events.pop_front() {
            match received {
                Ok(e) if kinds.matches(&e.kind) => return Some(Ok(e)),
                Ok(_) => {}
                Err(e) => return Some(Err(error(e))),
            }
        }
        None
    }

    impl NotifyWatcher {
        /// The next event, blocking until `deadline` (`None`: no limit).
        fn next(&self, deadline: Option<Instant>) -> io::Result<Event> {
            let mut state = self.shared.lock();
            loop {
                if let Some(e) = take(&mut state, self.kinds) {
                    return e;
                }
                state = match deadline {
                    None => self
                        .shared
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                    Some(at) => {
                        let left = at.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                        self.shared
                            .changed
                            .wait_timeout(state, left)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
            }
        }

        /// Starts the thread that keeps async deadlines, once.
        fn start_timer(&self) -> io::Result<()> {
            let mut state = self.shared.lock();
            if state.timer {
                return Ok(());
            }
            let shared = Arc::clone(&self.shared);
            std::thread::Builder::new()
                .name("seam-notify timer".into())
                .stack_size(64 * 1024)
                .spawn(move || timer(&shared))
                .map_err(io::Error::from)?;
            state.timer = true;
            Ok(())
        }
    }

    /// Wakes the async waiter when its deadline passes, until the watcher
    /// is dropped.
    fn timer(shared: &Shared) {
        let mut state = shared.lock();
        while !state.closed {
            state = match state.deadline {
                Some(at) if Instant::now() >= at => {
                    state.deadline = None;
                    state.expired = true;
                    if let Some(w) = state.waker.take() {
                        w.wake();
                    }
                    continue;
                }
                Some(at) => {
                    let left = at.saturating_duration_since(Instant::now());
                    shared
                        .changed
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }

    /// Waits for the next event without blocking, until an optional
    /// deadline. Dropping it gives up the wait.
    struct NextFuture<'w> {
        shared: &'w Shared,
        kinds: EventKindMask,
        timed: bool,
    }

    impl Future for NextFuture<'_> {
        type Output = io::Result<Event>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let mut state = self.shared.lock();
            if let Some(e) = take(&mut state, self.kinds) {
                state.deadline = None;
                return Poll::Ready(e);
            }
            if self.timed && state.expired {
                return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
            }
            if state.closed {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "the watcher stopped",
                )));
            }
            match &state.waker {
                Some(w) if w.will_wake(cx.waker()) => {}
                _ => state.waker = Some(cx.waker().clone()),
            }
            Poll::Pending
        }
    }

    impl Drop for NextFuture<'_> {
        fn drop(&mut self) {
            let mut state = self.shared.lock();
            state.waker = None;
            if self.timed {
                state.deadline = None;
                state.expired = false;
            }
        }
    }

    impl NotifyWatcher {
        async fn next_async(&self, after: Option<core::time::Duration>) -> io::Result<Event> {
            if let Some(after) = after {
                self.start_timer()?;
                let mut state = self.shared.lock();
                state.expired = false;
                state.deadline = Some(Instant::now().checked_add(after).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "the timeout is too long")
                })?);
                drop(state);
                self.shared.changed.notify_all();
            }
            NextFuture {
                shared: &self.shared,
                kinds: self.kinds,
                timed: after.is_some(),
            }
            .await
        }
    }

    impl<'w> Perform<Opening<Watch<'w, NotifyWatcher>>> for Notify {
        fn perform(
            &mut self,
            Opening(op, _): Opening<Watch<'w, NotifyWatcher>>,
        ) -> io::Result<NotifyWatcher> {
            let shared = Arc::new(Shared::default());
            let sender = Arc::clone(&shared);
            let mut watcher = notify::RecommendedWatcher::new(
                move |event| {
                    let mut state = sender.lock();
                    state.events.push_back(event);
                    if let Some(w) = state.waker.take() {
                        w.wake();
                    }
                    drop(state);
                    sender.changed.notify_all();
                },
                self.config,
            )
            .map_err(error)?;
            let mode = if op.recursive {
                notify::RecursiveMode::Recursive
            } else {
                notify::RecursiveMode::NonRecursive
            };
            watcher.watch(op.path, mode).map_err(error)?;
            Ok(NotifyWatcher {
                _watcher: watcher,
                shared,
                kinds: op.kinds,
            })
        }
    }

    impl<'w> Perform<Closing<Watch<'w, NotifyWatcher>>> for Notify {
        fn perform(&mut self, _: Closing<Watch<'w, NotifyWatcher>>) -> io::Result<()> {
            Ok(())
        }
    }

    impl Perform<NextEvent> for NotifyWatcher {
        fn perform(&mut self, _: NextEvent) -> io::Result<Event> {
            self.next(None)
        }
    }

    impl Perform<Timeout<NextEvent>> for NotifyWatcher {
        fn perform(&mut self, op: Timeout<NextEvent>) -> io::Result<Event> {
            self.next(Instant::now().checked_add(op.after))
        }
    }

    macro_rules! async_forms {
        ($($method:ident: $trait:ident;)*) => {$(
            impl seam_core::future::$trait<NextEvent> for NotifyWatcher {
                async fn $method(&mut self, _: NextEvent) -> io::Result<Event> {
                    self.next_async(None).await
                }
            }

            impl seam_core::future::$trait<Timeout<NextEvent>> for NotifyWatcher {
                async fn $method(&mut self, op: Timeout<NextEvent>) -> io::Result<Event> {
                    self.next_async(Some(op.after)).await
                }
            }
        )*};
    }

    async_forms! {
        perform_async: AsyncPerform;
        perform_send: SendPerform;
    }

    // Opening a watch never waits: its async forms are the lifted sync
    // ones. The watcher's are its own, above.
    impl seam_core::Capability for Notify {}
}
