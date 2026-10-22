//! Spawning and joining concurrent tasks.
//!
//! [`Spawn`] hands a future to the backend's executor and produces a handle
//! of the backend's type `H`. [`Join`], [`Abort`], [`Detach`] and
//! [`IsFinished`] are performed on that handle. [`YieldNow`] lets other
//! tasks run.
//!
//! Dropping a handle aborts its task unless it was detached.
//!
//! # Naming the future
//!
//! A bound has to name the future type it spawns, and a generic core cannot
//! name the type of an `async` block it writes itself. There are two ways
//! around that.
//!
//! **Pass the task in.** Take the task as a function argument; the future
//! type becomes a type parameter, inferred where the function is called.
//! This needs no allocation:
//!
//! ```
//! use core::future::Future;
//! use seam_core::future::SendPerform;
//! use seam_core::ops::task::{Join, JoinError, Spawn};
//!
//! /// Runs `job` on every input concurrently, then collects the results.
//! async fn fan_out<C, J, Fut, H>(c: &mut C, inputs: Vec<u32>, job: J) -> Result<Vec<u32>, JoinError>
//! where
//!     C: SendPerform<Spawn<Fut, H>>,
//!     J: Fn(u32) -> Fut,
//!     Fut: Future<Output = u32>,
//!     H: SendPerform<Join<u32>>,
//! {
//!     let mut handles = Vec::new();
//!     for input in inputs {
//!         handles.push(c.perform_send(Spawn::new(job(input))).await);
//!     }
//!     let mut results = Vec::new();
//!     for mut h in handles {
//!         results.push(h.perform_send(Join::new()).await?);
//!     }
//!     Ok(results)
//! }
//! ```
//!
//! **Box it.** With the `alloc` feature, `BoxFuture` erases the future's
//! type, so one bound covers any task, `async` blocks included:
//!
//! ```
//! # #[cfg(feature = "alloc")] {
//! use seam_core::future::SendPerform;
//! use seam_core::ops::task::{BoxFuture, Spawn};
//!
//! async fn background<C, H>(c: &mut C, name: &'static str) -> H
//! where
//!     C: SendPerform<Spawn<BoxFuture<()>, H>>,
//! {
//!     c.perform_send(Spawn::boxed(async move {
//!         let _ = name; // ...whatever the task does
//!     }))
//!     .await
//! }
//! # }
//! ```
//!
//! # Capabilities inside tasks
//!
//! A task is `'static`, so it cannot borrow the capability that spawned it.
//! Give it its own: the root capabilities of the real backends are `Clone`,
//! so a core can bound `C: Clone + Send + 'static` and move a clone in.

use core::fmt;
use core::future::Future;
use core::marker::PhantomData;

/// Start running a future concurrently, producing a handle of type `H`.
///
/// Dropping the handle aborts the task, so a task never outlives the code
/// that spawned it by accident: cancelling a parent cancels its children.
/// Perform [`Detach`] on the handle to let the task run on its own.
///
/// Handles implement [`AsyncFinalize`](crate::AsyncFinalize): in a scope,
/// closing waits for the task to finish, and abandoning aborts it.
pub struct Spawn<F, H> {
    /// The task.
    pub future: F,
    handle: PhantomData<fn() -> H>,
}

/// Wait for a task to finish and take its output. Performed on the handle.
///
/// # Panics
///
/// Joining a handle that was already joined successfully may panic.
pub struct Join<T> {
    output: PhantomData<fn() -> T>,
}

/// Cancel a task. Performed on the handle; a later [`Join`] reports
/// [`JoinError::Cancelled`] unless the task had already finished.
#[derive(Debug, Clone, Copy, Default)]
pub struct Abort;

/// Let a task keep running after its handle is dropped. Performed on the
/// handle; afterwards the handle can still be joined.
#[derive(Debug, Clone, Copy, Default)]
pub struct Detach;

/// Whether a task has finished. Performed on the handle.
#[derive(Debug, Clone, Copy, Default)]
pub struct IsFinished;

/// Give other tasks a chance to run before continuing.
#[derive(Debug, Clone, Copy, Default)]
pub struct YieldNow;

/// Why a task produced no output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JoinError {
    /// The task panicked.
    Panicked,
    /// The task was aborted, or its executor shut down first.
    Cancelled,
}

/// A type-erased task, for spawning futures whose type cannot be named.
#[cfg(feature = "alloc")]
pub type BoxFuture<T> = core::pin::Pin<alloc::boxed::Box<dyn Future<Output = T> + Send + 'static>>;

impl<F, H> Spawn<F, H> {
    /// Describes spawning `future`.
    pub fn new(future: F) -> Self {
        Self {
            future,
            handle: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
impl<T, H> Spawn<BoxFuture<T>, H> {
    /// Describes spawning `future`, boxed so that its type is erased.
    pub fn boxed(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self::new(alloc::boxed::Box::pin(future))
    }
}

impl<T> Join<T> {
    /// Describes joining a task.
    pub fn new() -> Self {
        Self {
            output: PhantomData,
        }
    }
}

impl<T> Default for Join<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for Join<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Join<T> {}

impl<F, H> fmt::Debug for Spawn<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Spawn")
    }
}

impl<T> fmt::Debug for Join<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Join")
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Panicked => "task panicked",
            Self::Cancelled => "task was cancelled",
        })
    }
}

impl core::error::Error for JoinError {}

impl<F: Future, H> crate::Operation for Spawn<F, H> {
    type Outcome = H;
}

impl<T> crate::Operation for Join<T> {
    type Outcome = Result<T, JoinError>;
}

operations! {
    Abort => ();
    Detach => ();
    #[query] IsFinished => bool;
    YieldNow => ();
}
