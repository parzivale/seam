/// An effect, described as a value.
///
/// Each operation is its own type and names the type it produces, so
/// different operations on the same resource can return different things.
pub trait Operation {
    /// What running the operation produces.
    type Outcome;
}

/// An operation with no side effects: it only reads.
///
/// Queries can run through shared access ([`Inspect`](crate::Inspect)).
/// Queries on the same resource commute with each other, but not with
/// operations that write.
pub trait Query: Operation {}
