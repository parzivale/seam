//! Timeouts on the runtime, on the virtual clock.
//!
//! Every operation that waits here is atomic under cancellation, so a
//! timeout is a race between the operation and a timer: whichever finishes
//! first wins, and an operation that loses has had no effect. The timer is
//! removed when the race ends, so a timeout that did not expire does not
//! move the clock later.

use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use core::time::Duration;
use seam_core::io;
use std::sync::Arc;

#[allow(unused_imports)]
use seam_core::future::SendPerform;
#[allow(unused_imports)]
use seam_core::ops::time::{Timeout, TimesOut};

#[allow(unused_imports)]
use super::{AsyncSim, Shared, async_op};

/// A timer that is removed when dropped, whether the race finished or was
/// itself cancelled.
struct Timer<'a> {
    shared: &'a Shared,
    key: Option<(Duration, u64)>,
}

impl Drop for Timer<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.shared.remove_timer(key);
        }
    }
}

/// Runs `fut` until it finishes or `after` passes on the virtual clock.
/// `None` if the time ran out first.
pub(crate) async fn within<F: Future>(
    shared: &Shared,
    after: Duration,
    fut: F,
) -> Option<F::Output> {
    let deadline = shared.sim().clock().saturating_add(after);
    let mut fut = pin!(fut);
    let mut timer = Timer { shared, key: None };
    poll_fn(|cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(v));
        }
        if shared.sim().clock() >= deadline {
            return Poll::Ready(None);
        }
        if timer.key.is_none() {
            timer.key = Some(shared.add_timer(deadline, cx.waker().clone()));
        }
        Poll::Pending
    })
    .await
}

#[allow(dead_code)]
fn timed_out() -> io::Error {
    io::ErrorKind::TimedOut.into()
}

#[cfg(feature = "sync")]
mod sync {
    use seam_core::ops::sync::{RecvMsg, SendError, SendMsg};

    use super::*;
    use crate::rt::sync::{Receiver, Sender};

    // A send keeps its value until it is enqueued, so a timed-out one
    // hands it back.
    async_op!([T: Send] Sender<T>, Timeout<SendMsg<T>>, |s, op| {
        let shared = Arc::clone(&s.1);
        let mut value = Some(op.op.0);
        let sent = within(
            &shared,
            op.after,
            poll_fn(|cx| {
                let v = value.take().expect("a send is only polled until it finishes");
                match s.0.poll_send(v, cx.waker()) {
                    Err(SendError::Full(v)) => {
                        value = Some(v);
                        Poll::Pending
                    }
                    done => Poll::Ready(done),
                }
            }),
        )
        .await;
        sent.unwrap_or_else(|| Err(SendError::Full(value.take().expect("still held"))))
    });

    async_op!([T: Send] Receiver<T>, Timeout<RecvMsg<T>>, |s, op| {
        let shared = Arc::clone(&s.1);
        within(&shared, op.after, s.perform_send(RecvMsg::new()))
            .await
            .unwrap_or_else(|| op.op.timed_out())
    });
}

#[cfg(feature = "net")]
mod net {
    use std::net::SocketAddr;

    use seam_core::ops::io::{Read, Write};
    use seam_core::ops::net::{Accept, RecvFrom, TcpConnect};
    use seam_core::ops::task::{SpawnWith, TaskBody};
    use seam_core::{Closing, Opening};

    use super::*;
    use crate::rt::JoinHandle;
    use crate::rt::net::{TcpListener, TcpStream, UdpSocket};

    async_op!(['a] TcpStream, Timeout<Read<'a>>, |s, op| {
        let shared = Arc::clone(&s.1);
        within(&shared, op.after, s.perform_send(op.op)).await.unwrap_or_else(|| Err(timed_out()))
    });

    // Writing never waits in the simulation.
    impl Perform<Timeout<Write<'_>>> for TcpStream {
        fn perform(&mut self, op: Timeout<Write<'_>>) -> io::Result<usize> {
            self.perform(op.op)
        }
    }

    async_op!(['a] UdpSocket, Timeout<RecvFrom<'a>>, |s, op| {
        let shared = Arc::clone(&s.1);
        let received: Option<io::Result<(usize, SocketAddr)>> =
            within(&shared, op.after, s.perform_send(op.op)).await;
        received.unwrap_or_else(|| Err(timed_out()))
    });

    /// Connecting never waits in the simulation, so the timeout is moot.
    impl Perform<Opening<Timeout<TcpConnect<TcpStream>>>> for AsyncSim {
        fn perform(
            &mut self,
            Opening(op, token): Opening<Timeout<TcpConnect<TcpStream>>>,
        ) -> io::Result<TcpStream> {
            self.perform(Opening(op.op, token))
        }
    }

    impl Perform<Closing<Timeout<TcpConnect<TcpStream>>>> for AsyncSim {
        fn perform(
            &mut self,
            Closing(stream, _): Closing<Timeout<TcpConnect<TcpStream>>>,
        ) -> io::Result<()> {
            stream.0.commit()
        }
    }

    async_op!([] TcpListener, Opening<Timeout<Accept<TcpStream>>>, |s, Opening(op, token)| {
        let shared = Arc::clone(&s.1);
        within(&shared, op.after, s.accept(token))
            .await
            .unwrap_or_else(|| Err(timed_out()))
    });
    async_op!([] TcpListener, Closing<Timeout<Accept<TcpStream>>>, |_s, Closing(stream, _)| {
        stream.0.commit()
    });

    // Waits at most the timeout for a connection, then handles it in a new
    // task, as `SpawnWith` without a timeout does.
    async_op!(
        [B] TcpListener,
        Opening<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |s, Opening(op, token)| {
            let shared = Arc::clone(&s.1);
            let mut stream = within(&shared, op.after, s.accept(token))
                .await
                .unwrap_or_else(|| Err(timed_out()))?;
            let body = op.op.body;
            Ok(JoinHandle::spawn(&shared, async move {
                let value = body.run(&mut stream).await?;
                stream.0.commit()?;
                Ok(value)
            }))
        },
        where B: TaskBody<TcpStream>, B::Error: From<io::Error>
    );
    async_op!(
        [B] TcpListener,
        Closing<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |_s, Closing(task, _)| {
            task.finish().await.map_err(io::Error::from)
        },
        where B: TaskBody<TcpStream>, B::Error: From<io::Error>
    );

    use seam_core::Perform;
}

mod task {
    use seam_core::ops::task::{Join, JoinNext};

    use super::*;
    use crate::rt::{JoinHandle, TaskSet};

    async_op!([T: Send] JoinHandle<T>, Timeout<Join<T>>, |s, op| {
        let shared = Arc::clone(&s.shared);
        within(&shared, op.after, s.join()).await.unwrap_or_else(|| op.op.timed_out())
    });

    async_op!([T: Send] TaskSet<T>, Timeout<JoinNext<T>>, |s, op| {
        let shared = Arc::clone(&s.shared);
        within(&shared, op.after, s.join_next()).await.unwrap_or_else(|| op.op.timed_out())
    });
}

#[cfg(feature = "signal")]
mod signal {
    use seam_core::ops::signal::CtrlC;

    use super::*;

    async_op!([] AsyncSim, Timeout<CtrlC>, |s, op| {
        let shared = Arc::clone(&s.shared);
        within(&shared, op.after, s.perform_send(CtrlC)).await.unwrap_or_else(|| op.op.timed_out())
    });
}
