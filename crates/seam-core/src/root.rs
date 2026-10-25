//! Roots: one capability built from parts.
//!
//! A program's edge builds a [`Root`] out of parts, and lists, for each
//! part, the effects it serves. Every operation names its effect by a
//! marker type, its key ([`Effect`]). seam's keys are all in [`effects`]:
//! `Now` routes by `effects::Now`, `Open` in mode `M` by `effects::Open<M>`,
//! and so on. The root forwards each operation to the part listed for its
//! effect, in whichever form (sync, async, `Send`) that part implements it.
//! So one root can mix backends, sync and async parts, simulated parts and
//! a program's own, and grants the core exactly the effects it lists:
//!
//! ```
//! # #[cfg(feature = "time")] {
//! use seam_core::effects;
//! use seam_core::ops::time::{Instant, Now, Sleep};
//! use seam_core::{Capability, Effect, Inspect, Operation, Perform, Query};
//!
//! // A program's own effect, which is its own key.
//! struct Greet(&'static str);
//! impl Operation for Greet {
//!     type Outcome = String;
//! }
//! impl Query for Greet {}
//! impl Effect for Greet {
//!     type Key = Greet;
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
//! // The root: each part, with the effects it serves.
//! let mut root = seam_core::root![
//!     Clock<effects::Sleep>(Instant::EPOCH),
//!     Polite<Greet>,
//! ];
//!
//! /// The core asks for exactly what it uses, as always.
//! fn core<C: Inspect<Greet> + Perform<Sleep>>(c: &mut C) -> String {
//!     c.perform(Sleep(core::time::Duration::from_secs(1)));
//!     c.inspect(Greet("ada"))
//! }
//!
//! assert_eq!(core(&mut root), "good day, ada");
//! assert_eq!(root.part::<effects::Sleep>().0.since_epoch().as_secs(), 1);
//! # }
//! ```
//!
//! The clock could also tell the time, but this root does not list
//! `effects::Now`, so a core given it cannot ask: "the root `Parts<..>` has
//! no part for `effects::Now`".
//!
//! ```compile_fail,E0277
//! # #[cfg(feature = "time")] {
//! # use seam_core::{effects, Capability, Inspect, Perform};
//! # use seam_core::ops::time::{Instant, Now, Sleep};
//! # struct Clock;
//! # impl Capability for Clock {}
//! # impl Inspect<Now> for Clock { fn inspect(&self, _: Now) -> Instant { Instant::EPOCH } }
//! # impl Perform<Sleep> for Clock { fn perform(&mut self, _: Sleep) {} }
//! let root = seam_core::root![Clock<effects::Sleep>];
//! root.inspect(Now); // error: the root has no part for `effects::Now`
//! # }
//! # #[cfg(not(feature = "time"))] compile_error!("E0277");
//! ```
//!
//! A root lists each effect once: two parts for the same effect fail to
//! compile, with "conflicting implementations of `Pick<App>`":
//!
//! ```compile_fail,E0119
//! # #[cfg(feature = "time")] {
//! # use seam_core::effects;
//! # struct A; struct B;
//! let root = seam_core::root![
//!     A<effects::Now>,
//!     B<effects::Now>, // error: `effects::Now` is already served
//! ];
//! # }
//! # #[cfg(not(feature = "time"))] compile_error!("E0119");
//! ```
//!
//! A scope routes by its acquisition: listing `effects::View` serves
//! `Opening<View<..>>` and `Closing<View<..>>`. A timed operation routes as
//! the operation does.
//!
//! [`root!`](crate::root!) builds the root: a struct of the parts, with one
//! [`Pick`] impl per effect saying which field serves it. It is only sugar;
//! a root is any struct with those impls, wrapped in [`Root::new`].
//!
//! Operations on handles (`Read` on a stream, `RecvMsg` on a channel,
//! `Join` on a task) never go through the root, so they have no key: the
//! handle a part hands out implements them itself.

use crate::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform};
use crate::{Inspect, Operation, Perform, Query};

/// The keys of seam's own effects, one per operation (or acquisition) a
/// root serves, named after it. List them in [`root!`](crate::root!):
///
/// ```text
/// use seam::effects as fx;
/// seam::root![Std<fx::Args, fx::View, fx::OpenStdout>]
/// ```
///
/// A program's own effect has its own key, usually the operation type
/// itself.
pub mod effects {
    #[allow(unused_imports)]
    use core::marker::PhantomData;

    macro_rules! keys {
        ($feature:literal: $($name:ident => $op:path),* $(,)?) => {$(
            #[doc = concat!("Routes [`", stringify!($op), "`](crate::ops::", stringify!($op), ").")]
            #[cfg(feature = $feature)]
            #[derive(Debug, Clone, Copy)]
            pub struct $name;
        )*};
    }

    keys!("env": Var => env::Var, SetVar => env::SetVar, RemoveVar => env::RemoveVar, Args => env::Args, CurrentDir => env::CurrentDir, TempDir => env::TempDir);
    keys!("fs": Metadata => fs::Metadata, CreateDir => fs::CreateDir, RemoveFile => fs::RemoveFile, RemoveDir => fs::RemoveDir, Rename => fs::Rename, CopyFile => fs::CopyFile, CreateAtomic => fs::CreateAtomic, View => fs::View, ReadDir => fs::ReadDir);
    keys!("net": ResolveInto => net::ResolveInto, TcpConnect => net::TcpConnect, TcpBind => net::TcpBind, UdpBind => net::UdpBind);
    keys!("process": RunCommand => process::RunCommand, SpawnChild => process::SpawnChild, Exit => process::Exit);
    keys!("random": FillRandom => random::FillRandom);
    keys!("signal": CtrlC => signal::CtrlC);
    keys!("io-std": OpenStdin => stdio::OpenStdin, OpenStdout => stdio::OpenStdout, OpenStderr => stdio::OpenStderr);
    keys!("time": Now => time::Now, Sleep => time::Sleep);
    keys!("rt": Spawn => task::Spawn, SpawnSet => task::SpawnSet, YieldNow => task::YieldNow, AvailableParallelism => task::AvailableParallelism);
    keys!("sync": Channel => sync::Channel, Share => sync::Share);

    /// Routes [`time::WallClock`](crate::ops::time::WallClock).
    #[cfg(all(feature = "time", feature = "std"))]
    #[derive(Debug, Clone, Copy)]
    pub struct WallClock;

    /// Routes [`fs::Open`](crate::ops::fs::Open) in mode `M`: a part that
    /// serves only `Open<ReadOnly>` cannot open a file to write it.
    #[cfg(feature = "fs")]
    #[derive(Debug, Clone, Copy)]
    pub struct Open<M>(PhantomData<fn() -> M>);
}

/// Names the effect an operation (or acquisition) is, by a marker type,
/// its key, which decides which part of a [`Root`] serves it.
///
/// A program's own effect is usually its own key:
/// `impl Effect for Greet { type Key = Greet; }`.
pub trait Effect {
    /// The key: seam's are in [`effects`].
    type Key;
}

/// Implemented on an effect's key, for a root's struct `A`: which part of
/// `A` serves that effect. [`root!`](crate::root!) writes these.
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

/// A capability built from parts: forwards each operation to the part
/// listed for its effect. See the [module docs](mod@crate::root).
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

    /// The part serving the effect `K`, for inspecting it at the edge:
    /// `root.part::<BindMount>()`.
    pub fn part<K: Pick<A>>(&self) -> &K::Part {
        K::pick(&self.0)
    }

    /// The part serving the effect `K`, exclusively.
    pub fn part_mut<K: Pick<A>>(&mut self) -> &mut K::Part {
        K::pick_mut(&mut self.0)
    }
}

/// The part of `A` that serves `O`.
type PartOf<O, A> = <<O as Effect>::Key as Pick<A>>::Part;

impl<A, Q> Inspect<Q> for Root<A>
where
    Q: Query + Effect,
    Q::Key: Pick<A>,
    PartOf<Q, A>: Inspect<Q>,
{
    fn inspect(&self, query: Q) -> Q::Outcome {
        <Q::Key as Pick<A>>::pick(&self.0).inspect(query)
    }
}

impl<A, O> Perform<O> for Root<A>
where
    O: Operation + Effect,
    O::Key: Pick<A>,
    PartOf<O, A>: Perform<O>,
{
    fn perform(&mut self, op: O) -> O::Outcome {
        <O::Key as Pick<A>>::pick_mut(&mut self.0).perform(op)
    }
}

impl<A, Q> AsyncInspect<Q> for Root<A>
where
    Q: Query + Effect,
    Q::Key: Pick<A>,
    PartOf<Q, A>: AsyncInspect<Q>,
{
    async fn inspect_async(&self, query: Q) -> Q::Outcome {
        <Q::Key as Pick<A>>::pick(&self.0)
            .inspect_async(query)
            .await
    }
}

impl<A, O> AsyncPerform<O> for Root<A>
where
    O: Operation + Effect,
    O::Key: Pick<A>,
    PartOf<O, A>: AsyncPerform<O>,
{
    async fn perform_async(&mut self, op: O) -> O::Outcome {
        <O::Key as Pick<A>>::pick_mut(&mut self.0)
            .perform_async(op)
            .await
    }
}

// The `Send` forms hand back the part's own future, which is `Send`; that
// needs the part to outlive the borrow, so parts behind them are `'static`.

impl<A, Q> SendInspect<Q> for Root<A>
where
    Q: Query + Effect,
    Q::Key: Pick<A>,
    PartOf<Q, A>: SendInspect<Q> + 'static,
{
    fn inspect_send(&self, query: Q) -> impl Future<Output = Q::Outcome> + Send {
        <Q::Key as Pick<A>>::pick(&self.0).inspect_send(query)
    }
}

impl<A, O> SendPerform<O> for Root<A>
where
    O: Operation + Effect,
    O::Key: Pick<A>,
    PartOf<O, A>: SendPerform<O> + 'static,
{
    fn perform_send(&mut self, op: O) -> impl Future<Output = O::Outcome> + Send {
        <O::Key as Pick<A>>::pick_mut(&mut self.0).perform_send(op)
    }
}

/// Builds a root from its parts. It evaluates to the [`Root`], ready to
/// hand to a core:
///
/// ```
/// # #[cfg(feature = "time")] {
/// # use seam_core::{effects, Capability, Inspect, Perform};
/// # use seam_core::ops::time::{Instant, Now, Sleep};
/// # #[derive(Clone, Default)] pub struct Clock;
/// # impl Capability for Clock {}
/// # impl Inspect<Now> for Clock { fn inspect(&self, _: Now) -> Instant { Instant::EPOCH } }
/// # impl Perform<Sleep> for Clock { fn perform(&mut self, _: Sleep) {} }
/// let mut root = seam_core::root![Clock<effects::Now, effects::Sleep>];
/// root.perform(Sleep(core::time::Duration::from_secs(1)));
/// let _clock: &Clock = root.part::<effects::Now>();
/// # }
/// ```
///
/// Each part is written as Rust builds a value of its type, with the effects
/// it serves in brackets after the type's name:
///
/// ```text
/// seam::root![
///     Std<fx::Args, fx::View>,                        // a unit struct
///     Pool<fx::Query>(4),                             // a tuple struct: Pool(4)
///     FileStore<Get, Put> { fs: sim.clone(), path },  // a struct literal
///     Sim<fx::Now, fx::Sleep>::from_world(WORLD)?,    // one of the type's constructors, with `?`
/// ]
/// ```
///
/// The brackets name the effects a part serves, and the root grants only
/// those, whatever else the part could do. Inside `root!`, `Name<..>`
/// always lists effects, so a part whose type is generic is built by a
/// constructor or a struct literal of that type. Each effect is served by
/// one part; a root has at most 32 parts. Its struct is generated, and is
/// `Clone`, `Copy`, `Debug` and `Default` whenever its parts are.
///
/// To name the root's type (to store it in a struct, say), define the
/// struct instead, and wrap a value of it in [`Root::new`]:
///
/// ```
/// # #[cfg(feature = "time")] {
/// # use seam_core::{effects, Capability, Inspect, Perform};
/// # use seam_core::ops::time::{Instant, Now, Sleep};
/// # #[derive(Clone, Default)] pub struct Clock;
/// # impl Capability for Clock {}
/// # impl Inspect<Now> for Clock { fn inspect(&self, _: Now) -> Instant { Instant::EPOCH } }
/// seam_core::root! {
///     #[derive(Clone, Default)]
///     pub struct App {
///         pub clock: Clock<effects::Now>,
///     }
/// }
/// let root: seam_core::Root<App> = seam_core::Root::new(App::default());
/// # let _ = root.inspect(Now);
/// # }
/// ```
#[macro_export]
macro_rules! root {
    // A named struct: each field a part, with the effects it serves.
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($fvis:vis $field:ident : $($seg:ident)::+ < $($key:ty),+ $(,)? >),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $($fvis $field: $($seg)::+),*
        }

        $($crate::root!(@named $name $field ($($seg)::+) [$($key),+]);)*
    };
    (@named $name:ident $field:ident ($part:ty) [$($key:ty),+]) => {
        $(
            impl $crate::Pick<$name> for $key {
                type Part = $part;
                fn pick(parts: &$name) -> &$part {
                    &parts.$field
                }
                fn pick_mut(parts: &mut $name) -> &mut $part {
                    &mut parts.$field
                }
            }
        )+
    };

    // A root value: a list of parts, each `Name<Keys>`, built from what
    // follows. The struct is generic over the parts' types, so they are
    // inferred from the values; its type parameters and fields are named
    // from the lists below, not after anything the caller wrote.
    (@munch [$($done:tt)*] [$p:ident $($ps:ident)*] [$f:ident $($fs:ident)*]
        $($seg:ident)::+ < $($key:ty),+ $(,)? > ($($args:tt)*) $(, $($rest:tt)*)?) => {
        $crate::root!(@munch [$($done)* ($f $p ($($seg)::+($($args)*)) [$($key),+])]
            [$($ps)*] [$($fs)*] $($($rest)*)?)
    };
    (@munch [$($done:tt)*] [$p:ident $($ps:ident)*] [$f:ident $($fs:ident)*]
        $($seg:ident)::+ < $($key:ty),+ $(,)? > { $($fields:tt)* } $(, $($rest:tt)*)?) => {
        $crate::root!(@munch [$($done)* ($f $p ($($seg)::+ { $($fields)* }) [$($key),+])]
            [$($ps)*] [$($fs)*] $($($rest)*)?)
    };
    (@munch [$($done:tt)*] [$p:ident $($ps:ident)*] [$f:ident $($fs:ident)*]
        $($seg:ident)::+ < $($key:ty),+ $(,)? > :: $ctor:ident ($($args:tt)*) ? $(, $($rest:tt)*)?) => {
        $crate::root!(@munch [$($done)* ($f $p ($($seg)::+::$ctor($($args)*)?) [$($key),+])]
            [$($ps)*] [$($fs)*] $($($rest)*)?)
    };
    (@munch [$($done:tt)*] [$p:ident $($ps:ident)*] [$f:ident $($fs:ident)*]
        $($seg:ident)::+ < $($key:ty),+ $(,)? > :: $ctor:ident ($($args:tt)*) $(, $($rest:tt)*)?) => {
        $crate::root!(@munch [$($done)* ($f $p ($($seg)::+::$ctor($($args)*)) [$($key),+])]
            [$($ps)*] [$($fs)*] $($($rest)*)?)
    };
    (@munch [$($done:tt)*] [$p:ident $($ps:ident)*] [$f:ident $($fs:ident)*]
        $($seg:ident)::+ < $($key:ty),+ $(,)? > $(, $($rest:tt)*)?) => {
        $crate::root!(@munch [$($done)* ($f $p ($($seg)::+) [$($key),+])]
            [$($ps)*] [$($fs)*] $($($rest)*)?)
    };
    (@munch [$(($f:ident $p:ident ($value:expr) [$($key:ty),+]))+] $ps:tt $fs:tt) => {{
        #[derive(Clone, Copy, Debug, Default)]
        struct Parts<$($p),+> {
            $($f: $p),+
        }
        $crate::root!(@picks [$($p),+] $($f $p [$($key),+])+);
        $crate::Root::new(Parts { $($f: $value),+ })
    }};

    // Anything else is not a part.
    (@munch $done:tt $ps:tt $fs:tt $($bad:tt)+) => {
        ::core::compile_error!(::core::concat!(
            "expected a part, written as its type builds: `Name<Keys>`, `Name<Keys>(..)`, ",
            "`Name<Keys> { .. }` or `Name<Keys>::constructor(..)`; found `",
            ::core::stringify!($($bad)+), "`"
        ))
    };

    (@picks $all:tt $($f:ident $p:ident [$($key:ty),+])+) => {
        $($($crate::root!(@pick $all $f $p $key);)+)+
    };
    (@pick [$($all:ident),+] $f:ident $p:ident $key:ty) => {
        impl<$($all),+> $crate::Pick<Parts<$($all),+>> for $key {
            type Part = $p;
            fn pick(parts: &Parts<$($all),+>) -> &$p {
                &parts.$f
            }
            fn pick_mut(parts: &mut Parts<$($all),+>) -> &mut $p {
                &mut parts.$f
            }
        }
    };

    ($($items:tt)+) => {
        $crate::root!(@munch []
            [P0 P1 P2 P3 P4 P5 P6 P7 P8 P9 P10 P11 P12 P13 P14 P15 P16 P17 P18 P19 P20 P21 P22 P23 P24 P25 P26 P27 P28 P29 P30 P31]
            [__seam0 __seam1 __seam2 __seam3 __seam4 __seam5 __seam6 __seam7 __seam8 __seam9 __seam10 __seam11 __seam12 __seam13 __seam14 __seam15 __seam16 __seam17 __seam18 __seam19 __seam20 __seam21 __seam22 __seam23 __seam24 __seam25 __seam26 __seam27 __seam28 __seam29 __seam30 __seam31]
            $($items)+)
    };
}

/// Implements [`Effect`] for operations: `keys!(['a] Var<'a> => Var, ...)`,
/// the key named in [`effects`].
#[allow(unused_macros)] // used by whichever ops modules are on
macro_rules! keys {
    ($([$($gen:tt)*] $op:ty => $key:ident $(<$($kg:ty),*>)?),* $(,)?) => {$(
        impl<$($gen)*> $crate::Effect for $op {
            type Key = $crate::effects::$key $(<$($kg),*>)?;
        }
    )*};
}
#[allow(unused_imports)]
pub(crate) use keys;
