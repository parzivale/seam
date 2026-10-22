//! Roots: one capability built from parts.
//!
//! A program's edge builds a [`Root`] out of parts, each serving one or
//! more *kinds* of operation, and hands it to the core. Every operation
//! names its kind ([`InKind`]): `Sleep` and `Now` are [`kinds::Time`],
//! `ReadFile` is [`kinds::Fs`], and so on. The root forwards each operation
//! to the part that serves its kind, in whichever form (sync, async,
//! `Send`) that part implements it. So one root can mix backends, sync and
//! async parts, simulated parts and a program's own:
//!
//! ```
//! # #[cfg(feature = "time")] {
//! use seam_core::kinds;
//! use seam_core::ops::time::{Instant, Now, Sleep};
//! use seam_core::{Capability, InKind, Inspect, Operation, Perform, Query};
//!
//! // A program's own kind of operation: a plain marker type.
//! struct Greeting;
//! struct Greet(&'static str);
//! impl Operation for Greet {
//!     type Outcome = String;
//! }
//! impl Query for Greet {}
//! impl InKind for Greet {
//!     type Kind = Greeting;
//! }
//!
//! // Two parts: a fake clock, and a greeter.
//! struct Clock(Instant);
//! impl Capability for Clock {}
//! impl Inspect<Now> for Clock {
//!     fn inspect(&self, _: Now) -> Instant {
//!         self.0
//!     }
//! }
//! impl Perform<Sleep> for Clock {
//!     fn perform(&mut self, op: Sleep) {
//!         self.0 += op.0;
//!     }
//! }
//! struct Polite;
//! impl Capability for Polite {}
//! impl Inspect<Greet> for Polite {
//!     fn inspect(&self, op: Greet) -> String {
//!         format!("good day, {}", op.0)
//!     }
//! }
//!
//! // The root: one field per part, each with the kinds it serves.
//! let mut root = seam_core::root! {
//!     clock: Clock(Instant::EPOCH) => [kinds::Time],
//!     greeter: Polite => [Greeting],
//! };
//!
//! /// The core asks for exactly what it uses, as always.
//! fn core<C: Inspect<Greet> + Perform<Sleep>>(c: &mut C) -> String {
//!     c.perform(Sleep(core::time::Duration::from_secs(1)));
//!     c.inspect(Greet("ada"))
//! }
//!
//! assert_eq!(core(&mut root), "good day, ada");
//! assert_eq!(root.parts().clock.0.since_epoch().as_secs(), 1);
//! # }
//! ```
//!
//! A root lists each kind once: two parts for the same kind fail to
//! compile, with "conflicting implementations of `Pick<App>`":
//!
//! ```compile_fail,E0119
//! # #[cfg(feature = "time")] {
//! # use seam_core::kinds;
//! # struct A; struct B;
//! let root = seam_core::root! {
//!     a: A => [kinds::Time],
//!     b: B => [kinds::Time], // error: `kinds::Time` is already served
//! };
//! # }
//! # #[cfg(not(feature = "time"))] compile_error!("E0119");
//! ```
//!
//! An operation whose kind the root has no part for is a missing
//! capability, like any other: "the root `App` has no part for
//! `kinds::Fs`".
//!
//! [`root!`](crate::root!) builds the root: a struct of the parts, with one
//! [`Pick`] impl per kind saying which field serves it. It is only sugar;
//! a root is any struct with those impls, wrapped in [`Root::new`].
//!
//! Operations on handles (`Read` on a stream, `RecvMsg` on a channel,
//! `Join` on a task) never go through the root, so they have no kind: the
//! handle a part hands out implements them itself.

use crate::acquire::{Acquisition, AsyncProvide, Provide, Token};
use crate::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform};
use crate::{Inspect, Operation, Perform, Query};

/// The kinds of seam's own operations. A program's own kinds are plain
/// marker types too.
pub mod kinds {
    /// `Var`, `Args`, `CurrentDir`, `TempDir`.
    #[derive(Debug, Clone, Copy)]
    pub struct Env;
    /// The filesystem operations: `ReadFile`, `WriteFile`, `Open`, ...
    #[derive(Debug, Clone, Copy)]
    pub struct Fs;
    /// `Resolve`, `TcpConnect`, `TcpBind`, `UdpBind`.
    #[derive(Debug, Clone, Copy)]
    pub struct Net;
    /// `RunCommand`, `Exit`.
    #[derive(Debug, Clone, Copy)]
    pub struct Process;
    /// `RandomU64`, `FillRandom`.
    #[derive(Debug, Clone, Copy)]
    pub struct Random;
    /// `CtrlC`.
    #[derive(Debug, Clone, Copy)]
    pub struct Signal;
    /// `Print`, `EPrint`, `ReadLine`.
    #[derive(Debug, Clone, Copy)]
    pub struct Stdio;
    /// `Now`, `WallClock`, `Sleep`.
    #[derive(Debug, Clone, Copy)]
    pub struct Time;
    /// `Spawn`, `SpawnSet`, `YieldNow`, `AvailableParallelism`.
    #[derive(Debug, Clone, Copy)]
    pub struct Task;
    /// `Channel`, `Share`. Named by path (`kinds::Sync`), since importing
    /// it would shadow `core::marker::Sync`.
    #[derive(Debug, Clone, Copy)]
    pub struct Sync;
}

/// Names the kind of operation (or acquisition) this is, which decides
/// which part of a [`Root`] serves it.
pub trait InKind {
    /// The kind: a marker type, such as [`kinds::Fs`].
    type Kind;
}

/// Implemented on a kind, for a root's struct `A`: which part of `A` serves
/// that kind. [`root!`](crate::root!) writes these.
#[diagnostic::on_unimplemented(
    message = "the root `{A}` has no part for `{Self}`",
    label = "no part serves `{Self}`",
    note = "add a part that serves `{Self}` to the root"
)]
pub trait Pick<A> {
    /// The part.
    type Part;

    /// The part, shared.
    fn pick(parts: &A) -> &Self::Part;

    /// The part, exclusively.
    fn pick_mut(parts: &mut A) -> &mut Self::Part;
}

/// A capability built from parts: forwards each operation to the part that
/// serves its kind. See the [module docs](mod@crate::root).
#[derive(Debug, Clone, Copy, Default)]
pub struct Root<A>(A);

impl<A> Root<A> {
    /// Builds a root from its parts.
    pub const fn new(parts: A) -> Self {
        Self(parts)
    }

    /// The parts, for setting up and inspecting them at the edge.
    pub fn parts(&self) -> &A {
        &self.0
    }

    /// The parts, exclusively.
    pub fn parts_mut(&mut self) -> &mut A {
        &mut self.0
    }

    /// Takes the parts back.
    pub fn into_parts(self) -> A {
        self.0
    }
}

/// The part of `A` that serves `O`.
type PartOf<O, A> = <<O as InKind>::Kind as Pick<A>>::Part;

impl<A, Q> Inspect<Q> for Root<A>
where
    Q: Query + InKind,
    Q::Kind: Pick<A>,
    PartOf<Q, A>: Inspect<Q>,
{
    fn inspect(&self, query: Q) -> Q::Outcome {
        <Q::Kind as Pick<A>>::pick(&self.0).inspect(query)
    }
}

impl<A, O> Perform<O> for Root<A>
where
    O: Operation + InKind,
    O::Kind: Pick<A>,
    PartOf<O, A>: Perform<O>,
{
    fn perform(&mut self, op: O) -> O::Outcome {
        <O::Kind as Pick<A>>::pick_mut(&mut self.0).perform(op)
    }
}

impl<A, Q> AsyncInspect<Q> for Root<A>
where
    Q: Query + InKind,
    Q::Kind: Pick<A>,
    PartOf<Q, A>: AsyncInspect<Q>,
{
    async fn inspect_async(&self, query: Q) -> Q::Outcome {
        <Q::Kind as Pick<A>>::pick(&self.0)
            .inspect_async(query)
            .await
    }
}

impl<A, O> AsyncPerform<O> for Root<A>
where
    O: Operation + InKind,
    O::Kind: Pick<A>,
    PartOf<O, A>: AsyncPerform<O>,
{
    async fn perform_async(&mut self, op: O) -> O::Outcome {
        <O::Kind as Pick<A>>::pick_mut(&mut self.0)
            .perform_async(op)
            .await
    }
}

// The `Send` forms hand back the part's own future, which is `Send`; that
// needs the part to outlive the borrow, so parts behind them are `'static`.

impl<A, Q> SendInspect<Q> for Root<A>
where
    Q: Query + InKind,
    Q::Kind: Pick<A>,
    PartOf<Q, A>: SendInspect<Q> + 'static,
{
    fn inspect_send(&self, query: Q) -> impl Future<Output = Q::Outcome> + Send {
        <Q::Kind as Pick<A>>::pick(&self.0).inspect_send(query)
    }
}

impl<A, O> SendPerform<O> for Root<A>
where
    O: Operation + InKind,
    O::Kind: Pick<A>,
    PartOf<O, A>: SendPerform<O> + 'static,
{
    fn perform_send(&mut self, op: O) -> impl Future<Output = O::Outcome> + Send {
        <O::Kind as Pick<A>>::pick_mut(&mut self.0).perform_send(op)
    }
}

impl<A, X> Provide<X> for Root<A>
where
    X: Acquisition + InKind,
    X::Kind: Pick<A>,
    PartOf<X, A>: Provide<X>,
{
    fn provide(&mut self, acquisition: X, token: Token) -> Result<X::Resource, X::Error> {
        <X::Kind as Pick<A>>::pick_mut(&mut self.0).provide(acquisition, token)
    }

    fn close(&mut self, resource: X::Resource) -> Result<(), X::Error> {
        Provide::<X>::close(<X::Kind as Pick<A>>::pick_mut(&mut self.0), resource)
    }

    fn abandon(resource: X::Resource) {
        <PartOf<X, A> as Provide<X>>::abandon(resource);
    }
}

impl<A, X> AsyncProvide<X> for Root<A>
where
    X: Acquisition + InKind,
    X::Kind: Pick<A>,
    PartOf<X, A>: AsyncProvide<X> + 'static,
{
    fn provide_async(
        &mut self,
        acquisition: X,
        token: Token,
    ) -> impl Future<Output = Result<X::Resource, X::Error>> + Send {
        <X::Kind as Pick<A>>::pick_mut(&mut self.0).provide_async(acquisition, token)
    }

    fn close_async(
        &mut self,
        resource: X::Resource,
    ) -> impl Future<Output = Result<(), X::Error>> + Send {
        AsyncProvide::<X>::close_async(<X::Kind as Pick<A>>::pick_mut(&mut self.0), resource)
    }

    fn abandon_async(resource: X::Resource) {
        <PartOf<X, A> as AsyncProvide<X>>::abandon_async(resource);
    }
}

/// Builds a root from its parts: one field per part, each with the kinds
/// it serves. It evaluates to the [`Root`], ready to hand to a core:
///
/// ```
/// # #[cfg(feature = "time")] {
/// # use seam_core::{kinds, Capability, Inspect, Perform};
/// # use seam_core::ops::time::{Instant, Now, Sleep};
/// # #[derive(Clone, Default)] pub struct Clock;
/// # impl Capability for Clock {}
/// # impl Inspect<Now> for Clock { fn inspect(&self, _: Now) -> Instant { Instant::EPOCH } }
/// # impl Perform<Sleep> for Clock { fn perform(&mut self, _: Sleep) {} }
/// let mut root = seam_core::root! {
///     clock: Clock => [kinds::Time],
/// };
/// root.perform(Sleep(core::time::Duration::from_secs(1)));
/// let _clock: &Clock = &root.parts().clock;
/// # }
/// ```
///
/// One part may serve several kinds (`std: Std::new() => [kinds::Env,
/// kinds::Fs]`); each kind is served by one part. The root's struct is
/// generated, and is `Clone`, `Copy`, `Debug` and `Default` whenever its
/// parts are; its fields are named as given.
///
/// To name the root's type (to store it in a struct, say), define the
/// struct instead, and wrap a value of it in [`Root::new`]:
///
/// ```
/// # #[cfg(feature = "time")] {
/// # use seam_core::{kinds, Capability, Inspect, Perform};
/// # use seam_core::ops::time::{Instant, Now, Sleep};
/// # #[derive(Clone, Default)] pub struct Clock;
/// # impl Capability for Clock {}
/// # impl Inspect<Now> for Clock { fn inspect(&self, _: Now) -> Instant { Instant::EPOCH } }
/// seam_core::root! {
///     #[derive(Clone, Default)]
///     pub struct App {
///         pub clock: Clock => [kinds::Time],
///     }
/// }
/// let root: seam_core::Root<App> = seam_core::Root::new(App::default());
/// # let _ = root.inspect(Now);
/// # }
/// ```
#[macro_export]
macro_rules! root {
    // A named struct.
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($fvis:vis $field:ident : $part:ty => [$($kind:path),+ $(,)?]),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $($fvis $field: $part),*
        }

        $($(
            impl $crate::Pick<$name> for $kind {
                type Part = $part;
                fn pick(parts: &$name) -> &$part {
                    &parts.$field
                }
                fn pick_mut(parts: &mut $name) -> &mut $part {
                    &mut parts.$field
                }
            }
        )+)*
    };

    // A root value: the struct is generic over its parts' types, so they
    // are inferred from the values.
    ($($field:ident : $value:expr => [$($kind:path),+ $(,)?]),+ $(,)?) => {{
        #[derive(Clone, Copy, Debug, Default)]
        #[allow(non_camel_case_types)]
        struct Parts<$($field),+> {
            $($field: $field),+
        }
        $crate::root!(@picks [$($field),+] $($field [$($kind),+])+);
        $crate::Root::new(Parts { $($field: $value),+ })
    }};
    (@picks $all:tt $($field:ident [$($kind:path),+])+) => {
        $($($crate::root!(@pick $all $field $kind);)+)+
    };
    (@pick [$($all:ident),+] $field:ident $kind:path) => {
        #[allow(non_camel_case_types)]
        impl<$($all),+> $crate::Pick<Parts<$($all),+>> for $kind {
            type Part = $field;
            fn pick(parts: &Parts<$($all),+>) -> &$field {
                &parts.$field
            }
            fn pick_mut(parts: &mut Parts<$($all),+>) -> &mut $field {
                &mut parts.$field
            }
        }
    };
}

/// Implements [`InKind`] for operations.
#[allow(unused_macros)] // used by whichever ops modules are on
macro_rules! in_kind {
    ($kind:ty: $([$($gen:tt)*] $op:ty),* $(,)?) => {$(
        impl<$($gen)*> $crate::InKind for $op {
            type Kind = $kind;
        }
    )*};
}
#[allow(unused_imports)]
pub(crate) use in_kind;
