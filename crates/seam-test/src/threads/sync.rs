//! Channels and shared cells for simulated threads.
//!
//! [`RecvMsg`] blocks the thread until a message arrives, and [`SendMsg`]
//! until a bounded channel has room; each is woken exactly when what it
//! waits for arrives, or when the channel closes. A receiver waiting on a
//! channel whose senders are all alive but blocked counts toward deadlock
//! detection, like any other wait.
//!
//! Every operation on these, and on [`Shared`], is a scheduling point, so
//! sweeping seeds explores how threads interleave around them.

#![allow(clippy::disallowed_types)] // the simulated shared state itself

use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Update};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::vec::Vec;

use seam_core::{Inspect, Perform};

use super::{Shared as Scheduler, ThreadSim, lock};
use crate::sync as sim;

/// The sending end of a channel between simulated threads. Clone it to
/// send from several threads.
pub struct Sender<T>(sim::Sender<T>, Arc<Scheduler>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), Arc::clone(&self.1))
    }
}

/// The receiving end of a channel between simulated threads.
pub struct Receiver<T>(sim::Receiver<T>, Arc<Scheduler>);

/// A value shared between simulated threads. Clone it to share the same
/// value.
///
/// It behaves like the `Mutex` it stands in for: an [`Update`] holds the
/// value for as long as its closure runs, and other threads touching the
/// value meanwhile block until it is done, even if the closure performs
/// effects of its own (and so lets them run).
pub struct Shared<T>(Arc<Cell<T>>, Arc<Scheduler>);

struct Cell<T> {
    value: Mutex<T>,
    /// Whether a thread holds the value, and who waits for it.
    held: Mutex<(bool, Vec<Waker>)>,
}

impl<T> Shared<T> {
    /// Waits for the value, then runs `f` on it.
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.1.switch();
        self.1.wait_until(|waker| {
            let mut held = lock(&self.0.held);
            if held.0 {
                held.1.push(waker.clone());
                None
            } else {
                held.0 = true;
                Some(())
            }
        });
        // Released even if `f` panics or the run halts inside it.
        let _release = Release(&self.0);
        f(&mut lock(&self.0.value))
    }
}

struct Release<'a, T>(&'a Cell<T>);

impl<T> Drop for Release<'_, T> {
    fn drop(&mut self) {
        let waiters = {
            let mut held = lock(&self.0.held);
            held.0 = false;
            core::mem::take(&mut held.1)
        };
        for w in waiters {
            w.wake();
        }
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), Arc::clone(&self.1))
    }
}

macro_rules! debug_as {
    ($($ty:ident),*) => {$(
        impl<T> core::fmt::Debug for $ty<T> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(stringify!($ty))
            }
        }
    )*};
}

debug_as!(Sender, Receiver, Shared);

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for ThreadSim {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        let (tx, rx) = sim::channel(op.capacity);
        (
            Sender(tx, Arc::clone(&self.shared)),
            Receiver(rx, Arc::clone(&self.shared)),
        )
    }
}

/// Blocks until there is room, or the receiver is gone.
impl<T> Perform<SendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: SendMsg<T>) -> Result<(), SendError<T>> {
        self.1.switch();
        let mut value = Some(op.0);
        self.1.wait_until(|waker| {
            let v = value.take().expect("a send stops waiting once it succeeds");
            match self.0.poll_send(v, waker) {
                Err(SendError::Full(v)) => {
                    value = Some(v);
                    None
                }
                done => Some(done),
            }
        })
    }
}

/// Blocks until a message arrives, or every sender is gone.
impl<T> Perform<RecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: RecvMsg<T>) -> Result<T, RecvError> {
        self.1.switch();
        self.1.wait_until(|waker| match self.0.poll_recv(waker) {
            Err(RecvError::Empty) => None,
            done => Some(done),
        })
    }
}

impl<T> Perform<Share<T, Shared<T>>> for ThreadSim {
    fn perform(&mut self, op: Share<T, Shared<T>>) -> Shared<T> {
        let cell = Cell {
            value: Mutex::new(op.value),
            held: Mutex::new((false, Vec::new())),
        };
        Shared(Arc::new(cell), Arc::clone(&self.shared))
    }
}

impl<T: Clone> Inspect<Load<T>> for Shared<T> {
    fn inspect(&self, _: Load<T>) -> T {
        self.with(|v| v.clone())
    }
}

/// The value is held for the whole update: no other thread sees it
/// half-updated.
impl<'a, T, R> Perform<Update<'a, T, R>> for Shared<T> {
    fn perform(&mut self, op: Update<'a, T, R>) -> R {
        self.with(op.0)
    }
}

#[cfg(feature = "time")]
mod timeouts {
    use seam_core::ops::time::{Timeout, TimesOut};

    use super::*;

    /// Blocks while the channel is full, for at most the timeout on the
    /// virtual clock; a send that times out hands its value back.
    impl<T> Perform<Timeout<SendMsg<T>>> for Sender<T> {
        fn perform(&mut self, op: Timeout<SendMsg<T>>) -> Result<(), SendError<T>> {
            self.1.switch();
            let mut value = Some(op.op.0);
            let sent = self.1.wait_within(op.after, |waker| {
                let v = value.take().expect("a send stops waiting once it succeeds");
                match self.0.poll_send(v, waker) {
                    Err(SendError::Full(v)) => {
                        value = Some(v);
                        None
                    }
                    done => Some(done),
                }
            });
            sent.unwrap_or_else(|| Err(SendError::Full(value.take().expect("still held"))))
        }
    }

    /// Blocks until a message arrives, for at most the timeout on the
    /// virtual clock.
    impl<T> Perform<Timeout<RecvMsg<T>>> for Receiver<T> {
        fn perform(&mut self, op: Timeout<RecvMsg<T>>) -> Result<T, RecvError> {
            self.1.switch();
            self.1
                .wait_within(op.after, |waker| match self.0.poll_recv(waker) {
                    Err(RecvError::Empty) => None,
                    done => Some(done),
                })
                .unwrap_or_else(|| op.op.timed_out())
        }
    }
}

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for Receiver<T> {}
impl<T> seam_core::Capability for Sender<T> {}
impl<T> seam_core::Capability for Shared<T> {}
