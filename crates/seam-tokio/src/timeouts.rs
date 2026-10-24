//! Timeouts, through `tokio::time::timeout`. Each operation below is
//! cancel-safe in Tokio, so one that times out had no effect. The inner
//! future is polled before the deadline is checked, so a timeout of zero
//! (`try_send`, `try_recv`) is a non-blocking attempt.
//!
//! Tokio's clock drives the deadline, so paused time (`test-util`)
//! applies.

#[allow(unused_imports)]
use seam_core::ops::time::{Timeout, TimesOut};
#[allow(unused_imports)]
use tokio::time::timeout;

#[cfg(feature = "sync")]
mod sync {
    use seam_core::ops::sync::{RecvError, RecvMsg, SendError, SendMsg};

    use super::*;
    use crate::sync::{Receiver, Rx, Sender, Tx};

    // Waits for room by reserving a slot, so a timed-out send keeps its
    // value and hands it back.
    async_op!(perform [T: Send] Sender<T>, Timeout<SendMsg<T>>, |s, op| match &s.0 {
        Tx::Unbounded(tx) => tx.send(op.op.0).map_err(|e| SendError::Closed(e.0)),
        Tx::Bounded(tx) => match timeout(op.after, tx.reserve()).await {
            Ok(Ok(permit)) => {
                permit.send(op.op.0);
                Ok(())
            }
            Ok(Err(_)) => Err(SendError::Closed(op.op.0)),
            Err(_) => op.op.timed_out(),
        },
    });

    async_op!(perform [T: Send] Receiver<T>, Timeout<RecvMsg<T>>, |s, op| {
        let received = match &mut s.0 {
            Rx::Unbounded(rx) => timeout(op.after, rx.recv()).await,
            Rx::Bounded(rx) => timeout(op.after, rx.recv()).await,
        };
        match received {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(RecvError::Closed),
            Err(_) => op.op.timed_out(),
        }
    });
}

#[cfg(feature = "net")]
mod net {
    use std::io;

    use seam_core::ops::io::{Read, Write};
    use seam_core::ops::net::{Accept, RecvFrom, TcpConnect};
    #[cfg(feature = "rt")]
    use seam_core::ops::task::SpawnWith;
    use seam_core::{Closing, Opening};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::Tokio;
    use crate::net::{TcpListener, TcpStream, UdpSocket};
    #[cfg(feature = "rt")]
    use crate::task::JoinHandle;

    fn timed_out() -> io::Error {
        io::ErrorKind::TimedOut.into()
    }

    async_op!(perform ['a] TcpStream, Timeout<Read<'a>>, |s, op| {
        timeout(op.after, s.0.read(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    async_op!(perform ['a] TcpStream, Timeout<Write<'a>>, |s, op| {
        timeout(op.after, s.0.write(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    async_op!(perform ['a] UdpSocket, Timeout<RecvFrom<'a>>, |s, op| {
        timeout(op.after, s.0.recv_from(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    async_op!(perform [] Tokio, Opening<Timeout<TcpConnect<TcpStream>>>, |_s, Opening(op, _)| {
        crate::sio(
            timeout(op.after, tokio::net::TcpStream::connect(op.op.addr))
                .await
                .unwrap_or_else(|_| Err(timed_out()))
                .map(TcpStream),
        )
    });
    async_op!(perform [] Tokio, Closing<Timeout<TcpConnect<TcpStream>>>, |_s, Closing(stream, _)| {
        stream.commit().await
    });

    impl TcpListener {
        pub(crate) async fn accept_within(
            &self,
            after: core::time::Duration,
        ) -> seam_core::io::Result<tokio::net::TcpStream> {
            let (stream, _) = timeout(after, self.0.accept())
                .await
                .unwrap_or_else(|_| Err(timed_out()))?;
            Ok(stream)
        }
    }

    async_op!(perform [] TcpListener, Opening<Timeout<Accept<TcpStream>>>, |s, Opening(op, _)| {
        s.accept_within(op.after).await.map(TcpStream)
    });
    async_op!(
        perform [] TcpListener, Closing<Timeout<Accept<TcpStream>>>,
        |_s, Closing(stream, _)| {
            stream.commit().await
        }
    );

    // Waits at most the timeout for a connection, then handles it in a new
    // task, as `SpawnWith` without a timeout does.
    #[cfg(feature = "rt")]
    async_op!(
        perform [B] TcpListener,
        Opening<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |s, Opening(op, _)| {
            let stream = s.accept_within(op.after).await?;
            seam_core::io::Result::Ok(crate::net::spawn_connection(stream, op.op.body))
        },
        where B: seam_core::ops::task::TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
    );
    #[cfg(feature = "rt")]
    async_op!(
        perform [B] TcpListener,
        Closing<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |_s, Closing(task, _)| {
            crate::sio(task.finish().await.map_err(io::Error::from))
        },
        where B: seam_core::ops::task::TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
    );
}

#[cfg(feature = "rt")]
mod task {
    use seam_core::ops::task::{Join, JoinNext};

    use super::*;
    use crate::task::{JoinHandle, TaskSet};

    // Polling a Tokio join handle or set by reference is cancel-safe: the
    // task keeps running, and can be joined again.
    async_op!(perform [T: Send] JoinHandle<T>, Timeout<Join<T>>, |s, op| {
        match timeout(op.after, s.join()).await {
            Ok(result) => result,
            Err(_) => op.op.timed_out(),
        }
    });

    async_op!(perform [T: Send + 'static] TaskSet<T>, Timeout<JoinNext<T>>, |s, op| {
        match timeout(op.after, s.join_next()).await {
            Ok(next) => next,
            Err(_) => op.op.timed_out(),
        }
    });
}

#[cfg(feature = "signal")]
mod signal {
    use seam_core::ops::signal::CtrlC;

    use super::*;
    use crate::Tokio;

    async_op!(perform [] Tokio, Timeout<CtrlC>, |_s, op| {
        match timeout(op.after, tokio::signal::ctrl_c()).await {
            Ok(result) => crate::sio(result),
            Err(_) => op.op.timed_out(),
        }
    });
}

#[cfg(feature = "process")]
mod process {
    use std::io;

    use seam_core::ops::io::Read;
    use seam_core::ops::process::{ReadErr, Wait};
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::process::Child;

    fn timed_out() -> io::Error {
        io::ErrorKind::TimedOut.into()
    }

    // Each of these is cancel-safe in Tokio: a wait leaves the child
    // running, and a read that times out has read nothing.
    async_op!(perform [] Child, Timeout<Wait>, |s, op| {
        timeout(op.after, s.wait()).await.unwrap_or_else(|_| op.op.timed_out())
    });
    async_op!(perform ['a] Child, Timeout<Read<'a>>, |s, op| {
        let out = s.stdout_mut()?;
        timeout(op.after, out.read(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });
    async_op!(perform ['a] Child, Timeout<ReadErr<'a>>, |s, op| {
        let err = s.stderr_mut()?;
        timeout(op.after, err.read(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });
}
