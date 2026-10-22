use seam_core::ops::signal::CtrlC;

use crate::Tokio;

async_op!(perform [] Tokio, CtrlC, |_s, _op| tokio::signal::ctrl_c().await);
