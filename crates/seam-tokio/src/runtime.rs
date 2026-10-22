use core::future::Future;

use crate::Tokio;

/// Runs a core on a new current-thread Tokio runtime, handing it the root
/// capability.
///
/// # Panics
///
/// If the runtime cannot be built, or if called from inside a runtime.
pub fn block_on<F: Future>(core: impl FnOnce(Tokio) -> F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build the Tokio runtime")
        .block_on(core(Tokio::new()))
}

/// Runs a core on a new multithreaded Tokio runtime, handing it the root
/// capability.
///
/// # Panics
///
/// If the runtime cannot be built, or if called from inside a runtime.
#[cfg(feature = "rt-multi-thread")]
pub fn block_on_multi_thread<F: Future>(core: impl FnOnce(Tokio) -> F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build the Tokio runtime")
        .block_on(core(Tokio::new()))
}
