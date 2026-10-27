use seam_core::ops::signal::CtrlC;

use crate::Smol;

/// Waits for the next SIGINT. The handler is installed for the wait only.
async fn ctrl_c() -> std::io::Result<()> {
    let mut signals = async_signal::Signals::new([async_signal::Signal::Int])?;
    match smol::stream::StreamExt::next(&mut signals).await {
        Some(signal) => signal.map(drop),
        None => Err(std::io::Error::other("the signal stream ended")),
    }
}

async_op!(perform [] Smol, CtrlC, |_s, _op| ctrl_c().await);

#[cfg(feature = "time")]
async_op!(perform [] Smol, seam_core::ops::time::Timeout<CtrlC>, |_s, op| {
    use seam_core::ops::time::TimesOut;
    match crate::within(op.after, ctrl_c()).await {
        Some(result) => crate::sio(result),
        None => op.op.timed_out(),
    }
});
