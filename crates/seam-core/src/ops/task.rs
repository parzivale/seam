//! Spawning and joining concurrent tasks.
//!
//! Spawning is an [`Acquisition`](crate::Acquisition): a task handle exists
//! only inside a scope, like a file or a socket. When the scope succeeds,
//! closing the handle waits for the task; when it fails, panics or is
//! cancelled, abandoning it aborts the task. So a task cannot outlive the
//! code that spawned it by accident. Perform [`Detach`] on the handle for a
//! task that should keep running on its own.
//!
//! On the handle, perform [`Join`] to take the task's output, [`Abort`] to
//! cancel it, or [`IsFinished`] to check on it. [`YieldNow`] lets other
//! tasks run.
//!
//! | Acquisition | Spawns | Feature |
//! |---|---|---|
//! | [`Spawn`] | one task | `rt` |
//! | `SpawnAll` | a batch of tasks, joined together | `rt` + `alloc` |
//! | `SpawnWith` | a task that owns a resource acquired for it, such as an accepted connection | `rt` + `alloc` |
//!
//! # Naming the future
//!
//! A bound has to name the future type it spawns, and a generic core cannot
//! name the type of an `async` block it writes itself. Take the task as a
//! function argument instead; the future type becomes a type parameter,
//! inferred where the function is called, and nothing is boxed:
//!
//! ```
//! # #[cfg(feature = "alloc")] {
//! use core::future::Future;
//! use seam_core::prelude::*;
//! use seam_core::ops::task::{Join, JoinError, SpawnAll};
//!
//! /// Runs `job` on every input concurrently and collects the results. If
//! /// this future is dropped, every task is aborted.
//! async fn fan_out<C, J, Fut, H>(c: &mut C, inputs: Vec<u32>, job: J) -> Result<Vec<u32>, JoinError>
//! where
//!     C: AsyncProvide<SpawnAll<Fut, H>>,
//!     J: Fn(u32) -> Fut,
//!     Fut: Future<Output = u32>,
//!     H: SendPerform<Join<u32>>,
//! {
//!     let tasks = SpawnAll::new(inputs.into_iter().map(job).collect());
//!     c.acquire_async(tasks, async |_c, handles| {
//!         let mut results = Vec::new();
//!         for h in handles {
//!             results.push(h.perform_send(Join::new()).await?);
//!         }
//!         Ok(results)
//!     })
//!     .await
//! }
//! # }
//! ```
//!
//! With `alloc`, `Spawn::boxed` erases the future's type, so one bound
//! covers any task, `async` blocks included.
//!
//! # Capabilities inside tasks
//!
//! A task is `'static`, so it cannot borrow the capability that spawned it.
//! Give it its own: the root capabilities of the real backends are `Clone`,
//! so a core can bound `C: Clone + Send + 'static` and move a clone in.

use core::fmt;
use core::future::Future;
use core::marker::PhantomData;

/// Spawn one task, providing a handle of type `H` for the length of a
/// scope. Closing the handle waits for the task; abandoning it aborts it.
pub struct Spawn<F, H> {
    /// The task.
    pub future: F,
    handle: PhantomData<fn() -> H>,
}

/// Spawn a batch of tasks, providing their handles, in order, for the
/// length of a scope. Closing waits for all of them and reports the first
/// failure; abandoning aborts them all.
#[cfg(feature = "alloc")]
pub struct SpawnAll<F, H> {
    /// The tasks.
    pub futures: alloc::vec::Vec<F>,
    handle: PhantomData<fn() -> H>,
}

/// Acquire `A` and hand the resource to `body` in a new task, which owns
/// it. Provides the task's handle, of type `H`.
///
/// This is how a resource reaches a task. A core cannot move a resource
/// anywhere, since it only ever borrows one, so the provider acquires it on
/// the task's behalf: for example, a listener accepts a connection and
/// spawns `body` with the stream. Inside the task, the resource is closed
/// when `body` returns `Ok` and abandoned otherwise.
#[cfg(feature = "alloc")]
pub struct SpawnWith<A: crate::Acquisition, T, E, H> {
    /// What to acquire for the task.
    pub acquisition: A,
    /// What the task does with it.
    pub body: TaskBody<A::Resource, T, E>,
    handle: PhantomData<fn() -> H>,
}

/// The body of a [`SpawnWith`] task: borrows the resource and returns a
/// boxed future.
#[cfg(feature = "alloc")]
pub type TaskBody<R, T, E> =
    alloc::boxed::Box<dyn for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<T, E>> + Send>;

/// A type-erased future.
#[cfg(feature = "alloc")]
pub type BoxFuture<'a, T> = core::pin::Pin<alloc::boxed::Box<dyn Future<Output = T> + Send + 'a>>;

/// Wait for a task to finish and take its output. Performed on the handle.
///
/// # Panics
///
/// Joining a handle that was already joined may panic.
pub struct Join<T> {
    output: PhantomData<fn() -> T>,
}

/// Cancel a task. Performed on the handle; a later [`Join`] reports
/// [`JoinError::Cancelled`] unless the task had already finished.
#[derive(Debug, Clone, Copy, Default)]
pub struct Abort;

/// Let a task keep running after its scope ends. Performed on the handle;
/// the scope then neither waits for it nor aborts it.
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
impl<T, H> Spawn<BoxFuture<'static, T>, H> {
    /// Describes spawning `future`, boxed so that its type is erased.
    pub fn boxed(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self::new(alloc::boxed::Box::pin(future))
    }
}

#[cfg(feature = "alloc")]
impl<F, H> SpawnAll<F, H> {
    /// Describes spawning each of `futures`.
    pub fn new(futures: alloc::vec::Vec<F>) -> Self {
        Self {
            futures,
            handle: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
impl<A: crate::Acquisition, T, E, H> SpawnWith<A, T, E, H> {
    /// Describes acquiring `acquisition` and running `body` with it in a new
    /// task.
    pub fn new(
        acquisition: A,
        body: impl for<'a> FnOnce(&'a mut A::Resource) -> BoxFuture<'a, Result<T, E>> + Send + 'static,
    ) -> Self {
        Self {
            acquisition,
            body: alloc::boxed::Box::new(body),
            handle: PhantomData,
        }
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

#[cfg(feature = "alloc")]
impl<F, H> fmt::Debug for SpawnAll<F, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnAll")
            .field("tasks", &self.futures.len())
            .finish()
    }
}

#[cfg(feature = "alloc")]
impl<A: crate::Acquisition + fmt::Debug, T, E, H> fmt::Debug for SpawnWith<A, T, E, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnWith")
            .field("acquisition", &self.acquisition)
            .finish_non_exhaustive()
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

/// So that cores working in `io::Result` can spawn tasks.
#[cfg(feature = "std")]
impl From<JoinError> for std::io::Error {
    fn from(e: JoinError) -> Self {
        std::io::Error::other(e)
    }
}

impl<F: Future, H> crate::Acquisition for Spawn<F, H> {
    type Resource = H;
    type Error = JoinError;
}

#[cfg(feature = "alloc")]
impl<F: Future, H> crate::Acquisition for SpawnAll<F, H> {
    type Resource = alloc::vec::Vec<H>;
    type Error = JoinError;
}

#[cfg(feature = "alloc")]
impl<A: crate::Acquisition, T, E, H> crate::Acquisition for SpawnWith<A, T, E, H> {
    type Resource = H;
    type Error = A::Error;
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
