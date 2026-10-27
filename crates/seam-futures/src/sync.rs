//! Channels and shared cells from futures-rs: `futures::channel::mpsc` and
//! `futures::lock::Mutex`.
//!
//! Neither needs a runtime or a clock, so they work under any executor,
//! including the simulated runtime. Every operation is atomic under
//! cancellation: a `RecvMsg` cancelled while waiting takes no message, and
//! a `SendMsg` cancelled while waiting for room sends nothing.
//!
//! A bounded channel holds `capacity` messages, plus one more for every
//! clone of the sender, as futures-rs's channels do.
//!
//! # Timeouts
//!
//! With `time`, `try_send` and `try_recv` (a timeout of zero) work on these
//! endpoints. futures-rs has no clock, so a timeout that would wait cannot
//! be kept: race the operation against a clock with
//! [`within`](crate::time::within) instead. A nonzero `Timeout` panics.

use std::sync::Arc;

use futures::channel::mpsc;
use futures::lock::Mutex;
use seam_core::Perform;
use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Update};

use crate::Futures;

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

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Futures {
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
    async fn send(&mut self, mut message: T) -> Result<(), SendError<T>> {
        match &mut self.0 {
            Tx::Unbounded(tx) => tx
                .unbounded_send(message)
                .map_err(|e| SendError::Closed(e.into_inner())),
            Tx::Bounded(tx) => loop {
                // Waiting for room takes nothing, so a cancelled send
                // sends nothing.
                if core::future::poll_fn(|cx| tx.poll_ready(cx)).await.is_err() {
                    return Err(SendError::Closed(message));
                }
                match tx.try_send(message) {
                    Ok(()) => return Ok(()),
                    // Another clone took the room first.
                    Err(e) if e.is_full() => message = e.into_inner(),
                    Err(e) => return Err(SendError::Closed(e.into_inner())),
                }
            },
        }
    }

    #[cfg_attr(not(feature = "time"), allow(dead_code))]
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
}

impl<T> Receiver<T> {
    async fn recv(&mut self) -> Result<T, RecvError> {
        let received = match &mut self.0 {
            Rx::Unbounded(rx) => futures::StreamExt::next(rx).await,
            Rx::Bounded(rx) => futures::StreamExt::next(rx).await,
        };
        received.ok_or(RecvError::Closed)
    }

    #[cfg_attr(not(feature = "time"), allow(dead_code))]
    fn try_recv(&mut self) -> Result<T, RecvError> {
        let received = match &mut self.0 {
            Rx::Unbounded(rx) => rx.try_recv(),
            Rx::Bounded(rx) => rx.try_recv(),
        };
        received.map_err(|e| match e {
            mpsc::TryRecvError::Closed => RecvError::Closed,
            mpsc::TryRecvError::Empty => RecvError::Empty,
        })
    }
}

async_op!(perform [T: Send] Sender<T>, SendMsg<T>, |s, op| s.send(op.0).await);
async_op!(perform [T: Send] Receiver<T>, RecvMsg<T>, |s, _op| s.recv().await);

#[cfg(feature = "time")]
mod timeouts {
    use core::time::Duration;

    use seam_core::ops::time::Timeout;

    use super::*;

    fn at_once(after: Duration) {
        assert!(
            after.is_zero(),
            "seam-futures: futures-rs has no clock, so its channels only take a timeout of \
             zero; race the operation against a clock with `seam_futures::time::within`"
        );
    }

    /// A timeout of zero only: `try_send`.
    impl<T> Perform<Timeout<SendMsg<T>>> for Sender<T> {
        fn perform(&mut self, op: Timeout<SendMsg<T>>) -> Result<(), SendError<T>> {
            at_once(op.after);
            self.try_send(op.op.0)
        }
    }

    /// A timeout of zero only: `try_recv`.
    impl<T> Perform<Timeout<RecvMsg<T>>> for Receiver<T> {
        fn perform(&mut self, op: Timeout<RecvMsg<T>>) -> Result<T, RecvError> {
            at_once(op.after);
            self.try_recv()
        }
    }
}

/// A value shared between tasks, behind futures-rs's async mutex. Clone it
/// to give another task access to the same value.
#[derive(Debug, Default)]
pub struct Shared<T>(Arc<Mutex<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Perform<Share<T, Shared<T>>> for Futures {
    fn perform(&mut self, op: Share<T, Shared<T>>) -> Shared<T> {
        Shared(Arc::new(Mutex::new(op.value)))
    }
}

async_op!(inspect [T: Clone + Send] Shared<T>, Load<T>, |s, _op| s.0.lock().await.clone());

// The closure runs under the lock, which is held only while it runs: a
// cancelled update either has not taken the lock, or has finished.
async_op!(perform ['a, T: Send, R] Shared<T>, Update<'a, T, R>, |s, op| {
    let mut value = s.0.lock().await;
    (op.0)(&mut value)
});

// Every capability type opts in to seam's lifts. The channel endpoints'
// sends and receives are async only, and so are the cells.
impl<T> seam_core::Capability for Receiver<T> {}
impl<T> seam_core::Capability for Sender<T> {}
