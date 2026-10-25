//! Shared state: channels, and cells several tasks or threads can update.
//!
//! Shared state is an effect: what one task observes depends on what others
//! did. So a core does not reach for `Arc<Mutex<_>>`; it asks the backend
//! for a channel or a cell, and every send, receive, load and update is an
//! operation, which a test backend can make deterministic.
//!
//! Unlike files and sockets, channel endpoints and cells are **owned
//! values**, not scoped resources. They hold nothing that needs committing,
//! so dropping one is always correct, and a core can clone them and move
//! them into the tasks or threads it starts.
//!
//! # Channels
//!
//! [`Channel`] creates a sender of type `S` and a receiver of type `R`. On
//! the sender, [`SendMsg`] waits for room in a bounded channel; on the
//! receiver, [`RecvMsg`] waits for a message. [`try_send`] and [`try_recv`]
//! do not wait: they are `SendMsg` and `RecvMsg` with a timeout of zero.
//! The channel closes when every
//! sender is dropped (receives then fail with [`RecvError::Closed`] once it
//! is empty) or when the receiver is dropped (sends fail with
//! [`SendError::Closed`]).
//!
//! ```
//! use seam_core::prelude::*;
//! use seam_core::ops::sync::{Channel, RecvError, RecvMsg, SendMsg};
//!
//! /// Squares every number on one channel and sends it on to another,
//! /// until the input closes.
//! async fn square_all<I, O>(input: &mut I, output: &mut O) -> Result<(), RecvError>
//! where
//!     I: SendPerform<RecvMsg<u64>>,
//!     O: SendPerform<SendMsg<u64>>,
//! {
//!     loop {
//!         match input.perform_send(RecvMsg::new()).await {
//!             Ok(n) => {
//!                 if output.perform_send(SendMsg(n * n)).await.is_err() {
//!                     return Ok(()); // nobody is listening any more
//!                 }
//!             }
//!             Err(RecvError::Closed) => return Ok(()),
//!             Err(e) => return Err(e),
//!         }
//!     }
//! }
//! ```
//!
//! # Cells
//!
//! [`Share`] puts a value in a cell of type `C`, which can be cloned to give
//! other tasks or threads access to the same value. [`Load`] reads a copy,
//! and [`Update`] runs a closure on it atomically, with no other task able
//! to observe or change it in between; [`store`] replaces it, as an
//! `Update`.
//!
//! ```
//! use seam_core::prelude::*;
//! use seam_core::ops::sync::{Load, Update};
//!
//! /// Counts a request, and returns how many there have been. Atomic, so
//! /// concurrent callers never lose a count.
//! fn count_request<C: for<'a> Perform<Update<'a, u64, u64>>>(counter: &mut C) -> u64 {
//!     counter.perform(Update::new(&mut |n: &mut u64| {
//!         *n += 1;
//!         *n
//!     }))
//! }
//!
//! # struct Cell(u64);
//! # impl<'a> Perform<Update<'a, u64, u64>> for Cell {
//! #     fn perform(&mut self, op: Update<'a, u64, u64>) -> u64 { (op.0)(&mut self.0) }
//! # }
//! # impl Inspect<Load<u64>> for Cell {
//! #     fn inspect(&self, _: Load<u64>) -> u64 { self.0 }
//! # }
//! let mut counter = Cell(0);
//! count_request(&mut counter);
//! assert_eq!(count_request(&mut counter), 2);
//! assert_eq!(counter.inspect(Load::new()), 2);
//! ```
//!
//! Do not touch the same cell from inside an `Update` closure: the cell is
//! locked while the closure runs.

use crate::three_ways::three_ways;
use core::fmt;
use core::marker::PhantomData;

/// Create a channel, producing a sender of type `S` and a receiver of type
/// `R`. The message type is the endpoints' business: for example
/// `seam_std::sync::Sender<T>` and `seam_std::sync::Receiver<T>`.
pub struct Channel<S, R> {
    /// How many messages may wait to be received; `None` for no limit.
    pub capacity: Option<usize>,
    endpoints: PhantomData<fn() -> (S, R)>,
}

impl<S, R> Channel<S, R> {
    /// A channel holding up to `capacity` messages waiting to be received.
    /// Senders wait (with [`SendMsg`]) while it is full.
    ///
    /// # Panics
    ///
    /// If `capacity` is 0.
    pub fn bounded(capacity: usize) -> Self {
        assert!(
            capacity > 0,
            "a channel needs room for at least one message"
        );
        Self {
            capacity: Some(capacity),
            endpoints: PhantomData,
        }
    }

    /// A channel with no limit on waiting messages. Sending never waits.
    pub fn unbounded() -> Self {
        Self {
            capacity: None,
            endpoints: PhantomData,
        }
    }
}

/// Send a message, waiting for room if the channel is full. Performed on a
/// sender.
///
/// A backend that cannot wait (the plain test `Sim`) fails with
/// [`SendError::Full`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendMsg<T>(pub T);

/// Receive a message, waiting for one if the channel is empty. Performed
/// on the receiver.
///
/// A backend that cannot wait (the plain test `Sim`) fails with
/// [`RecvError::Empty`] instead.
pub struct RecvMsg<T> {
    message: PhantomData<fn() -> T>,
}

/// Why a message was not sent. Carries the message back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError<T> {
    /// The receiver is gone.
    Closed(T),
    /// The channel is full, and the operation could not wait.
    Full(T),
}

/// Why no message was received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecvError {
    /// Every sender is gone, and no messages are left.
    Closed,
    /// No message is waiting, and the operation could not wait.
    Empty,
}

/// Put `value` in a new cell of type `C`, which can be cloned to share it.
pub struct Share<T, C> {
    /// The initial value.
    pub value: T,
    cell: PhantomData<fn() -> C>,
}

/// Read a copy of a cell's value. Performed on the cell.
pub struct Load<T> {
    value: PhantomData<fn() -> T>,
}

/// Run a closure on a cell's value, atomically, and return what it
/// returns. Performed on the cell.
///
/// The closure is borrowed, so that a bound can name the operation without
/// naming the closure's type: `C: for<'a> Perform<Update<'a, T, R>>`.
pub struct Update<'a, T, R>(pub &'a mut (dyn FnMut(&mut T) -> R + Send));

impl<T> SendError<T> {
    /// The message that was not sent.
    pub fn into_inner(self) -> T {
        match self {
            Self::Closed(t) | Self::Full(t) => t,
        }
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed(_) => "the receiver is gone",
            Self::Full(_) => "the channel is full",
        })
    }
}

impl<T: fmt::Debug> core::error::Error for SendError<T> {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "every sender is gone",
            Self::Empty => "no message is waiting",
        })
    }
}

impl core::error::Error for RecvError {}

/// So that cores working in `io::Result` can use channels.
impl<T> From<SendError<T>> for crate::io::Error {
    fn from(e: SendError<T>) -> Self {
        match e {
            SendError::Closed(_) => {
                Self::new(crate::io::ErrorKind::BrokenPipe, "channel is closed")
            }
            SendError::Full(_) => Self::new(crate::io::ErrorKind::WouldBlock, "channel is full"),
        }
    }
}

/// So that cores working in `io::Result` can use channels.
impl From<RecvError> for crate::io::Error {
    fn from(e: RecvError) -> Self {
        match e {
            RecvError::Closed => Self::new(crate::io::ErrorKind::BrokenPipe, "channel is closed"),
            RecvError::Empty => Self::new(crate::io::ErrorKind::WouldBlock, "channel is empty"),
        }
    }
}

/// So that cores working in `std::io::Result` can use channels.
#[cfg(feature = "std")]
impl<T> From<SendError<T>> for std::io::Error {
    fn from(e: SendError<T>) -> Self {
        crate::io::Error::from(e).into()
    }
}

/// So that cores working in `std::io::Result` can use channels.
#[cfg(feature = "std")]
impl From<RecvError> for std::io::Error {
    fn from(e: RecvError) -> Self {
        crate::io::Error::from(e).into()
    }
}

impl<T> RecvMsg<T> {
    /// Describes receiving a message.
    pub fn new() -> Self {
        Self {
            message: PhantomData,
        }
    }
}

impl<T, C> Share<T, C> {
    /// Describes sharing `value`.
    pub fn new(value: T) -> Self {
        Self {
            value,
            cell: PhantomData,
        }
    }
}

impl<T> Load<T> {
    /// Describes reading the value.
    pub fn new() -> Self {
        Self { value: PhantomData }
    }
}

impl<'a, T, R> Update<'a, T, R> {
    /// Describes running `f` on the value.
    pub fn new(f: &'a mut (impl FnMut(&mut T) -> R + Send)) -> Self {
        Self(f)
    }
}

macro_rules! marker_impls {
    ($($ty:ident),*) => {$(
        impl<T> Default for $ty<T> {
            fn default() -> Self {
                Self::new()
            }
        }

        impl<T> Clone for $ty<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T> Copy for $ty<T> {}

        impl<T> fmt::Debug for $ty<T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(stringify!($ty))
            }
        }
    )*};
}
marker_impls!(RecvMsg, Load);

impl<S, R> fmt::Debug for Channel<S, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl<T: fmt::Debug, C> fmt::Debug for Share<T, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Share").field("value", &self.value).finish()
    }
}

impl<T, R> fmt::Debug for Update<'_, T, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Update")
    }
}

impl<S, R> crate::Operation for Channel<S, R> {
    type Outcome = (S, R);
}

impl<T> crate::Operation for SendMsg<T> {
    type Outcome = Result<(), SendError<T>>;
}

impl<T> crate::Operation for RecvMsg<T> {
    type Outcome = Result<T, RecvError>;
}

impl<T, C> crate::Operation for Share<T, C> {
    type Outcome = C;
}

impl<T> crate::Operation for Load<T> {
    type Outcome = T;
}

impl<T> crate::Query for Load<T> {}

impl<T, R> crate::Operation for Update<'_, T, R> {
    type Outcome = R;
}

/// Gives up as `Full`, handing the value back.
#[cfg(feature = "time")]
impl<T> crate::ops::time::TimesOut for SendMsg<T> {
    type Expired = Result<(), SendError<T>>;

    fn timed_out(self) -> Self::Expired {
        Err(SendError::Full(self.0))
    }
}

/// Gives up as `Empty`.
#[cfg(feature = "time")]
impl<T> crate::ops::time::TimesOut for RecvMsg<T> {
    type Expired = Result<T, RecvError>;

    fn timed_out(self) -> Self::Expired {
        Err(RecvError::Empty)
    }
}

crate::root::keys!(
    [S, R] Channel<S, R> => Channel,
    [T, C] Share<T, C> => Share,
);

three_ways! {
    /// Sends `value` if there is room right now: [`SendMsg`] with a timeout
    /// of zero. A full channel hands the value back in
    /// [`SendError::Full`].
    pub fn try_send, try_send_async, try_send_send
    [S, T] (tx: &mut S, value: T) -> Result<(), SendError<T>>
    where
        sync { S: crate::Perform<crate::ops::time::Timeout<SendMsg<T>>> }
        async { S: crate::future::AsyncPerform<crate::ops::time::Timeout<SendMsg<T>>> }
        send { S: crate::future::SendPerform<crate::ops::time::Timeout<SendMsg<T>>> + Send, T: Send }
    {
        run!(tx, crate::ops::time::Timeout::new(core::time::Duration::ZERO, SendMsg(value)))
    }

    /// Receives a message if one is waiting: [`RecvMsg`] with a timeout of
    /// zero. An empty channel reports [`RecvError::Empty`].
    pub fn try_recv, try_recv_async, try_recv_send
    [R, T] (rx: &mut R) -> Result<T, RecvError>
    where
        sync { R: crate::Perform<crate::ops::time::Timeout<RecvMsg<T>>> }
        async { R: crate::future::AsyncPerform<crate::ops::time::Timeout<RecvMsg<T>>> }
        send { R: crate::future::SendPerform<crate::ops::time::Timeout<RecvMsg<T>>> + Send, T: Send }
    {
        run!(rx, crate::ops::time::Timeout::new(core::time::Duration::ZERO, RecvMsg::new()))
    }

    /// Replaces a cell's value, atomically: an [`Update`].
    pub fn store, store_async, store_send
    [C, T] (cell: &mut C, value: T) -> ()
    where
        sync { C: for<'a> crate::Perform<Update<'a, T, ()>>, T: Send }
        async { C: for<'a> crate::future::AsyncPerform<Update<'a, T, ()>>, T: Send }
        send { C: for<'a> crate::future::SendPerform<Update<'a, T, ()>> + Send, T: Send }
    {
        let mut value = Some(value);
        run!(cell, Update::new(&mut |v: &mut T| {
            if let Some(new) = value.take() {
                *v = new;
            }
        }))
    }
}
