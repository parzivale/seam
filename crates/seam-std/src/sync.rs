//! Channels and shared cells over std: `std::sync::mpsc` and a mutex.

use std::sync::mpsc::{self, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use seam_core::ops::sync::{
    Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Store, TryRecvMsg, TrySendMsg,
    Update,
};
use seam_core::{Inspect, Perform};

use crate::Std;

/// The sending end of a channel. Clone it to send from several threads.
///
/// [`SendMsg`] blocks the thread while a bounded channel is full.
#[derive(Debug)]
pub struct Sender<T>(Tx<T>);

#[derive(Debug)]
enum Tx<T> {
    Unbounded(mpsc::Sender<T>),
    Bounded(mpsc::SyncSender<T>),
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(match &self.0 {
            Tx::Unbounded(tx) => Tx::Unbounded(tx.clone()),
            Tx::Bounded(tx) => Tx::Bounded(tx.clone()),
        })
    }
}

/// The receiving end of a channel. [`RecvMsg`] blocks the thread until a
/// message arrives or every sender is gone.
#[derive(Debug)]
pub struct Receiver<T>(mpsc::Receiver<T>);

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for Std {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        match op.capacity {
            None => {
                let (tx, rx) = mpsc::channel();
                (Sender(Tx::Unbounded(tx)), Receiver(rx))
            }
            Some(n) => {
                let (tx, rx) = mpsc::sync_channel(n);
                (Sender(Tx::Bounded(tx)), Receiver(rx))
            }
        }
    }
}

impl<T> Perform<SendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: SendMsg<T>) -> Result<(), SendError<T>> {
        let sent = match &self.0 {
            Tx::Unbounded(tx) => tx.send(op.0),
            Tx::Bounded(tx) => tx.send(op.0),
        };
        sent.map_err(|e| SendError::Closed(e.0))
    }
}

impl<T> Perform<TrySendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: TrySendMsg<T>) -> Result<(), SendError<T>> {
        match &self.0 {
            Tx::Unbounded(tx) => tx.send(op.0).map_err(|e| SendError::Closed(e.0)),
            Tx::Bounded(tx) => tx.try_send(op.0).map_err(|e| match e {
                TrySendError::Full(t) => SendError::Full(t),
                TrySendError::Disconnected(t) => SendError::Closed(t),
            }),
        }
    }
}

impl<T> Perform<RecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: RecvMsg<T>) -> Result<T, RecvError> {
        self.0.recv().map_err(|_| RecvError::Closed)
    }
}

impl<T> Perform<TryRecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, _: TryRecvMsg<T>) -> Result<T, RecvError> {
        self.0.try_recv().map_err(|e| match e {
            TryRecvError::Empty => RecvError::Empty,
            TryRecvError::Disconnected => RecvError::Closed,
        })
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
