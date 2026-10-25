//! Resources, which a core can only use inside a scope.
//!
//! Some effects produce something that lives for a while: an open file, a
//! connected socket, a running task. In seam a core never owns one of these.
//! It asks for the capability to [`Provide`] it, and then
//! [`acquire`](Provide::acquire)s it for the length of a closure:
//!
//! ```
//! # #[cfg(feature = "fs")] {
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::fs::{Open, ReadOnly};
//! use seam_core::ops::io::{Read, read_to_end};
//!
//! fn read_all<C, F>(c: &mut C, path: &str) -> io::Result<Vec<u8>>
//! where
//!     C: for<'a> Provide<Open<'a, ReadOnly, F>>,
//!     F: for<'a> Perform<Read<'a>>,
//! {
//!     c.acquire(Open::read(path), |_c, file| {
//!         let mut bytes = Vec::new();
//!         read_to_end(file, &mut bytes)?;
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
//! # A scope is two operations
//!
//! Opening and closing are ordinary operations, [`Opening`] and
//! [`Closing`], and a scope is the pair: [`Provide`] is the name for
//! `Perform<Opening<A>> + Perform<Closing<A>>`, and [`acquire`] performs one,
//! runs the body, then performs the other. So everything that works on
//! operations works on scopes too: the async forms, a [`Root`](crate::Root)
//! routing them to a part, and a wrapper that forwards every operation.
//!
//! A scope that changes nothing outside itself, such as reading a file, is
//! a query: [`Lend`] is `Inspect<Opening<A>> + Inspect<Closing<A>>`, and
//! [`lend`](Lend::lend) borrows the capability shared. What the body does to
//! the resource is local to the scope, the way a function can mutate its
//! own variables and still be pure.
//!
//! [`acquire`]: Provide::acquire
//!
//! # Commit on success, roll back otherwise
//!
//! When the body returns `Ok`, the scope performs [`Closing`], and the
//! provider **commits** the resource: it flushes and syncs a file, renames
//! an atomic file into place, shuts a stream down cleanly, or waits for a
//! task. An error from closing is returned from `acquire`. When the body
//! returns `Err`, panics, or (with
//! [`acquire_async`](AsyncProvide::acquire_async)) is cancelled by dropping
//! its future, the resource is **dropped** instead, and dropping rolls back.
//!
//! # Cancellation safety
//!
//! A provider must make dropping a resource leave things as they were: only
//! closing commits. A future can be dropped at any `.await`, including
//! halfway through opening or closing; whatever it held is dropped, and
//! dropping never commits half a result.
//!
//! # Implementing a provider
//!
//! Backends implement `Perform<Opening<A>>` and `Perform<Closing<A>>` (or
//! the async forms, or `Inspect` for a scope that is a query). An
//! [`Opening`] carries a [`Token`], which only this crate can create, so a
//! core cannot open a resource outside a scope. A provider can still pass
//! the token on to a provider it wraps.
//!
//! ```
//! use seam_core::{Acquisition, Closing, Opening, Perform, Provide};
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
//! impl Perform<Opening<Begin>> for Db {
//!     fn perform(&mut self, _: Opening<Begin>) -> Result<Vec<u32>, String> {
//!         Ok(Vec::new())
//!     }
//! }
//!
//! impl Perform<Closing<Begin>> for Db {
//!     fn perform(&mut self, Closing(pending, _): Closing<Begin>) -> Result<(), String> {
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

use core::fmt;
use core::future::Future;
use core::marker::PhantomData;

use crate::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform};
use crate::{Inspect, Operation, Perform, Query};

/// Something a capability can provide for the length of a scope: what to
/// open, connect, or spawn.
pub trait Acquisition {
    /// What the body of the scope borrows.
    type Resource;
    /// Why opening or closing failed.
    type Error;
}

/// Proof that an [`Opening`] comes from a scope.
///
/// Only `seam-core` can create one, so only a scope can open a resource: a
/// core cannot get a resource it would then own.
#[derive(Debug)]
pub struct Token(());

/// Open the resource `A` describes, for a scope. Produces the resource.
///
/// Only a scope can create one, since only this crate can create the
/// [`Token`]. A provider that opens by asking another one passes the token
/// on: `Opening(other, token)`.
pub struct Opening<A>(pub A, pub Token);

/// Commit a resource at the end of a scope that succeeded.
///
/// Only a scope ever holds a resource by value, so only a scope can close
/// one.
pub struct Closing<A: Acquisition>(pub A::Resource, pub PhantomData<fn() -> A>);

impl<A: Acquisition> Operation for Opening<A> {
    type Outcome = Result<A::Resource, A::Error>;
}

impl<A: Acquisition> Operation for Closing<A> {
    type Outcome = Result<(), A::Error>;
}

/// Whether opening is a query is up to the provider: it implements
/// `Inspect` for a scope that changes nothing outside itself.
impl<A: Acquisition> Query for Opening<A> {}
impl<A: Acquisition> Query for Closing<A> {}

impl<A: crate::Effect> crate::Effect for Opening<A> {
    type Key = A::Key;
}

impl<A: Acquisition + crate::Effect> crate::Effect for Closing<A> {
    type Key = A::Key;
}

impl<A: fmt::Debug> fmt::Debug for Opening<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Opening").field(&self.0).finish()
    }
}

impl<A: Acquisition> fmt::Debug for Closing<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Closing")
    }
}

/// Defines a scope trait: an alias for opening and closing `A`, with a
/// blanket impl, and the method that runs a scope.
macro_rules! scope_trait {
    // A bound only: its method is on `SendScope`.
    ($(#[$m:meta])* $name:ident: $cap:ident;) => {
        $(#[$m])*
        pub trait $name<A: Acquisition>: $cap<Opening<A>> + $cap<Closing<A>> {}

        impl<T: $cap<Opening<A>> + $cap<Closing<A>> + ?Sized, A: Acquisition> $name<A> for T {}
    };
    (
        $(#[$m:meta])*
        $name:ident: $cap:ident, $call:ident;
        $(#[$fm:meta])*
        fn $method:ident($($mut:ident)?) $(-> $fut:ident)?;
    ) => {
        $(#[$m])*
        pub trait $name<A: Acquisition>: $cap<Opening<A>> + $cap<Closing<A>> {
            scope_trait!(@sig [$(#[$fm])*] $method, [$($mut)?], [$($fut)?]);
        }

        impl<T: $cap<Opening<A>> + $cap<Closing<A>> + ?Sized, A: Acquisition> $name<A> for T {
            scope_trait!(@body $method, $call, [$($mut)?], [$($fut)?]);
        }
    };
    (@sig [$(#[$fm:meta])*] $method:ident, [$($mut:ident)?], []) => {
        $(#[$fm])*
        fn $method<R, E>(
            &$($mut)? self,
            acquisition: A,
            body: impl FnOnce(&$($mut)? Self, &mut A::Resource) -> Result<R, E>,
        ) -> Result<R, E>
        where
            E: From<A::Error>;
    };
    (@sig [$(#[$fm:meta])*] $method:ident, [$($mut:ident)?], [$fut:ident]) => {
        $(#[$fm])*
        fn $method<R, E>(
            &$($mut)? self,
            acquisition: A,
            body: impl AsyncFnOnce(&$($mut)? Self, &mut A::Resource) -> Result<R, E>,
        ) -> impl Future<Output = Result<R, E>>
        where
            E: From<A::Error>;
    };
    (@body $method:ident, $call:ident, [$($mut:ident)?], []) => {
        fn $method<R, E>(
            &$($mut)? self,
            acquisition: A,
            body: impl FnOnce(&$($mut)? Self, &mut A::Resource) -> Result<R, E>,
        ) -> Result<R, E>
        where
            E: From<A::Error>,
        {
            let mut resource = self.$call(Opening(acquisition, Token(())))?;
            // On an error or a panic the resource is dropped, which rolls
            // it back.
            let value = body(self, &mut resource)?;
            self.$call(Closing(resource, PhantomData))?;
            Ok(value)
        }
    };
    (@body $method:ident, $call:ident, [$($mut:ident)?], [$fut:ident]) => {
        async fn $method<R, E>(
            &$($mut)? self,
            acquisition: A,
            body: impl AsyncFnOnce(&$($mut)? Self, &mut A::Resource) -> Result<R, E>,
        ) -> Result<R, E>
        where
            E: From<A::Error>,
        {
            let mut resource = self.$call(Opening(acquisition, Token(()))).await?;
            // Dropped on an error, a panic or a cancellation, which rolls
            // it back.
            let value = body(self, &mut resource).await?;
            self.$call(Closing(resource, PhantomData)).await?;
            Ok(value)
        }
    };
}

scope_trait! {
    /// The capability to provide resources of kind `A`: to perform opening
    /// and closing them. Cores bound on this and call
    /// [`acquire`](Self::acquire); backends implement the two operations.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot provide `{A}`",
        label = "missing capability `Provide<{A}>`",
        note = "add a `Provide<{A}>` bound, or pass a value that implements it"
    )]
    Provide: Perform, perform;
    /// Opens `acquisition` and lends it to `body`, along with `self`. Closes
    /// it if `body` returns `Ok`; drops it otherwise, and on a panic.
    fn acquire(mut);
}

scope_trait! {
    /// [`Provide`], async.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot provide `{A}` asynchronously",
        label = "missing capability `AsyncProvide<{A}>`",
        note = "add an `AsyncProvide<{A}>` bound, or pass a value that implements it",
        note = "a sync `Provide<{A}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
    )]
    AsyncProvide: AsyncPerform, perform_async;
    /// [`acquire`](Provide::acquire), async. A cancelled scope drops the
    /// resource.
    fn acquire_async(mut) -> Future;
}

scope_trait! {
    /// [`Provide`], async, with `Send` futures: for multithreaded runtimes.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot provide `{A}` from a `Send` future",
        label = "missing capability `SendProvide<{A}>`",
        note = "add a `SendProvide<{A}>` bound, or pass a value that implements it",
        note = "a sync `Provide<{A}>` also works here if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
    )]
    SendProvide: SendPerform;
}

scope_trait! {
    /// The capability to lend resources of kind `A`: a scope that changes
    /// nothing outside itself, so it needs only shared access. Every
    /// [`Capability`](crate::Capability) that lends also provides.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot lend `{A}`",
        label = "missing capability `Lend<{A}>`",
        note = "add a `Lend<{A}>` bound, or pass a value that implements it"
    )]
    Lend: Inspect, inspect;
    /// Opens `acquisition` and lends it to `body`, along with `self`,
    /// shared. Closes it if `body` returns `Ok`; drops it otherwise.
    fn lend();
}

scope_trait! {
    /// [`Lend`], async.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot lend `{A}` asynchronously",
        label = "missing capability `AsyncLend<{A}>`",
        note = "add an `AsyncLend<{A}>` bound, or pass a value that implements it"
    )]
    AsyncLend: AsyncInspect, inspect_async;
    /// [`lend`](Lend::lend), async.
    fn lend_async() -> Future;
}

scope_trait! {
    /// [`Lend`], async, with `Send` futures: for multithreaded runtimes.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot lend `{A}` from a `Send` future",
        label = "missing capability `SendLend<{A}>`",
        note = "add a `SendLend<{A}>` bound, or pass a value that implements it"
    )]
    SendLend: SendInspect;
}

/// The scopes of [`SendProvide`] and [`SendLend`]. Implemented for every
/// type; in the prelude.
///
/// The methods are here, and not on those traits, so that generic code can
/// tell their futures are `Send`. A method of a trait in a function's
/// bounds is resolved through the bound, which hides the future's type; one
/// of this trait, which nothing names in its bounds, resolves to the impl
/// below, whose future is `Send` whenever what it holds is.
pub trait SendScope {
    /// [`acquire`](Provide::acquire), async, opening and closing through
    /// `Send` futures.
    fn acquire_send<A: Acquisition, R, E>(
        &mut self,
        acquisition: A,
        body: impl AsyncFnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> impl Future<Output = Result<R, E>>
    where
        Self: SendProvide<A>,
        E: From<A::Error>;

    /// [`lend`](Lend::lend), async, through `Send` futures.
    fn lend_send<A: Acquisition, R, E>(
        &self,
        acquisition: A,
        body: impl AsyncFnOnce(&Self, &mut A::Resource) -> Result<R, E>,
    ) -> impl Future<Output = Result<R, E>>
    where
        Self: SendLend<A>,
        E: From<A::Error>;
}

impl<T: ?Sized> SendScope for T {
    async fn acquire_send<A: Acquisition, R, E>(
        &mut self,
        acquisition: A,
        body: impl AsyncFnOnce(&mut Self, &mut A::Resource) -> Result<R, E>,
    ) -> Result<R, E>
    where
        Self: SendProvide<A>,
        E: From<A::Error>,
    {
        let mut resource = self.perform_send(Opening(acquisition, Token(()))).await?;
        let value = body(self, &mut resource).await?;
        self.perform_send(Closing(resource, PhantomData)).await?;
        Ok(value)
    }

    async fn lend_send<A: Acquisition, R, E>(
        &self,
        acquisition: A,
        body: impl AsyncFnOnce(&Self, &mut A::Resource) -> Result<R, E>,
    ) -> Result<R, E>
    where
        Self: SendLend<A>,
        E: From<A::Error>,
    {
        let mut resource = self.inspect_send(Opening(acquisition, Token(()))).await?;
        let value = body(self, &mut resource).await?;
        self.inspect_send(Closing(resource, PhantomData)).await?;
        Ok(value)
    }
}
