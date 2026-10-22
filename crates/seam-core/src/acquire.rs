//! Resources, which a core can only use inside a scope.
//!
//! Some effects produce something that lives for a while: an open file, a
//! connected socket, a running task. In seam a core never owns one of these.
//! It asks for the capability to [`Provide`] it, and then
//! [`acquire`](Acquire::acquire)s it for the length of a closure:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use std::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::fs::Open;
//! use seam_core::ops::io::ReadToEnd;
//!
//! fn read_all<C, F>(c: &mut C, path: &str) -> io::Result<Vec<u8>>
//! where
//!     C: Provide<Open<F>>,
//!     F: for<'a> Perform<ReadToEnd<'a>>,
//! {
//!     c.acquire(Open::read(path), |_c, file| {
//!         let mut bytes = Vec::new();
//!         file.perform(ReadToEnd(&mut bytes))?;
//!         Ok(bytes)
//!     })
//! }
//! # }
//! ```
//!
//! The body borrows the resource, so it cannot keep it, drop it early or
//! leak it. It also gets the capability back, so it can perform other
//! effects, or acquire more resources, while it holds this one.
//!
//! # Commit on success, roll back otherwise
//!
//! When the body returns `Ok`, the provider **closes** the resource: it
//! flushes and syncs a file, renames an atomic file into place, shuts a
//! stream down cleanly, or waits for a task. An error from closing is
//! returned from `acquire`. When the body returns `Err`, panics, or (with
//! [`acquire_async`](AsyncAcquire::acquire_async)) is cancelled by dropping
//! its future, the provider **abandons** the resource instead.
//!
//! # Cancellation safety
//!
//! A provider must make dropping a resource leave things as abandoning it
//! would: only `close` commits. A future can be dropped at any `.await`,
//! including halfway through opening or closing; whatever it held is
//! dropped, and dropping never commits half a result.
//!
//! # Implementing a provider
//!
//! Backends implement [`Provide`] or [`AsyncProvide`]. Their `provide`
//! methods take a [`Token`], which only this crate can create, so a core
//! cannot call them to get a resource outside a scope. A provider can still
//! pass its token on to another provider it wraps.
//!
//! ```
//! use seam_core::{Acquire, Acquisition, Provide, Token};
//!
//! /// Writes that land in `committed` only when the transaction closes.
//! struct Begin;
//! impl Acquisition for Begin {
//!     type Resource = Vec<u32>;
//!     type Error = String;
//! }
//!
//! #[derive(Default)]
//! struct Db(Vec<u32>);
//!
//! impl Provide<Begin> for Db {
//!     fn provide(&mut self, _: Begin, _: Token) -> Result<Vec<u32>, String> {
//!         Ok(Vec::new())
//!     }
//!
//!     fn close(&mut self, pending: Vec<u32>) -> Result<(), String> {
//!         self.0.extend(pending);
//!         Ok(())
//!     }
//! }
//!
//! let mut db = Db::default();
//! db.acquire(Begin, |_, txn| {
//!     txn.push(1);
//!     Ok::<_, String>(())
//! })
//! .unwrap();
//! let failed = db.acquire(Begin, |_, txn| {
//!     txn.push(2);
//!     Err::<(), _>("validation failed".to_owned())
//! });
//! assert!(failed.is_err());
//! assert_eq!(db.0, [1], "only the transaction that succeeded committed");
//! ```

use core::future::Future;

/// Something a capability can provide for the length of a scope: what to
/// open, connect, or spawn.
///
/// The provider decides how the resource is committed (`close`) and rolled
/// back (`abandon`); `Error` covers both opening and closing.
pub trait Acquisition {
    /// What the body of the scope borrows.
    type Resource;
    /// Why opening or closing failed.
    type Error;
}

/// Proof that a call to [`Provide::provide`] comes from a scope.
///
/// Only `seam-core` can create one, so only [`Acquire::acquire`] and
/// [`AsyncAcquire::acquire_async`] can call `provide`: a core cannot get a
/// resource it would then own.
#[derive(Debug)]
pub struct Token(());

/// The capability to provide resources of kind `A`, synchronously.
///
/// Cores bound on this and call [`acquire`](Acquire::acquire); backends
/// implement it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot provide `{A}`",
    label = "missing capability `Provide<{A}>`",
    note = "add a `Provide<{A}>` bound, or pass a value that implements it"
)]
pub trait Provide<A: Acquisition> {
    /// Opens the resource. Called only from a scope, which proves it with
    /// the token.
    fn provide(&mut self, acquisition: A, token: Token) -> Result<A::Resource, A::Error>;

    /// Commits the resource. Called when the scope's body succeeds.
    fn close(&mut self, resource: A::Resource) -> Result<(), A::Error>;

    /// Rolls the resource back. Called when the body fails or panics. The
    /// default drops it, which must have the same effect.
    fn abandon(resource: A::Resource) {
        drop(resource);
    }
}

/// The capability to provide resources of kind `A`, asynchronously.
///
/// Every `Send` [`Provide`] is also an `AsyncProvide`. Abandoning is always
/// synchronous, since it must also be what dropping does, and drop cannot
/// wait. The futures are `Send`, so scopes work in tasks on a multithreaded
/// runtime.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot provide `{A}` asynchronously",
    label = "missing capability `AsyncProvide<{A}>`",
    note = "add an `AsyncProvide<{A}>` bound, or pass a value that implements it"
)]
pub trait AsyncProvide<A: Acquisition> {
    /// Opens the resource. Called only from a scope.
    fn provide_async(
        &mut self,
        acquisition: A,
        token: Token,
    ) -> impl Future<Output = Result<A::Resource, A::Error>> + Send;

    /// Commits the resource. Called when the scope's body succeeds.
    fn close_async(
        &mut self,
        resource: A::Resource,
    ) -> impl Future<Output = Result<(), A::Error>> + Send;

    /// Rolls the resource back. Called when the body fails, panics, or is
    /// cancelled. The default drops it, which must have the same effect.
    fn abandon_async(resource: A::Resource) {
        drop(resource);
    }
}

impl<T, A> AsyncProvide<A> for T
where
    T: Provide<A> + Send,
    A: Acquisition + Send,
    A::Resource: Send,
{
    async fn provide_async(
        &mut self,
        acquisition: A,
        token: Token,
    ) -> Result<A::Resource, A::Error> {
        self.provide(acquisition, token)
    }

    async fn close_async(&mut self, resource: A::Resource) -> Result<(), A::Error> {
        self.close(resource)
    }

    fn abandon_async(resource: A::Resource) {
        T::abandon(resource);
    }
}

/// Uses a resource for the length of a closure. Implemented for every
/// [`Provide`], and only through that blanket impl, so a provider cannot
/// skip the commit or rollback.
pub trait Acquire<A: Acquisition>: Provide<A> {
    /// Opens `acquisition` and lends it to `body`, along with `self`.
    ///
    /// If `body` returns `Ok`, closes the resource and returns `body`'s value
    /// or the close error. If `body` returns `Err` or panics, abandons it.
    fn acquire<R, E>(
        &mut self,
        acquisition: A,
        body: impl FnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> Result<R, E>
    where
        E: From<A::Error>;
}

impl<T: Provide<A>, A: Acquisition> Acquire<A> for T {
    fn acquire<R, E>(
        &mut self,
        acquisition: A,
        body: impl FnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> Result<R, E>
    where
        E: From<A::Error>,
    {
        let resource = self.provide(acquisition, Token(()))?;
        let mut guard = Guard::new(resource, T::abandon);
        let out = body(self, guard.get());
        let resource = guard.disarm();
        match out {
            Ok(value) => {
                self.close(resource)?;
                Ok(value)
            }
            Err(e) => {
                T::abandon(resource);
                Err(e)
            }
        }
    }
}

/// Uses a resource for the length of an async closure. Implemented for every
/// [`AsyncProvide`], and only through that blanket impl.
///
/// Like [`Acquire`], plus cancellation: if the future is dropped before it
/// finishes, the resource is abandoned. The future is `Send` when the
/// capability, the resource and the body's future are.
pub trait AsyncAcquire<A: Acquisition>: AsyncProvide<A> {
    /// Opens `acquisition` and lends it to `body`, along with `self`.
    ///
    /// If `body` returns `Ok`, closes the resource and returns `body`'s value
    /// or the close error. If `body` returns `Err`, panics, or the returned
    /// future is dropped before it finishes, abandons it.
    fn acquire_async<R, E>(
        &mut self,
        acquisition: A,
        body: impl AsyncFnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> impl Future<Output = Result<R, E>>
    where
        E: From<A::Error>;
}

impl<T: AsyncProvide<A>, A: Acquisition> AsyncAcquire<A> for T {
    async fn acquire_async<R, E>(
        &mut self,
        acquisition: A,
        body: impl AsyncFnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> Result<R, E>
    where
        E: From<A::Error>,
    {
        let resource = self.provide_async(acquisition, Token(())).await?;
        let mut guard = Guard::new(resource, T::abandon_async);
        // Dropped here (a panic or a cancellation) abandons the resource.
        let out = body(self, guard.get()).await;
        let resource = guard.disarm();
        match out {
            Ok(value) => {
                // Dropped here, the resource is dropped inside `close_async`,
                // which by the provider's contract also abandons it.
                self.close_async(resource).await?;
                Ok(value)
            }
            Err(e) => {
                T::abandon_async(resource);
                Err(e)
            }
        }
    }
}

/// Holds a resource, and abandons it if dropped while still holding it,
/// which only happens when the body unwinds or is cancelled.
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
