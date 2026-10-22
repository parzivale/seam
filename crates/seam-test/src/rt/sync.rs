//! Channels on the deterministic runtime, whose sends and receives wait.
//!
//! [`RecvMsg`] waits for a message, and [`SendMsg`] waits for room in a
//! bounded channel; each is woken exactly when what it waits for arrives,
//! or when the channel closes. A receiver waiting on a channel whose
//! senders are all alive but idle counts toward deadlock detection, like
//! any other wait.
//!
//! Both are atomic under cancellation: a receive cancelled while waiting
//! takes no message, since it only takes one in the same poll that returns
//! it, and a send cancelled while waiting for room enqueues nothing.
//!
//! Shared cells never wait, so the runtime uses `seam_test::sync::Shared`
//! as it is.

use core::future::poll_fn;
use core::task::Poll;

use seam_core::Perform;
use seam_core::ops::sync::{
    Channel, RecvError, RecvMsg, SendError, SendMsg, Share, TryRecvMsg, TrySendMsg,
};

use super::{AsyncSim, async_op};
use crate::sync as sim;

/// The sending end of a channel on the runtime. Clone it to send from
/// several tasks.
#[derive(Debug)]
pub struct Sender<T>(sim::Sender<T>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// The receiving end of a channel on the runtime.
#[derive(Debug)]
pub struct Receiver<T>(sim::Receiver<T>);

impl<T> Perform<Channel<Sender<T>, Receiver<T>>> for AsyncSim {
    fn perform(&mut self, op: Channel<Sender<T>, Receiver<T>>) -> (Sender<T>, Receiver<T>) {
        let (tx, rx) = sim::channel(op.capacity);
        (Sender(tx), Receiver(rx))
    }
}

impl<T> Perform<Share<T, sim::Shared<T>>> for AsyncSim {
    fn perform(&mut self, op: Share<T, sim::Shared<T>>) -> sim::Shared<T> {
        self.shared.sim().perform(op)
    }
}

async_op!([T: Send] Sender<T>, SendMsg<T>, |s, op| {
    let mut value = Some(op.0);
    poll_fn(|cx| {
        let v = value.take().expect("a send is only polled until it finishes");
        match s.0.poll_send(v, cx.waker()) {
            Err(SendError::Full(v)) => {
                value = Some(v);
                Poll::Pending
            }
            done => Poll::Ready(done),
        }
    })
    .await
});

impl<T> Perform<TrySendMsg<T>> for Sender<T> {
    fn perform(&mut self, op: TrySendMsg<T>) -> Result<(), SendError<T>> {
        self.0.perform(op)
    }
}

async_op!([T: Send] Receiver<T>, RecvMsg<T>, |s, _op| {
    poll_fn(|cx| match s.0.poll_recv(cx.waker()) {
        Err(RecvError::Empty) => Poll::Pending,
        done => Poll::Ready(done),
    })
    .await
});

impl<T> Perform<TryRecvMsg<T>> for Receiver<T> {
    fn perform(&mut self, op: TryRecvMsg<T>) -> Result<T, RecvError> {
        self.0.perform(op)
    }
}
