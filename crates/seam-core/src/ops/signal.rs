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

/// Gives up with `io::ErrorKind::TimedOut`; the Ctrl-C, if one comes
/// later, is still delivered to the next wait.
#[cfg(feature = "time")]
impl crate::ops::time::TimesOut for CtrlC {
    type Expired = io::Result<()>;
    fn timed_out(self) -> Self::Expired {
        Err(crate::ops::time::io_timed_out())
    }
}
