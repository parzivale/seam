use crate::Operation;

/// Emit one item to a sink.
///
/// Core code that produces output takes a `Perform<Push<T>>` sink instead of
/// building a collection. Tests can then count or cap the items it emits, to
/// check that output scales correctly with input size.
///
/// ```
/// use seam_core::{Perform, Push, Rejected};
///
/// /// Emits each word of `text`. Stops if the sink refuses one.
/// fn words<'t, S: Perform<Push<&'t str>>>(text: &'t str, sink: &mut S) -> Result<(), Rejected<&'t str>> {
///     text.split_whitespace().try_for_each(|w| sink.perform(Push(w)))
/// }
///
/// /// A test sink that only counts.
/// struct Count(usize);
/// impl<T> Perform<Push<T>> for Count {
///     fn perform(&mut self, _: Push<T>) -> Result<(), Rejected<T>> {
///         self.0 += 1;
///         Ok(())
///     }
/// }
///
/// let mut n = Count(0);
/// words("one two three", &mut n).unwrap();
/// assert_eq!(n.0, 3);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Push<T>(pub T);

impl<T> Operation for Push<T> {
    type Outcome = Result<(), Rejected<T>>;
}

/// The sink refused an item, and handed it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected<T>(pub T);
