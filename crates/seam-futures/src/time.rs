//! Giving any async operation a time limit, by racing it against a clock.
//!
//! futures-rs has no clock of its own, but seam does: [`within`] races a
//! future against `Sleep` on whatever clock it is given (Tokio, the
//! simulated runtime's virtual clock, ...). This is a `Timeout` for
//! operations whose handler has none, such as a `Futures` channel.
//!
//! The losing side is dropped, so the raced future must be safe to cancel:
//! every waiting operation in seam's handlers is.
//!
//! The clock must sleep asynchronously. `Std`'s sleep blocks the thread
//! inside the poll, so with `Std` the race always waits the full time.

use core::future::Future;
use core::pin::pin;
use core::time::Duration;

use futures::future::{Either, select};
use seam_core::future::{AsyncPerform, SendPerform};
use seam_core::ops::time::Sleep;

/// Runs `fut` until it finishes or `after` passes on `clock`. `None` if the
/// time ran out first, in which case `fut` is dropped.
///
/// ```
/// use core::time::Duration;
/// use seam_core::prelude::*;
/// use seam_core::ops::sync::{Channel, RecvMsg};
/// use seam_futures::Futures;
/// use seam_futures::sync::{Receiver, Sender};
/// use seam_futures::time::within;
/// use seam_test::Sim;
/// use seam_test::rt::Runtime;
///
/// let got = Runtime::new(Sim::new()).block_on(|mut sim| async move {
///     let (_tx, mut rx) = Futures.perform(Channel::<Sender<u8>, Receiver<u8>>::unbounded());
///     // Nobody sends: give up after a second of virtual time.
///     within(&mut sim, Duration::from_secs(1), rx.perform_async(RecvMsg::new())).await
/// });
/// assert!(got.is_none());
/// ```
pub async fn within<C, F>(clock: &mut C, after: Duration, fut: F) -> Option<F::Output>
where
    C: AsyncPerform<Sleep>,
    F: Future,
{
    let fut = pin!(fut);
    let sleep = pin!(clock.perform_async(Sleep(after)));
    match select(fut, sleep).await {
        Either::Left((output, _)) => Some(output),
        Either::Right(((), _)) => None,
    }
}

/// [`within`], with a `Send` future: for multithreaded runtimes.
pub async fn within_send<C, F>(clock: &mut C, after: Duration, fut: F) -> Option<F::Output>
where
    C: SendPerform<Sleep> + Send,
    F: Future + Send,
{
    let fut = pin!(fut);
    let sleep = pin!(clock.perform_send(Sleep(after)));
    match select(fut, sleep).await {
        Either::Left((output, _)) => Some(output),
        Either::Right(((), _)) => None,
    }
}
