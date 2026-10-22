//! Spawning and joining concurrent tasks.
//!
//! Spawning is an [`Acquisition`](crate::Acquisition): a task handle (or a
//! task set) exists only inside a scope, like a file or a socket. When the scope succeeds,
//! closing the handle waits for the task; when it fails, panics or is
//! cancelled, abandoning it aborts the task. So a task cannot outlive the
//! code that spawned it by accident. Perform [`Detach`] on the handle for a
//! task that should keep running on its own.
//!
//! On the handle, perform [`Join`] to take the task's output, [`Abort`] to
//! cancel it, or [`IsFinished`] to check on it. [`YieldNow`] lets other
//! tasks run.
//!
//! | Acquisition | Provides | Allocates in seam |
//! |---|---|---|
//! | [`Spawn`] | a handle to one task | no |
//! | [`SpawnSet`] | a set to spawn tasks into ([`SpawnInto`]) and collect them from ([`JoinNext`]) | no |
//! | [`SpawnWith`] | a handle to a task that owns a resource acquired for it, such as an accepted connection | no |
//!
//! None of these needs `alloc`; whether spawning allocates is up to the
//! backend (Tokio allocates every task). With `alloc`, `Spawn::boxed` and
//! `SpawnWith::boxed` take futures and closures whose types cannot be named,
//! at the cost of a box.
//!
//! # Naming the future
//!
//! A bound has to name the future type it spawns, and a generic core cannot
//! name the type of an `async` block it writes itself. Take the task as a
//! function argument instead; the future type becomes a type parameter,
//! inferred where the function is called:
//!
//! ```
//! use core::future::Future;
//! use seam_core::prelude::*;
//! use seam_core::ops::task::{JoinError, JoinNext, SpawnInto, SpawnSet};
//!
//! /// Runs `job` on every input concurrently and sums the results. If this
//! /// future is dropped, every task is aborted.
//! async fn fan_out<C, S, J, Fut>(c: &mut C, inputs: &[u32], job: J) -> Result<u32, JoinError>
//! where
//!     C: AsyncProvide<SpawnSet<S>>,
//!     S: SendPerform<SpawnInto<Fut>> + SendPerform<JoinNext<u32>>,
//!     J: Fn(u32) -> Fut,
//!     Fut: Future<Output = u32>,
//! {
//!     c.acquire_async(SpawnSet::new(), async |_, set| {
//!         for &input in inputs {
//!             if set.perform_send(SpawnInto::new(job(input))).await.is_err() {
//!                 break; // a fixed-capacity set is full
//!             }
//!         }
//!         let mut sum = 0;
//!         while let Some(result) = set.perform_send(JoinNext::new()).await {
//!             sum += result?;
//!         }
//!         Ok(sum)
//!     })
//!     .await
//! }
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

/// Spawn one task, providing a handle of type `H` for the length of a
/// scope. Closing the handle waits for the task; abandoning it aborts it.
pub struct Spawn<F, H> {
    /// The task.
    pub future: F,
    handle: PhantomData<fn() -> H>,
}

/// Open an empty set of tasks of the backend's type `S`, for the length of
/// a scope. In the scope, perform [`SpawnInto`] on the set to add tasks and
/// [`JoinNext`] to collect their outputs as they finish.
///
/// Closing the set waits for every task still in it and reports the first
/// failure; abandoning it aborts them all. Storing the tasks is the
/// backend's business: seam itself allocates nothing.
pub struct SpawnSet<S> {
    set: PhantomData<fn() -> S>,
}

/// Add a task to a set opened with [`SpawnSet`]. Performed on the set.
///
/// A set with a fixed capacity hands the future back when it is full.
pub struct SpawnInto<F> {
    /// The task.
    pub future: F,
}

/// Wait for the next task in a set to finish, and take its output.
/// Performed on the set; `None` once the set is empty.
pub struct JoinNext<T> {
    output: PhantomData<fn() -> T>,
}

/// Acquire `A` and run `body` with the resource in a new task, which owns
/// the resource. Provides the task's handle, of type `H`.
///
/// This is how a resource reaches a task. A core cannot move a resource
/// anywhere, since it only ever borrows one, so the provider acquires it on
/// the task's behalf: for example, a listener accepts a connection and
/// spawns `body` with the stream. Inside the task, the resource is closed
/// when `body` returns `Ok` and abandoned otherwise.
///
/// `body` is a [`TaskBody`]: a named type, so that a bound can name it, and
/// nothing is boxed. With `alloc`, `SpawnWith::boxed` takes a closure
/// instead.
pub struct SpawnWith<A, B, H> {
    /// What to acquire for the task.
    pub acquisition: A,
    /// What the task does with it.
    pub body: B,
    handle: PhantomData<fn() -> H>,
}

/// What a [`SpawnWith`] task does with the resource acquired for it.
///
/// A trait rather than a closure so that it can be named in a bound, and so
/// that its future is known to be `Send`, without boxing:
///
/// ```
/// # #[cfg(feature = "io")] {
/// use std::io;
/// use seam_core::prelude::*;
/// use seam_core::ops::io::{ReadToEnd, WriteAll};
/// use seam_core::ops::task::TaskBody;
///
/// /// Replies to whatever the peer sent with its length.
/// struct CountBytes;
///
/// impl<S> TaskBody<S> for CountBytes
/// where
///     S: for<'a> SendPerform<ReadToEnd<'a>> + for<'a> SendPerform<WriteAll<'a>> + Send,
/// {
///     type Output = usize;
///     type Error = io::Error;
///
///     async fn run(self, stream: &mut S) -> io::Result<usize> {
///         let mut bytes = Vec::new();
///         let n = stream.perform_send(ReadToEnd(&mut bytes)).await?;
///         stream.perform_send(WriteAll(n.to_string().as_bytes())).await?;
///         Ok(n)
///     }
/// }
/// # }
/// ```
pub trait TaskBody<R>: Send + 'static {
    /// What the task produces when it succeeds.
    type Output: Send + 'static;
    /// Why the task failed.
    type Error: Send + 'static;

    /// Runs the task. The resource is closed if this returns `Ok`, and
    /// abandoned otherwise.
    fn run(
        self,
        resource: &mut R,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

/// A type-erased future.
#[cfg(feature = "alloc")]
pub type BoxFuture<'a, T> = core::pin::Pin<alloc::boxed::Box<dyn Future<Output = T> + Send + 'a>>;

/// A [`TaskBody`] made from a closure, by `SpawnWith::boxed`.
#[cfg(feature = "alloc")]
pub struct BoxedBody<R, T, E>(alloc::boxed::Box<dyn BodyFn<R, T, E>>);

/// The closure inside a [`BoxedBody`].
#[cfg(feature = "alloc")]
trait BodyFn<R, T, E>: for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<T, E>> + Send {}

#[cfg(feature = "alloc")]
impl<R, T, E, F> BodyFn<R, T, E> for F where
    F: for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<T, E>> + Send
{
}

#[cfg(feature = "alloc")]
impl<R: 'static, T: Send + 'static, E: Send + 'static> TaskBody<R> for BoxedBody<R, T, E> {
    type Output = T;
    type Error = E;

    fn run(self, resource: &mut R) -> impl Future<Output = Result<T, E>> + Send {
        (self.0)(resource)
    }
}

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

impl<S> SpawnSet<S> {
    /// Describes opening an empty task set.
    pub fn new() -> Self {
        Self { set: PhantomData }
    }
}

impl<S> Default for SpawnSet<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F> SpawnInto<F> {
    /// Describes adding `future` to the set.
    pub fn new(future: F) -> Self {
        Self { future }
    }
}

impl<T> JoinNext<T> {
    /// Describes waiting for the next task to finish.
    pub fn new() -> Self {
        Self {
            output: PhantomData,
        }
    }
}

impl<T> Default for JoinNext<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A, B, H> SpawnWith<A, B, H> {
    /// Describes acquiring `acquisition` and running `body` with it in a new
    /// task.
    pub fn new(acquisition: A, body: B) -> Self {
        Self {
            acquisition,
            body,
            handle: PhantomData,
        }
    }
}

#[cfg(feature = "alloc")]
impl<A: crate::Acquisition, T, E, H> SpawnWith<A, BoxedBody<A::Resource, T, E>, H> {
    /// Describes acquiring `acquisition` and running the closure `body` with
    /// it in a new task. The closure and its future are boxed.
    pub fn boxed(
        acquisition: A,
        body: impl for<'a> FnOnce(&'a mut A::Resource) -> BoxFuture<'a, Result<T, E>> + Send + 'static,
    ) -> Self {
        Self::new(acquisition, BoxedBody(alloc::boxed::Box::new(body)))
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

impl<S> fmt::Debug for SpawnSet<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SpawnSet")
    }
}

impl<F> fmt::Debug for SpawnInto<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SpawnInto")
    }
}

impl<T> fmt::Debug for JoinNext<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinNext")
    }
}

impl<A: fmt::Debug, B, H> fmt::Debug for SpawnWith<A, B, H> {
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

impl<S> crate::Acquisition for SpawnSet<S> {
    type Resource = S;
    type Error = JoinError;
}

impl<A: crate::Acquisition, B, H> crate::Acquisition for SpawnWith<A, B, H> {
    type Resource = H;
    type Error = A::Error;
}

impl<F: Future> crate::Operation for SpawnInto<F> {
    type Outcome = Result<(), crate::Rejected<F>>;
}

impl<T> crate::Operation for JoinNext<T> {
    type Outcome = Option<Result<T, JoinError>>;
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
