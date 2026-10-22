//! State shared between the [`Sim`](crate::Sim) and the handles it hands
//! out. A handle (an open file, a socket) outlives the borrow of the `Sim`
//! that created it, so the two share the state behind a mutex.
//!
//! This is the one place the crate uses shared state. It is the simulated
//! world itself, not an effect the core can see: every access is a pure
//! in-memory update, and the lock is never contended unless the test itself
//! uses threads.
#![allow(clippy::disallowed_types)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Everything handles need to reach.
#[derive(Debug)]
pub(crate) struct World {
    /// The wall clock. Lives here so that writes through file handles can
    /// stamp modification times.
    #[cfg(any(feature = "fs", feature = "time"))]
    pub(crate) wall: std::time::SystemTime,
    #[cfg(feature = "fs")]
    pub(crate) fs: crate::fs::Fs,
    #[cfg(feature = "net")]
    pub(crate) net: crate::net::Net,
}

#[allow(clippy::derivable_impls)] // derivable only when `wall` is compiled out
impl Default for World {
    fn default() -> Self {
        Self {
            #[cfg(any(feature = "fs", feature = "time"))]
            wall: std::time::UNIX_EPOCH,
            #[cfg(feature = "fs")]
            fs: crate::fs::Fs::default(),
            #[cfg(feature = "net")]
            net: crate::net::Net::default(),
        }
    }
}

/// A handle on the shared world.
#[derive(Debug, Clone, Default)]
pub(crate) struct Shared(Arc<Mutex<World>>);

impl Shared {
    /// Locks the world. A panic in another holder (a failed assertion in a
    /// test thread) does not poison it for the rest of the test.
    pub(crate) fn lock(&self) -> MutexGuard<'_, World> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
