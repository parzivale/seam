//! The simulated world's state. With std, every [`Sim`](crate::Sim) is a
//! handle on one world behind a mutex: clones of a `Sim`, and the handles it
//! hands out (an open file, a socket), outlive any one borrow and see the
//! same files, clock and output. Without std, the `Sim` holds the world
//! itself.
//!
//! This is the one place the crate uses shared state. It is the simulated
//! world itself, not an effect the core can see: every access is a pure
//! in-memory update, and the lock is never contended unless the test itself
//! uses threads.
#![allow(clippy::disallowed_types)]

/// Everything in the world.
#[derive(Debug)]
pub(crate) struct World {
    /// The monotonic clock, since `Instant::EPOCH`.
    #[cfg(feature = "time")]
    pub(crate) clock: core::time::Duration,
    /// The wall clock. Lives here so that writes through file handles can
    /// stamp modification times.
    #[cfg(all(feature = "std", any(feature = "fs", feature = "time")))]
    pub(crate) wall: std::time::SystemTime,
    #[cfg(feature = "random")]
    pub(crate) rng: crate::random::SplitMix64,
    #[cfg(feature = "env")]
    pub(crate) env: crate::env::Env,
    /// Relative paths resolve against it.
    #[cfg(any(feature = "fs", feature = "env"))]
    pub(crate) cwd: std::path::PathBuf,
    #[cfg(feature = "fs")]
    pub(crate) fs: crate::fs::Fs,
    #[cfg(feature = "net")]
    pub(crate) net: crate::net::Net,
    #[cfg(feature = "io-std")]
    pub(crate) stdio: crate::stdio::Stdio,
    #[cfg(feature = "process")]
    pub(crate) process: crate::process::Processes,
    /// Ctrl-Cs raised and not yet waited for.
    #[cfg(feature = "signal")]
    pub(crate) ctrl_c: u32,
    #[cfg(feature = "rt")]
    pub(crate) parallelism: core::num::NonZeroUsize,
}

impl World {
    /// A fresh world whose random numbers come from `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        #[cfg(not(feature = "random"))]
        let _ = seed;
        Self {
            #[cfg(feature = "time")]
            clock: core::time::Duration::ZERO,
            #[cfg(all(feature = "std", any(feature = "fs", feature = "time")))]
            wall: std::time::UNIX_EPOCH,
            #[cfg(feature = "random")]
            rng: crate::random::SplitMix64(seed),
            #[cfg(feature = "env")]
            env: crate::env::Env::default(),
            #[cfg(any(feature = "fs", feature = "env"))]
            cwd: std::path::PathBuf::from("/"),
            #[cfg(feature = "fs")]
            fs: crate::fs::Fs::default(),
            #[cfg(feature = "net")]
            net: crate::net::Net::default(),
            #[cfg(feature = "io-std")]
            stdio: crate::stdio::Stdio::default(),
            #[cfg(feature = "process")]
            process: crate::process::Processes::default(),
            #[cfg(feature = "signal")]
            ctrl_c: 0,
            #[cfg(feature = "rt")]
            parallelism: core::num::NonZeroUsize::MIN,
        }
    }
}

/// A handle on the shared world.
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub(crate) struct Shared(std::sync::Arc<std::sync::Mutex<World>>);

#[cfg(feature = "std")]
impl Shared {
    pub(crate) fn new(world: World) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(world)))
    }

    /// Locks the world. A panic in another holder (a failed assertion in a
    /// test thread) does not poison it for the rest of the test.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, World> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
