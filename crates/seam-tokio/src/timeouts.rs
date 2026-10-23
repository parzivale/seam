//! Timeouts, through `tokio::time::timeout`. Each operation below is
//! cancel-safe in Tokio, so one that times out had no effect, except
//! where `TimesOut` says otherwise (`ReadToEnd` keeps what it read, and
//! `WriteAll` may have written part of its buffer).
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

    use seam_core::ops::io::{Read, ReadToEnd, Write, WriteAll};
    use seam_core::ops::net::{Accept, RecvFrom, TcpConnect};
    use seam_core::{AsyncProvide, Token};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::Tokio;
    use crate::net::{TcpListener, TcpStream, UdpSocket};

    fn timed_out() -> io::Error {
        io::ErrorKind::TimedOut.into()
    }

    async_op!(perform ['a] TcpStream, Timeout<Read<'a>>, |s, op| {
        timeout(op.after, s.0.read(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    // Reads chunk by chunk, so that what arrived before the deadline stays
    // in the buffer.
    async_op!(perform ['a] TcpStream, Timeout<ReadToEnd<'a>>, |s, op| {
        let deadline = tokio::time::Instant::now() + op.after;
        let out = op.op.0;
        let mut chunk = [0; 8192];
        let mut total = 0;
        loop {
            let n = tokio::time::timeout_at(deadline, s.0.read(&mut chunk))
                .await
                .unwrap_or_else(|_| Err(timed_out()))?;
            if n == 0 {
                return Ok(total);
            }
            out.extend_from_slice(&chunk[..n]);
            total += n;
        }
    });

    async_op!(perform ['a] TcpStream, Timeout<Write<'a>>, |s, op| {
        timeout(op.after, s.0.write(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    async_op!(perform ['a] TcpStream, Timeout<WriteAll<'a>>, |s, op| {
        timeout(op.after, s.0.write_all(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    async_op!(perform ['a] UdpSocket, Timeout<RecvFrom<'a>>, |s, op| {
        timeout(op.after, s.0.recv_from(op.op.0)).await.unwrap_or_else(|_| Err(timed_out()))
    });

    impl AsyncProvide<Timeout<TcpConnect<TcpStream>>> for Tokio {
        async fn provide_async(
            &mut self,
            op: Timeout<TcpConnect<TcpStream>>,
            _: Token,
        ) -> io::Result<TcpStream> {
            timeout(op.after, tokio::net::TcpStream::connect(op.op.addr))
                .await
                .unwrap_or_else(|_| Err(timed_out()))
                .map(TcpStream)
        }

        async fn close_async(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit().await
        }
    }

    impl TcpListener {
        pub(crate) async fn accept_within(
            &self,
            after: core::time::Duration,
        ) -> io::Result<tokio::net::TcpStream> {
            let (stream, _) = timeout(after, self.0.accept())
                .await
                .unwrap_or_else(|_| Err(timed_out()))?;
            Ok(stream)
        }
    }

    impl AsyncProvide<Timeout<Accept<TcpStream>>> for TcpListener {
        async fn provide_async(
            &mut self,
            op: Timeout<Accept<TcpStream>>,
            _: Token,
        ) -> io::Result<TcpStream> {
            self.accept_within(op.after).await.map(TcpStream)
        }

        async fn close_async(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit().await
        }
    }

    /// Waits at most the timeout for a connection, then handles it in a new
    /// task, as `SpawnWith` without a timeout does.
    #[cfg(feature = "rt")]
    impl<B>
        AsyncProvide<
            Timeout<
                seam_core::ops::task::SpawnWith<
                    Accept<TcpStream>,
                    B,
                    crate::task::JoinHandle<Result<B::Output, B::Error>>,
                >,
            >,
        > for TcpListener
    where
        B: seam_core::ops::task::TaskBody<TcpStream>,
        B::Error: From<io::Error>,
    {
        async fn provide_async(
            &mut self,
            op: Timeout<
                seam_core::ops::task::SpawnWith<
                    Accept<TcpStream>,
                    B,
                    crate::task::JoinHandle<Result<B::Output, B::Error>>,
                >,
            >,
            _: Token,
        ) -> io::Result<crate::task::JoinHandle<Result<B::Output, B::Error>>> {
            let stream = self.accept_within(op.after).await?;
            Ok(crate::net::spawn_connection(stream, op.op.body))
        }

        async fn close_async(
            &mut self,
            task: crate::task::JoinHandle<Result<B::Output, B::Error>>,
        ) -> io::Result<()> {
            task.finish().await.map_err(io::Error::from)
        }
    }
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
            Ok(result) => result,
            Err(_) => op.op.timed_out(),
        }
    });
}

#[cfg(feature = "process")]
mod process {
    use std::io;

    use seam_core::ops::io::{Read, ReadToEnd};
    use seam_core::ops::process::{ReadErr, ReadErrToEnd, Wait};
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

    // Chunk by chunk, so what arrived before the deadline stays.
    async_op!(perform ['a] Child, Timeout<ReadToEnd<'a>>, |s, op| {
        let deadline = tokio::time::Instant::now() + op.after;
        let out = op.op.0;
        let pipe = s.stdout_mut()?;
        let mut chunk = [0; 8192];
        let mut total = 0;
        loop {
            let n = tokio::time::timeout_at(deadline, pipe.read(&mut chunk))
                .await
                .unwrap_or_else(|_| Err(timed_out()))?;
            if n == 0 {
                return Ok(total);
            }
            out.extend_from_slice(&chunk[..n]);
            total += n;
        }
    });
    async_op!(perform ['a] Child, Timeout<ReadErrToEnd<'a>>, |s, op| {
        let deadline = tokio::time::Instant::now() + op.after;
        let out = op.op.0;
        let pipe = s.stderr_mut()?;
        let mut chunk = [0; 8192];
        let mut total = 0;
        loop {
            let n = tokio::time::timeout_at(deadline, pipe.read(&mut chunk))
                .await
                .unwrap_or_else(|_| Err(timed_out()))?;
            if n == 0 {
                return Ok(total);
            }
            out.extend_from_slice(&chunk[..n]);
            total += n;
        }
    });
}
