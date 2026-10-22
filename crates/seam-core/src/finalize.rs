/// A resource with distinct success and failure cleanup.
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
