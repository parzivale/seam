/// A resource with distinct success and failure cleanup.
///
/// Implement it for handles that must be committed when work succeeds and
/// discarded when it fails: a file that is synced, a transaction that is
/// committed, a temporary file that is renamed into place. Then use it
/// through [`Scoped::scope`], which picks the right one.
///
/// Dropping a resource without `close` (for example, by never calling
/// `scope`) only drops it; nothing is committed.
pub trait Finalize: Sized {
    /// Normal completion: flush, sync, commit.
    fn close(self);

    /// Failure or unwinding: discard without syncing.
    fn abandon(self) {}
}

/// Runs a closure over a resource, then closes or abandons it.
///
/// This is an extension trait with a single blanket impl, so implementors of
/// [`Finalize`] cannot override `scope` and skip cleanup.
///
/// ```
/// use seam_core::{Finalize, Scoped};
///
/// /// Buffered writes that only land on `close`.
/// struct Txn<'a> {
///     pending: Vec<u32>,
///     committed: &'a mut Vec<u32>,
/// }
///
/// impl Finalize for Txn<'_> {
///     fn close(self) {
///         self.committed.extend(self.pending);
///     }
///     // `abandon` defaults to dropping: nothing is committed.
/// }
///
/// let mut db = Vec::new();
/// Txn { pending: Vec::new(), committed: &mut db }.scope(|t| t.pending.push(1));
/// assert_eq!(db, [1]);
///
/// let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
///     Txn { pending: Vec::new(), committed: &mut db }.scope(|t| {
///         t.pending.push(2);
///         panic!("validation failed");
///     })
/// }));
/// assert!(failed.is_err());
/// assert_eq!(db, [1], "the failed transaction was abandoned");
/// ```
pub trait Scoped: Finalize + Sized {
    /// Lends the resource to `f`. Calls [`Finalize::close`] when `f`
    /// returns and [`Finalize::abandon`] if it panics.
    fn scope<R>(self, f: impl FnOnce(&mut Self) -> R) -> R;
}

impl<T: Finalize> Scoped for T {
    fn scope<R>(self, f: impl FnOnce(&mut Self) -> R) -> R {
        let mut guard = Guard(Some(self));
        let out = f(guard
            .0
            .as_mut()
            .expect("resource is present until the guard ends"));
        if let Some(resource) = guard.0.take() {
            resource.close();
        }
        out
    }
}

/// Abandons the resource if it is still held when dropped, which only
/// happens when the closure unwinds.
struct Guard<T: Finalize>(Option<T>);

impl<T: Finalize> Drop for Guard<T> {
    fn drop(&mut self) {
        if let Some(resource) = self.0.take() {
            resource.abandon();
        }
    }
}
