use crate::Operation;

/// Emit one item to a sink.
///
/// Core code that produces output takes a `Perform<Push<T>>` sink instead of
/// building a collection. Tests can then count or cap the items it emits, to
/// check that output scales correctly with input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Push<T>(pub T);

impl<T> Operation for Push<T> {
    type Outcome = Result<(), Rejected<T>>;
}

/// The sink refused an item, and handed it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected<T>(pub T);
