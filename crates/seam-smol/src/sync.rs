//! Channels and shared cells.
//!
//! Channels are futures-rs's `mpsc`, which can reserve room before
//! sending, so a send that times out hands its message back. Every
//! operation is atomic under cancellation: a `RecvMsg` cancelled while
//! waiting takes no message, and a `SendMsg` cancelled while waiting for
//! room sends nothing. A bounded channel holds `capacity` messages, plus
//! one more for every clone of the sender, as futures-rs's channels do.
//! Cells never wait: `Update` runs its closure under a short lock.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::channel::mpsc;
use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Update};
use seam_core::{Inspect, Perform};

use crate::Smol;

/// The sending end of a channel. Clone it to send from several tasks.
///
/// [`SendMsg`] waits while a bounded channel is full.
#[derive(Debug)]
pub struct Sender<T>(Tx<T>);

#[derive(Debug)]
enum Tx<T> {
    Unbounded(mpsc::UnboundedSender<T>),
    Bounded(mpsc::Sender<T>),
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(match &self.0 {
            Tx::Unbounded(tx) => Tx::Unbounded(tx.clone()),
            Tx::Bounded(tx) => Tx::Bounded(tx.clone()),
        })
    }
}

/// The receiving end of a channel. [`RecvMsg`] waits until a message
/// arrives or every sender is gone.
#[derive(Debug)]
pub struct Receiver<T>(Rx<T>);

#[derive(Debug)]
enum Rx<T> {
    Unbounded(mpsc::UnboundedReceiver<T>),
    Bounded(mpsc::Receiver<T>),
}

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Smol {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        match op.capacity {
            None => {
                let (tx, rx) = mpsc::unbounded();
                (Sender(Tx::Unbounded(tx)), Receiver(Rx::Unbounded(rx)))
            }
            // futures-rs adds a slot per sender to the buffer it is given.
            Some(n) => {
                let (tx, rx) = mpsc::channel(n.saturating_sub(1));
                (Sender(Tx::Bounded(tx)), Receiver(Rx::Bounded(rx)))
            }
        }
    }
}

impl<T> Sender<T> {
    /// Waits for room in a bounded channel, without taking the message.
    /// `false` if the receiver is gone.
    async fn room(&mut self) -> bool {
        match &mut self.0 {
            Tx::Unbounded(tx) => !tx.is_closed(),
            Tx::Bounded(tx) => core::future::poll_fn(|cx| tx.poll_ready(cx)).await.is_ok(),
        }
    }

    fn try_send(&mut self, message: T) -> Result<(), SendError<T>> {
        let sent = match &mut self.0 {
            Tx::Unbounded(tx) => tx.unbounded_send(message),
            Tx::Bounded(tx) => tx.try_send(message),
        };
        sent.map_err(|e| {
            if e.is_full() {
                SendError::Full(e.into_inner())
            } else {
                SendError::Closed(e.into_inner())
            }
        })
    }

    async fn send(&mut self, mut message: T) -> Result<(), SendError<T>> {
        loop {
            if !self.room().await {
                return Err(SendError::Closed(message));
            }
            match self.try_send(message) {
                // Another clone took the room first.
                Err(SendError::Full(m)) => message = m,
                sent => return sent,
            }
        }
    }
}

impl<T> Receiver<T> {
    async fn recv(&mut self) -> Result<T, RecvError> {
        let received = match &mut self.0 {
            Rx::Unbounded(rx) => smol::stream::StreamExt::next(rx).await,
            Rx::Bounded(rx) => smol::stream::StreamExt::next(rx).await,
        };
        received.ok_or(RecvError::Closed)
    }
}

async_op!(perform [T: Send] Sender<T>, SendMsg<T>, |s, op| s.send(op.0).await);
async_op!(perform [T: Send] Receiver<T>, RecvMsg<T>, |s, _op| s.recv().await);

mod timeouts {
    use seam_core::ops::time::{Timeout, TimesOut};

    use super::*;

    // Waits for room without taking the message, so a send that times out
    // keeps it, and hands it back.
    async_op!(perform [T: Send] Sender<T>, Timeout<SendMsg<T>>, |s, op| {
        let mut message = op.op.0;
        loop {
            match crate::within(op.after, s.room()).await {
                Some(true) => {}
                Some(false) => break Err(SendError::Closed(message)),
                None => break SendMsg(message).timed_out(),
            }
            match s.try_send(message) {
                Err(SendError::Full(m)) if !op.after.is_zero() => message = m,
                sent => break sent,
            }
        }
    });

    async_op!(perform [T: Send] Receiver<T>, Timeout<RecvMsg<T>>, |s, op| {
        match crate::within(op.after, s.recv()).await {
            Some(received) => received,
            None => op.op.timed_out(),
        }
    });
}

/// A value shared between tasks. Clone it to give another task access to
/// the same value.
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

impl<T> Perform<Share<T, Shared<T>>> for Smol {
    fn perform(&mut self, op: Share<T, Shared<T>>) -> Shared<T> {
        Shared(Arc::new(Mutex::new(op.value)))
    }
}

impl<T: Clone> Inspect<Load<T>> for Shared<T> {
    fn inspect(&self, _: Load<T>) -> T {
        self.lock().clone()
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
