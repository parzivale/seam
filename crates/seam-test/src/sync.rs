//! Channels and shared cells in the simulated world.
//!
//! Used directly on a [`Sim`], nothing waits: sending to a full bounded
//! channel fails with [`SendError::Full`], and receiving from an empty one
//! with [`RecvError::Empty`]. On the deterministic runtime, the endpoints in
//! `rt::sync` wait instead, woken exactly when a message, room, or a close
//! arrives.
#![allow(clippy::disallowed_types)] // the simulated shared state itself

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;
use std::vec::Vec;

use seam_core::ops::sync::{
    Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Store, TryRecvMsg, TrySendMsg,
    Update,
};
use seam_core::{Inspect, Perform};

use crate::Sim;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A channel's shared state.
pub(crate) struct Chan<T> {
    queue: VecDeque<T>,
    capacity: Option<usize>,
    senders: usize,
    receiver: bool,
    /// The runtime's tasks waiting for a message or a close.
    recv_waiter: Option<Waker>,
    /// The runtime's tasks waiting for room or a close.
    send_waiters: Vec<Waker>,
}

type Link<T> = Arc<Mutex<Chan<T>>>;

impl<T> Chan<T> {
    fn wake_receiver(&mut self) {
        if let Some(w) = self.recv_waiter.take() {
            w.wake();
        }
    }

    fn wake_senders(&mut self) {
        for w in self.send_waiters.drain(..) {
            w.wake();
        }
    }

    fn try_send(&mut self, value: T) -> Result<(), SendError<T>> {
        if !self.receiver {
            return Err(SendError::Closed(value));
        }
        if self.capacity.is_some_and(|c| self.queue.len() >= c) {
            return Err(SendError::Full(value));
        }
        self.queue.push_back(value);
        self.wake_receiver();
        Ok(())
    }

    fn try_recv(&mut self) -> Result<T, RecvError> {
        match self.queue.pop_front() {
            Some(value) => {
                self.wake_senders();
                Ok(value)
            }
            None if self.senders == 0 => Err(RecvError::Closed),
            None => Err(RecvError::Empty),
        }
    }
}

/// The sending end of a simulated channel. Clone it to send from several
/// places.
pub struct Sender<T>(pub(crate) Link<T>);

/// The receiving end of a simulated channel.
pub struct Receiver<T>(pub(crate) Link<T>);

impl<T> Sender<T> {
    /// Sends, or registers `waker` to be woken when there is room or the
    /// channel closes.
    #[cfg_attr(not(feature = "rt"), allow(dead_code))]
    pub(crate) fn poll_send(&self, value: T, waker: &Waker) -> Result<(), SendError<T>> {
        let mut chan = lock(&self.0);
        let result = chan.try_send(value);
        if let Err(SendError::Full(_)) = &result {
            if !chan.send_waiters.iter().any(|w| w.will_wake(waker)) {
                chan.send_waiters.push(waker.clone());
            }
        }
        result
    }
}

impl<T> Receiver<T> {
    /// Receives, or registers `waker` to be woken when a message arrives
    /// or the channel closes.
    #[cfg_attr(not(feature = "rt"), allow(dead_code))]
    pub(crate) fn poll_recv(&self, waker: &Waker) -> Result<T, RecvError> {
        let mut chan = lock(&self.0);
        let result = chan.try_recv();
        if let Err(RecvError::Empty) = result {
            chan.recv_waiter = Some(waker.clone());
        }
        result
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        lock(&self.0).senders += 1;
        Self(Arc::clone(&self.0))
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut chan = lock(&self.0);
        chan.senders -= 1;
        if chan.senders == 0 {
            chan.wake_receiver();
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut chan = lock(&self.0);
        chan.receiver = false;
        chan.queue.clear();
        chan.wake_senders();
    }
}

impl<T> core::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Sender")
    }
}

impl<T> core::fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Receiver")
    }
}

/// Creates a simulated channel.
pub(crate) fn channel<T>(capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
    let chan = Chan {
        queue: VecDeque::new(),
        capacity,
        senders: 1,
        receiver: true,
        recv_waiter: None,
        send_waiters: Vec::new(),
    };
    let link = Arc::new(Mutex::new(chan));
    (Sender(Arc::clone(&link)), Receiver(link))
}

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Sim {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        channel(op.capacity)
    }
}

/// Fails with `Full` instead of waiting.
impl<T> Perform<SendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: SendMsg<T>) -> Result<(), SendError<T>> {
        lock(&self.0).try_send(op.0)
    }
}

impl<T> Perform<TrySendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: TrySendMsg<T>) -> Result<(), SendError<T>> {
        lock(&self.0).try_send(op.0)
    }
}

/// Fails with `Empty` instead of waiting.
impl<T> Perform<RecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: RecvMsg<T>) -> Result<T, RecvError> {
        lock(&self.0).try_recv()
    }
}

impl<T> Perform<TryRecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: TryRecvMsg<T>) -> Result<T, RecvError> {
        lock(&self.0).try_recv()
    }
}

/// A value shared between the parts of a simulated program. Clone it to
/// share the same value. Nothing about it waits, so it is the same type on
/// `Sim` and on the runtime.
pub struct Shared<T>(Arc<Mutex<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> core::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Shared")
    }
}

impl<T> Perform<Share<T, Shared<T>>> for Sim {
    fn perform(&mut self, op: Share<T, Shared<T>>) -> Shared<T> {
        Shared(Arc::new(Mutex::new(op.value)))
    }
}

impl<T: Clone> Inspect<Load<T>> for Shared<T> {
    fn inspect(&self, _: Load<T>) -> T {
        lock(&self.0).clone()
    }
}

impl<T> Perform<Store<T>> for Shared<T> {
    fn perform(&mut self, op: Store<T>) {
        *lock(&self.0) = op.0;
    }
}

impl<'a, T, R> Perform<Update<'a, T, R>> for Shared<T> {
    fn perform(&mut self, op: Update<'a, T, R>) -> R {
        (op.0)(&mut lock(&self.0))
    }
}

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for Receiver<T> {}
impl<T> seam_core::Capability for Sender<T> {}
impl<T> seam_core::Capability for Shared<T> {}
