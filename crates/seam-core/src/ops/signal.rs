//! OS signals.
//!
//! Waiting for a signal is inherently async; use it with
//! [`SendPerform`](crate::future::SendPerform) or
//! [`AsyncPerform`](crate::future::AsyncPerform) on a backend such as
//! `seam-tokio`.

use std::io;

/// Wait for Ctrl-C.
#[derive(Debug, Clone, Copy, Default)]
pub struct CtrlC;

operations! {
    CtrlC => io::Result<()>;
}
