/// An effect, described as a value.
///
/// Each operation is its own type and names the type it produces, so
/// different operations on the same resource can return different things.
/// The operation's fields are its arguments.
///
/// ```
/// use seam_core::Operation;
///
/// /// Look up a user's display name.
/// struct DisplayName {
///     user_id: u64,
/// }
///
/// impl Operation for DisplayName {
///     type Outcome = Option<String>;
/// }
/// ```
///
/// Ready-made operations for common effects are in [`ops`](crate::ops).
pub trait Operation {
    /// What running the operation produces.
    type Outcome;
}

/// An operation with no side effects: it only reads.
///
/// Queries can run through shared access ([`Inspect`](crate::Inspect)), and
/// anything that can inspect a query can also perform it. Mark an operation
/// as a query when running it twice in a row, with nothing in between, would
/// give the same answer and change nothing.
///
/// Queries on the same resource commute with each other, but not with
/// operations that write.
///
/// ```
/// use seam_core::{Operation, Query};
///
/// struct Temperature;
/// impl Operation for Temperature {
///     type Outcome = f32;
/// }
/// impl Query for Temperature {}
/// ```
pub trait Query: Operation {}
