//! Channels and shared cells on Tokio: `tokio::sync::mpsc`, and a mutex.
//!
//! Every operation is atomic under cancellation. A `RecvMsg` cancelled
//! while waiting takes no message (Tokio's `recv` is cancel-safe), and a
//! `SendMsg` cancelled while waiting for room sends nothing. Cells never
//! wait: `Update` runs its closure under a short lock and cannot be
//! interrupted, so a plain mutex is enough.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use seam_core::ops::sync::{
    Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Store, TryRecvMsg, TrySendMsg,
    Update,
};
use seam_core::{Inspect, Perform};
use tokio::sync::mpsc;

use crate::Tokio;

/// The sending end of a channel. Clone it to send from several tasks.
///
/// [`SendMsg`] waits while a bounded channel is full.
#[derive(Debug)]
pub struct Sender<T>(pub(crate) Tx<T>);

#[derive(Debug)]
pub(crate) enum Tx<T> {
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
pub struct Receiver<T>(pub(crate) Rx<T>);

#[derive(Debug)]
pub(crate) enum Rx<T> {
    Unbounded(mpsc::UnboundedReceiver<T>),
    Bounded(mpsc::Receiver<T>),
}

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Tokio {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        match op.capacity {
            None => {
                let (tx, rx) = mpsc::unbounded_channel();
                (Sender(Tx::Unbounded(tx)), Receiver(Rx::Unbounded(rx)))
            }
            Some(n) => {
                let (tx, rx) = mpsc::channel(n);
                (Sender(Tx::Bounded(tx)), Receiver(Rx::Bounded(rx)))
            }
        }
    }
}

async_op!(perform [T: Send] Sender<T>, SendMsg<T>, |s, op| match &s.0 {
    Tx::Unbounded(tx) => tx.send(op.0).map_err(|e| SendError::Closed(e.0)),
    Tx::Bounded(tx) => tx.send(op.0).await.map_err(|e| SendError::Closed(e.0)),
});

impl<T> Perform<TrySendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: TrySendMsg<T>) -> Result<(), SendError<T>> {
        match &self.0 {
            Tx::Unbounded(tx) => tx.send(op.0).map_err(|e| SendError::Closed(e.0)),
            Tx::Bounded(tx) => tx.try_send(op.0).map_err(|e| match e {
                mpsc::error::TrySendError::Full(t) => SendError::Full(t),
                mpsc::error::TrySendError::Closed(t) => SendError::Closed(t),
            }),
        }
    }
}

async_op!(perform [T: Send] Receiver<T>, RecvMsg<T>, |s, _op| {
    let received = match &mut s.0 {
        Rx::Unbounded(rx) => rx.recv().await,
        Rx::Bounded(rx) => rx.recv().await,
    };
    received.ok_or(RecvError::Closed)
});

impl<T> Perform<TryRecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: TryRecvMsg<T>) -> Result<T, RecvError> {
        let received = match &mut self.0 {
            Rx::Unbounded(rx) => rx.try_recv(),
            Rx::Bounded(rx) => rx.try_recv(),
        };
        received.map_err(|e| match e {
            mpsc::error::TryRecvError::Empty => RecvError::Empty,
            mpsc::error::TryRecvError::Disconnected => RecvError::Closed,
        })
    }
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

impl<T> Perform<Share<T, Shared<T>>> for Tokio {
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
