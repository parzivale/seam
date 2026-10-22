use crate::{Operation, Query};

/// Shared access: the capability to run a side-effect-free query.
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
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not have the capability to perform `{O}`",
    label = "missing capability `Perform<{O}>`",
    note = "add a `Perform<{O}>` bound, or pass a value that implements it"
)]
pub trait Perform<O: Operation> {
    /// Runs the operation.
    fn perform(&mut self, op: O) -> O::Outcome;
}

/// Anything that can inspect can also perform the same query, the way
/// `&mut T` reborrows as `&T`.
impl<T: Inspect<Q> + ?Sized, Q: Query> Perform<Q> for T {
    fn perform(&mut self, query: Q) -> Q::Outcome {
        self.inspect(query)
    }
}
