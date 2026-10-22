//! OS signals.

use std::io;

/// Wait for Ctrl-C.
#[derive(Debug, Clone, Copy, Default)]
pub struct CtrlC;

operations! {
    CtrlC => io::Result<()>;
}
