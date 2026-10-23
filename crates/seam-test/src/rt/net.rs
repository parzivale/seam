//! Sockets on the simulated runtime, whose reads and accepts wait.
//!
//! These wrap the in-memory sockets of [`crate::net`]. Writing never waits,
//! since the simulated network has no buffer limit. Reading a stream with
//! no data yet, accepting with no connection pending, and receiving with no
//! datagram suspend until another task makes it possible.
#![allow(clippy::disallowed_types)] // a listener keeps the runtime, to spawn tasks

use core::future::poll_fn;
use core::net::SocketAddr;
use core::task::Poll;
use seam_core::io;
use std::sync::Arc;

use seam_core::ops::io::{Flush, Read, ReadExact, ReadToEnd, Write, WriteAll};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::ops::task::{SpawnWith, TaskBody};
use seam_core::{AsyncProvide, Inspect, Perform, Provide, Token};

use super::{AsyncSim, JoinHandle, Shared, async_op};
use crate::net as sim;

/// A connected TCP stream on the simulated runtime, acquired with
/// [`TcpConnect`] or [`Accept`]. Reads wait for data or end of input.
///
/// When its scope succeeds its write half is shut down, so the peer sees
/// end of input.
pub struct TcpStream(
    pub(crate) sim::TcpStream,
    #[cfg_attr(not(feature = "time"), allow(dead_code))] pub(crate) Arc<Shared>,
);

/// A listening TCP socket on the simulated runtime, acquired with
/// [`TcpBind`]. Accepts wait for a connection.
pub struct TcpListener(pub(crate) sim::TcpListener, pub(crate) Arc<Shared>);

impl core::fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("TcpStream").field(&self.0).finish()
    }
}

impl core::fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("UdpSocket").field(&self.0).finish()
    }
}

impl core::fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("TcpListener").field(&self.0).finish()
    }
}

/// A UDP socket on the simulated runtime, acquired with [`UdpBind`].
/// Receives wait for a datagram.
pub struct UdpSocket(
    pub(crate) sim::UdpSocket,
    #[cfg_attr(not(feature = "time"), allow(dead_code))] pub(crate) Arc<Shared>,
);

impl TcpStream {
    pub(crate) async fn readable(&self) {
        poll_fn(|cx| {
            if self.0.poll_readable(cx.waker()) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

impl Provide<TcpBind<TcpListener>> for AsyncSim {
    fn provide(&mut self, op: TcpBind<TcpListener>, token: Token) -> io::Result<TcpListener> {
        Provide::provide(
            &mut *self.shared.sim(),
            TcpBind::<sim::TcpListener>::new(op.addr),
            token,
        )
        .map(|l| TcpListener(l, Arc::clone(&self.shared)))
    }

    fn close(&mut self, listener: TcpListener) -> io::Result<()> {
        Provide::<TcpBind<sim::TcpListener>>::close(&mut *self.shared.sim(), listener.0)
    }
}

impl Provide<TcpConnect<TcpStream>> for AsyncSim {
    fn provide(&mut self, op: TcpConnect<TcpStream>, token: Token) -> io::Result<TcpStream> {
        Provide::provide(
            &mut *self.shared.sim(),
            TcpConnect::<sim::TcpStream>::new(op.addr),
            token,
        )
        .map(|s| TcpStream(s, Arc::clone(&self.shared)))
    }

    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.0.commit()
    }
}

impl Provide<UdpBind<UdpSocket>> for AsyncSim {
    fn provide(&mut self, op: UdpBind<UdpSocket>, token: Token) -> io::Result<UdpSocket> {
        Provide::provide(
            &mut *self.shared.sim(),
            UdpBind::<sim::UdpSocket>::new(op.addr),
            token,
        )
        .map(|s| UdpSocket(s, Arc::clone(&self.shared)))
    }

    fn close(&mut self, _: UdpSocket) -> io::Result<()> {
        Ok(())
    }
}

impl TcpListener {
    pub(crate) async fn accept(&mut self, token: Token) -> io::Result<TcpStream> {
        poll_fn(|cx| {
            if self.0.poll_acceptable(cx.waker()) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        self.0
            .provide(Accept::new(), token)
            .map(|s| TcpStream(s, Arc::clone(&self.1)))
    }
}

impl AsyncProvide<Accept<TcpStream>> for TcpListener {
    async fn provide_async(&mut self, _: Accept<TcpStream>, token: Token) -> io::Result<TcpStream> {
        self.accept(token).await
    }

    async fn close_async(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.0.commit()
    }
}

/// Waits for a connection and runs `body` with it in a new task, which owns
/// the stream. In the task, the stream is shut down when the body returns
/// `Ok`, and just closed otherwise.
///
/// Closing the provided handle waits for the task; perform `Join` on it to
/// see the body's result.
impl<B> AsyncProvide<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>
    for TcpListener
where
    B: TaskBody<TcpStream>,
    B::Error: From<io::Error>,
{
    async fn provide_async(
        &mut self,
        op: SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>,
        token: Token,
    ) -> io::Result<JoinHandle<Result<B::Output, B::Error>>> {
        let mut stream = self.accept(token).await?;
        let body = op.body;
        Ok(JoinHandle::spawn(&self.1, async move {
            let value = body.run(&mut stream).await?;
            stream.0.commit()?;
            Ok(value)
        }))
    }

    async fn close_async(
        &mut self,
        task: JoinHandle<Result<B::Output, B::Error>>,
    ) -> io::Result<()> {
        task.finish().await.map_err(io::Error::from)
    }
}

async_op!(['a] TcpStream, Read<'a>, |s, op| {
    s.readable().await;
    s.0.perform(op)
});

// Waits until the buffer is full or the peer closes its end, as a real
// stream does.
async_op!(['a] TcpStream, ReadExact<'a>, |s, op| {
    let mut buf = op.0;
    while !buf.is_empty() {
        s.readable().await;
        let rest = core::mem::take(&mut buf);
        match s.0.perform(Read(&mut *rest))? {
            0 => return Err(crate::error(io::ErrorKind::UnexpectedEof, "the stream ended before the buffer was full")),
            n => buf = &mut rest[n..],
        }
    }
    Ok(())
});

// Reads until the peer closes its end, as a real stream does.
async_op!(['a] TcpStream, ReadToEnd<'a>, |s, op| {
    let mut total = 0;
    loop {
        s.readable().await;
        let n = s.0.perform(ReadToEnd(&mut *op.0))?;
        total += n;
        if n == 0 {
            return Ok(total);
        }
    }
});

impl Perform<Write<'_>> for TcpStream {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.0.perform(op)
    }
}

impl Perform<WriteAll<'_>> for TcpStream {
    fn perform(&mut self, op: WriteAll<'_>) -> io::Result<()> {
        self.0.perform(op)
    }
}

impl Perform<Flush> for TcpStream {
    fn perform(&mut self, op: Flush) -> io::Result<()> {
        self.0.perform(op)
    }
}

impl Perform<Shutdown> for TcpStream {
    fn perform(&mut self, op: Shutdown) -> io::Result<()> {
        self.0.perform(op)
    }
}

impl Perform<SendTo<'_>> for UdpSocket {
    fn perform(&mut self, op: SendTo<'_>) -> io::Result<usize> {
        self.0.perform(op)
    }
}

async_op!(['a] UdpSocket, RecvFrom<'a>, |s, op| {
    poll_fn(|cx| if s.0.poll_receivable(cx.waker()) { Poll::Ready(()) } else { Poll::Pending }).await;
    s.0.perform(op)
});

macro_rules! addr_queries {
    ($($ty:ty: $($op:ident),*;)*) => {$($(
        impl Inspect<$op> for $ty {
            fn inspect(&self, op: $op) -> io::Result<SocketAddr> {
                self.0.inspect(op)
            }
        }
    )*)*};
}

addr_queries! {
    TcpStream: LocalAddr, PeerAddr;
    TcpListener: LocalAddr;
    UdpSocket: LocalAddr, PeerAddr;
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for TcpListener {}
impl seam_core::Capability for TcpStream {}
impl seam_core::Capability for UdpSocket {}
