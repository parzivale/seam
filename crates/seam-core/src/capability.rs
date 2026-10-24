use crate::{Operation, Query};

/// Shared access: the capability to run a side-effect-free query.
///
/// Implement this, rather than [`Perform`], for operations that are
/// [`Query`]s. The blanket impl then provides `Perform` too, so the
/// implementation is accepted wherever either is required.
///
/// Prefer an `Inspect` bound when a function only reads: it makes the weaker
/// demand, and it can be called with a shared reference.
///
/// ```
/// use seam_core::{Inspect, Operation, Query};
///
/// struct Now;
/// impl Operation for Now {
///     type Outcome = u64;
/// }
/// impl Query for Now {}
///
/// /// Only reads the clock, so it asks for `Inspect` and takes `&C`.
/// fn is_expired<C: Inspect<Now>>(clock: &C, deadline: u64) -> bool {
///     clock.inspect(Now) >= deadline
/// }
///
/// struct Fixed(u64);
/// impl Inspect<Now> for Fixed {
///     fn inspect(&self, _: Now) -> u64 {
///         self.0
///     }
/// }
///
/// assert!(is_expired(&Fixed(10), 5));
/// ```
///
/// A missing capability is reported as such:
///
/// ```text
/// error[E0277]: `Fixed` does not have the capability to inspect `Other`
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to inspect `{Q}`",
    label = "missing capability `Inspect<{Q}>`",
    note = "add an `Inspect<{Q}>` bound, or pass a value that implements it"
)]
pub trait Inspect<Q: Query> {
    /// Runs the query.
    fn inspect(&self, query: Q) -> Q::Outcome;
}

/// Exclusive access: the capability to run any operation.
///
/// ```
/// use seam_core::{Operation, Perform};
///
/// struct Emit(&'static str);
/// impl Operation for Emit {
///     type Outcome = ();
/// }
///
/// fn greet<C: Perform<Emit>>(out: &mut C) {
///     out.perform(Emit("hello"));
/// }
///
/// #[derive(Default)]
/// struct Recorder(Vec<&'static str>);
/// impl Perform<Emit> for Recorder {
///     fn perform(&mut self, op: Emit) {
///         self.0.push(op.0);
///     }
/// }
///
/// let mut out = Recorder::default();
/// greet(&mut out);
/// assert_eq!(out.0, ["hello"]);
/// ```
///
/// # Bundling capabilities
///
/// A core that needs many capabilities can name them once with a trait and
/// a blanket impl, instead of repeating a long `where` clause:
///
/// ```
/// # use seam_core::{Operation, Perform, Inspect, Query};
/// # struct Load; impl Operation for Load { type Outcome = u64; } impl Query for Load {}
/// # struct Store(u64); impl Operation for Store { type Outcome = (); }
/// /// Everything the counter core needs.
/// trait CounterEnv: Inspect<Load> + Perform<Store> {}
/// impl<T: Inspect<Load> + Perform<Store>> CounterEnv for T {}
///
/// fn bump(env: &mut impl CounterEnv) {
///     let n = env.inspect(Load);
///     env.perform(Store(n + 1));
/// }
/// ```
///
/// # Operations that borrow
///
/// Operations like `ops::io::Read<'a>` borrow a buffer. Bound them for
/// every lifetime with a higher-ranked bound: `for<'a> Perform<Read<'a>>`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}`",
    label = "missing capability `Perform<{O}>`",
    note = "add a `Perform<{O}>` bound, or pass a value that implements it",
    note = "a query is also performed through `Inspect<{O}>`, if the type opts in to the lifts with `impl seam_core::Capability for {Self} {{}}`"
)]
pub trait Perform<O: Operation> {
    /// Runs the operation.
    fn perform(&mut self, op: O) -> O::Outcome;
}

/// Opts a type in to seam's lifts: every [`Inspect`] it implements is also
/// a [`Perform`], and every sync operation also works from async code
/// (`AsyncInspect`, `AsyncPerform`, `SendInspect`, `SendPerform`). Scopes
/// are operations, so the same lifts make every `Lend` a `Provide`, and
/// every sync scope an async one.
///
/// Implement it, with no body, on any capability type you write:
///
/// ```
/// use seam_core::{Capability, Inspect, Operation, Perform, Query};
///
/// struct Now;
/// impl Operation for Now {
///     type Outcome = u64;
/// }
/// impl Query for Now {}
///
/// struct Fixed(u64);
/// impl Capability for Fixed {}
/// impl Inspect<Now> for Fixed {
///     fn inspect(&self, _: Now) -> u64 {
///         self.0
///     }
/// }
///
/// fn tick<C: Perform<Now>>(c: &mut C) -> u64 {
///     c.perform(Now) // a query, performed: thanks to the lift
/// }
/// assert_eq!(tick(&mut Fixed(3)), 3);
/// ```
///
/// The lifts are opt-in, rather than for every type, so that a
/// [`Root`](crate::Root) can forward each form to its parts itself: a sync
/// part's operations through the sync traits, an async part's through the
/// async ones. Every capability type in seam's backends implements it.
pub trait Capability {}

/// Anything that can inspect can also perform the same query, the way
/// `&mut T` reborrows as `&T`.
impl<T: Inspect<Q> + Capability + ?Sized, Q: Query> Perform<Q> for T {
    fn perform(&mut self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}
