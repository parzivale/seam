//! Resources that commit on success and roll back on failure.
//!
//! A resource that has something to commit (a file to sync and rename into
//! place, a stream to shut down cleanly, a task to wait for) implements
//! [`Finalize`] or [`AsyncFinalize`]. A core then uses it through
//! [`Scoped::scope`] or [`AsyncScoped::scope_async`]:
//!
//! - when the closure returns `Ok`, the resource is closed, and an error
//!   from closing is returned;
//! - when the closure returns `Err`, panics, or (for async) the future is
//!   dropped before it finishes, the resource is abandoned.
//!
//! # Drop means abandon
//!
//! Implementations must make dropping the resource leave things as
//! [`Finalize::abandon`] would: committing happens only in `close`. That is
//! what makes scopes cancellation-safe. A future can be dropped at any
//! `.await`, including halfway through an async `close`; whatever it held is
//! dropped, and dropping never commits a half-finished result.

use core::future::Future;

/// A resource with a commit step (`close`) and a rollback step (`abandon`).
///
/// See the [module docs](self) for the contract, and [`Scoped`] for how a
/// core uses it.
pub trait Finalize: Sized {
    /// Why closing failed.
    type Error;

    /// Commits: flush, sync, rename into place, shut down cleanly.
    fn close(self) -> Result<(), Self::Error>;

    /// Rolls back: discard without committing. The default drops the
    /// resource, which must have the same effect.
    fn abandon(self) {}
}

/// A resource whose commit step is async.
///
/// Every `Send` [`Finalize`] is also an `AsyncFinalize`. Abandoning is
/// always synchronous, since it must also be what dropping does, and drop
/// cannot wait.
///
/// The future from `close_async` is `Send`, so scopes work in tasks on a
/// multithreaded runtime.
pub trait AsyncFinalize: Sized {
    /// Why closing failed.
    type Error;

    /// Commits.
    fn close_async(self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Rolls back. The default drops the resource, which must have the same
    /// effect.
    fn abandon_async(self) {}
}

impl<T: Finalize + Send> AsyncFinalize for T {
    type Error = T::Error;

    async fn close_async(self) -> Result<(), Self::Error> {
        self.close()
    }

    fn abandon_async(self) {
        self.abandon();
    }
}

/// Lends a resource to a closure, then commits or rolls it back.
///
/// This is an extension trait with a single blanket impl, so implementors of
/// [`Finalize`] cannot override `scope` and skip cleanup.
///
/// ```
/// use seam_core::{Finalize, Scoped};
///
/// /// Writes that only land on `close`.
/// struct Txn<'a> {
///     pending: Vec<u32>,
///     committed: &'a mut Vec<u32>,
/// }
///
/// #[derive(Debug)]
/// struct Full;
///
/// impl Finalize for Txn<'_> {
///     type Error = Full;
///
///     fn close(self) -> Result<(), Full> {
///         if self.committed.len() + self.pending.len() > 3 {
///             return Err(Full);
///         }
///         self.committed.extend(self.pending);
///         Ok(())
///     }
///     // `abandon` defaults to dropping: nothing is committed.
/// }
///
/// let mut db = Vec::new();
///
/// // The closure succeeds: committed.
/// let txn = Txn { pending: Vec::new(), committed: &mut db };
/// txn.scope(|t| {
///     t.pending.push(1);
///     Ok::<_, Full>(())
/// })
/// .unwrap();
/// assert_eq!(db, [1]);
///
/// // The closure fails: rolled back.
/// let txn = Txn { pending: Vec::new(), committed: &mut db };
/// let failed = txn.scope(|t| {
///     t.pending.push(2);
///     Err::<(), _>(Full)
/// });
/// assert!(failed.is_err());
/// assert_eq!(db, [1]);
///
/// // Closing fails: the error comes back out of `scope`.
/// let txn = Txn { pending: Vec::new(), committed: &mut db };
/// let failed = txn.scope(|t| {
///     t.pending.extend([3, 4, 5]);
///     Ok::<_, Full>(())
/// });
/// assert!(failed.is_err());
/// assert_eq!(db, [1]);
/// ```
pub trait Scoped: Finalize {
    /// Lends the resource to `f`. If `f` returns `Ok`, closes the resource
    /// and returns `f`'s value, or the close error. If `f` returns `Err` or
    /// panics, abandons the resource.
    fn scope<R, E>(self, f: impl FnOnce(&mut Self) -> Result<R, E>) -> Result<R, E>
    where
        E: From<Self::Error>;
}

impl<T: Finalize> Scoped for T {
    fn scope<R, E>(self, f: impl FnOnce(&mut Self) -> Result<R, E>) -> Result<R, E>
    where
        E: From<Self::Error>,
    {
        let mut guard = Guard::new(self, T::abandon);
        let out = f(guard.get());
        let resource = guard.disarm();
        match out {
            Ok(value) => {
                resource.close()?;
                Ok(value)
            }
            Err(e) => {
                resource.abandon();
                Err(e)
            }
        }
    }
}

/// Lends a resource to an async closure, then commits or rolls it back.
///
/// Like [`Scoped`], plus cancellation: if the future is dropped before it
/// finishes, the resource is abandoned. A single blanket impl means
/// implementors cannot override it.
///
/// ```
/// use std::sync::Mutex;
/// use seam_core::future::block_on;
/// use seam_core::{AsyncFinalize, AsyncScoped};
///
/// /// A message that is only sent on `close`.
/// struct Draft<'a> {
///     text: String,
///     outbox: &'a Mutex<Vec<String>>,
/// }
///
/// impl AsyncFinalize for Draft<'_> {
///     type Error = std::convert::Infallible;
///
///     async fn close_async(self) -> Result<(), Self::Error> {
///         self.outbox.lock().unwrap().push(self.text);
///         Ok(())
///     }
/// }
///
/// let outbox = Mutex::new(Vec::new());
/// let draft = Draft { text: String::new(), outbox: &outbox };
/// block_on(draft.scope_async(async |d| {
///     d.text.push_str("hello");
///     Ok::<_, std::convert::Infallible>(())
/// }))
/// .unwrap();
/// assert_eq!(*outbox.lock().unwrap(), ["hello"]);
/// ```
pub trait AsyncScoped: AsyncFinalize {
    /// Lends the resource to `f`. If `f` returns `Ok`, closes the resource
    /// and returns `f`'s value, or the close error. If `f` returns `Err` or
    /// panics, or the returned future is dropped before it finishes,
    /// abandons the resource.
    fn scope_async<R, E>(
        self,
        f: impl AsyncFnOnce(&mut Self) -> Result<R, E>,
    ) -> impl Future<Output = Result<R, E>>
    where
        E: From<Self::Error>;
}

impl<T: AsyncFinalize> AsyncScoped for T {
    async fn scope_async<R, E>(self, f: impl AsyncFnOnce(&mut Self) -> Result<R, E>) -> Result<R, E>
    where
        E: From<Self::Error>,
    {
        let mut guard = Guard::new(self, T::abandon_async);
        // Dropped here (a panic or a cancellation) abandons the resource.
        let out = f(guard.get()).await;
        let resource = guard.disarm();
        match out {
            Ok(value) => {
                // Dropped here, the resource is dropped inside `close_async`,
                // which by the contract also abandons it.
                resource.close_async().await?;
                Ok(value)
            }
            Err(e) => {
                resource.abandon_async();
                Err(e)
            }
        }
    }
}

/// Holds a resource, and abandons it if dropped while still holding it,
/// which only happens when the scope's closure unwinds or is cancelled.
struct Guard<T> {
    resource: Option<T>,
    abandon: fn(T),
}

impl<T> Guard<T> {
    fn new(resource: T, abandon: fn(T)) -> Self {
        Self {
            resource: Some(resource),
            abandon,
        }
    }

    fn get(&mut self) -> &mut T {
        self.resource
            .as_mut()
            .expect("the resource is held until the guard is disarmed")
    }

    fn disarm(mut self) -> T {
        self.resource
            .take()
            .expect("the guard is disarmed only once")
    }
}

impl<T> Drop for Guard<T> {
    fn drop(&mut self) {
        if let Some(resource) = self.resource.take() {
            (self.abandon)(resource);
        }
    }
}
