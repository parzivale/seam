//! Channels and shared cells over std: a queue and a mutex, with condvars
//! to block on.
//!
//! The channel is its own rather than `std::sync::mpsc`, because `mpsc`
//! cannot time out a send to a full channel.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
#[cfg(feature = "time")]
use std::time::{Duration, Instant};

use seam_core::ops::sync::{
    Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Store, TryRecvMsg, TrySendMsg,
    Update,
};
#[cfg(feature = "time")]
use seam_core::ops::time::Timeout;
use seam_core::{Inspect, Perform};

use crate::Std;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Queue<T> {
    items: VecDeque<T>,
    capacity: Option<usize>,
    senders: usize,
    receiver: bool,
}

struct Chan<T> {
    queue: Mutex<Queue<T>>,
    /// Signalled when a message arrives or the last sender goes.
    readable: Condvar,
    /// Signalled when room appears or the receiver goes.
    writable: Condvar,
}

/// How long a blocking operation may wait.
#[derive(Clone, Copy)]
enum Wait {
    Forever,
    #[cfg(feature = "time")]
    Until(Instant),
}

impl<T> Chan<T> {
    /// Waits on `cv` while `blocked`, until `wait` runs out. Returns the
    /// guard, and whether it is still blocked.
    fn wait_while<'a>(
        &self,
        mut q: MutexGuard<'a, Queue<T>>,
        cv: &Condvar,
        wait: Wait,
        blocked: impl Fn(&Queue<T>) -> bool,
    ) -> (MutexGuard<'a, Queue<T>>, bool) {
        while blocked(&q) {
            match wait {
                Wait::Forever => q = cv.wait(q).unwrap_or_else(PoisonError::into_inner),
                #[cfg(feature = "time")]
                Wait::Until(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return (q, true);
                    }
                    q = cv
                        .wait_timeout(q, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
        (q, false)
    }

    fn send(&self, value: T, wait: Wait) -> Result<(), SendError<T>> {
        let q = lock(&self.queue);
        let (mut q, full) = self.wait_while(q, &self.writable, wait, |q| {
            q.receiver && q.capacity.is_some_and(|c| q.items.len() >= c)
        });
        if !q.receiver {
            return Err(SendError::Closed(value));
        }
        if full {
            return Err(SendError::Full(value));
        }
        q.items.push_back(value);
        self.readable.notify_one();
        Ok(())
    }

    fn recv(&self, wait: Wait) -> Result<T, RecvError> {
        let q = lock(&self.queue);
        let (mut q, _) = self.wait_while(q, &self.readable, wait, |q| {
            q.items.is_empty() && q.senders > 0
        });
        match q.items.pop_front() {
            Some(value) => {
                self.writable.notify_one();
                Ok(value)
            }
            None if q.senders == 0 => Err(RecvError::Closed),
            None => Err(RecvError::Empty),
        }
    }

    fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        let mut q = lock(&self.queue);
        if !q.receiver {
            return Err(SendError::Closed(value));
        }
        if q.capacity.is_some_and(|c| q.items.len() >= c) {
            return Err(SendError::Full(value));
        }
        q.items.push_back(value);
        self.readable.notify_one();
        Ok(())
    }

    fn try_recv(&self) -> Result<T, RecvError> {
        let mut q = lock(&self.queue);
        match q.items.pop_front() {
            Some(value) => {
                self.writable.notify_one();
                Ok(value)
            }
            None if q.senders == 0 => Err(RecvError::Closed),
            None => Err(RecvError::Empty),
        }
    }
}

/// The sending end of a channel. Clone it to send from several threads.
///
/// [`SendMsg`] blocks the thread while a bounded channel is full.
pub struct Sender<T>(Arc<Chan<T>>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        lock(&self.0.queue).senders += 1;
        Self(Arc::clone(&self.0))
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut q = lock(&self.0.queue);
        q.senders -= 1;
        if q.senders == 0 {
            self.0.readable.notify_all();
        }
    }
}

/// The receiving end of a channel. [`RecvMsg`] blocks the thread until a
/// message arrives or every sender is gone.
pub struct Receiver<T>(Arc<Chan<T>>);

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut q = lock(&self.0.queue);
        q.receiver = false;
        q.items.clear();
        self.0.writable.notify_all();
    }
}

impl<T> std::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sender")
    }
}

impl<T> std::fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Receiver")
    }
}

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Std {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        let chan = Arc::new(Chan {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                capacity: op.capacity,
                senders: 1,
                receiver: true,
            }),
            readable: Condvar::new(),
            writable: Condvar::new(),
        });
        (Sender(Arc::clone(&chan)), Receiver(chan))
    }
}

impl<T> Perform<SendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: SendMsg<T>) -> Result<(), SendError<T>> {
        self.0.send(op.0, Wait::Forever)
    }
}

impl<T> Perform<TrySendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: TrySendMsg<T>) -> Result<(), SendError<T>> {
        self.0.try_send(op.0)
    }
}

impl<T> Perform<RecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: RecvMsg<T>) -> Result<T, RecvError> {
        self.0.recv(Wait::Forever)
    }
}

impl<T> Perform<TryRecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: TryRecvMsg<T>) -> Result<T, RecvError> {
        self.0.try_recv()
    }
}

#[cfg(feature = "time")]
fn deadline(after: Duration) -> Wait {
    // A deadline too far to represent is no deadline.
    Instant::now()
        .checked_add(after)
        .map_or(Wait::Forever, Wait::Until)
}

/// Blocks while the channel is full, for at most the timeout.
#[cfg(feature = "time")]
impl<T> Perform<Timeout<SendMsg<T>>> for Sender<T> {
    fn perform(&mut self, op: Timeout<SendMsg<T>>) -> Result<(), SendError<T>> {
        self.0.send(op.op.0, deadline(op.after))
    }
}

/// Blocks until a message arrives, for at most the timeout.
#[cfg(feature = "time")]
impl<T> Perform<Timeout<RecvMsg<T>>> for Receiver<T> {
    fn perform(&mut self, op: Timeout<RecvMsg<T>>) -> Result<T, RecvError> {
        self.0.recv(deadline(op.after))
    }
}

/// A value shared between threads. Clone it to give another thread access
/// to the same value.
///
/// A thread that panics inside an [`Update`] leaves the value as the
/// closure left it; the cell stays usable.
#[derive(Debug, Default)]
pub struct Shared<T>(Arc<Mutex<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T> Perform<Share<T, Shared<T>>> for Std {
    fn perform(&mut self, op: Share<T, Shared<T>>) -> Shared<T> {
        Shared(Arc::new(Mutex::new(op.value)))
    }
}

impl<T: Clone> Inspect<Load<T>> for Shared<T> {
    fn inspect(&self, _: Load<T>) -> T {
        self.lock().clone()
    }
}

impl<T> Perform<Store<T>> for Shared<T> {
    fn perform(&mut self, op: Store<T>) {
        *self.lock() = op.0;
    }
}

impl<'a, T, R> Perform<Update<'a, T, R>> for Shared<T> {
    fn perform(&mut self, op: Update<'a, T, R>) -> R {
        (op.0)(&mut self.lock())
    }
}

// Every capability type opts in to seam's lifts.
impl<T> seam_core::Capability for Receiver<T> {}
impl<T> seam_core::Capability for Sender<T> {}
impl<T> seam_core::Capability for Shared<T> {}
