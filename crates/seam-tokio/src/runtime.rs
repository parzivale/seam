use core::future::Future;

/// Runs `future` to completion on a new current-thread Tokio runtime.
/// Build the root inside it, from `Tokio::new()` and any other parts.
///
/// # Panics
///
/// If the runtime cannot be built, or if called from inside a runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the Tokio runtime")
        .block_on(future)
}

/// Runs `future` to completion on a new multithreaded Tokio runtime.
///
/// # Panics
///
/// If the runtime cannot be built, or if called from inside a runtime.
#[cfg(feature = "rt-multi-thread")]
pub fn block_on_multi_thread<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the Tokio runtime")
        .block_on(future)
}
