//! Networking operations, and the socket handles.

use std::io;
use std::net::SocketAddr;

use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::{AsyncProvide, Inspect, Token};

#[cfg(feature = "rt")]
use seam_core::ops::task::{SpawnWith, TaskBody};

use crate::Tokio;
#[cfg(feature = "rt")]
use crate::task::JoinHandle;

/// A connected TCP stream, acquired with [`TcpConnect`] or [`Accept`]. Read
/// and write it with the `io` operations.
///
/// When its scope succeeds, its write half is flushed and shut down, so the
/// peer sees end of input; otherwise it is just closed.
#[derive(Debug)]
pub struct TcpStream(tokio::net::TcpStream);

/// A listening TCP socket, acquired with [`TcpBind`]. Acquire [`Accept`]
/// from it to handle a connection, or `SpawnWith<Accept<_>, ..>` to handle
/// it in a new task.
#[derive(Debug)]
pub struct TcpListener(tokio::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(tokio::net::UdpSocket);

io_ops!(TcpStream);

impl TcpStream {
    /// Flushes and shuts down the write half, so the peer sees end of input.
    async fn commit(mut self) -> io::Result<()> {
        tokio::io::AsyncWriteExt::shutdown(&mut self.0).await
    }
}

impl AsyncProvide<TcpConnect<TcpStream>> for Tokio {
    async fn provide_async(
        &mut self,
        op: TcpConnect<TcpStream>,
        _: Token,
    ) -> io::Result<TcpStream> {
        tokio::net::TcpStream::connect(op.addr).await.map(TcpStream)
    }

    async fn close_async(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit().await
    }
}

impl AsyncProvide<TcpBind<TcpListener>> for Tokio {
    async fn provide_async(
        &mut self,
        op: TcpBind<TcpListener>,
        _: Token,
    ) -> io::Result<TcpListener> {
        tokio::net::TcpListener::bind(op.addr)
            .await
            .map(TcpListener)
    }

    async fn close_async(&mut self, _: TcpListener) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncProvide<UdpBind<UdpSocket>> for Tokio {
    async fn provide_async(&mut self, op: UdpBind<UdpSocket>, _: Token) -> io::Result<UdpSocket> {
        tokio::net::UdpSocket::bind(op.addr).await.map(UdpSocket)
    }

    async fn close_async(&mut self, _: UdpSocket) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncProvide<Accept<TcpStream>> for TcpListener {
    async fn provide_async(&mut self, _: Accept<TcpStream>, _: Token) -> io::Result<TcpStream> {
        self.0.accept().await.map(|(stream, _)| TcpStream(stream))
    }

    async fn close_async(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit().await
    }
}

/// Accepts a connection and runs `body` with it in a new task, which owns
/// the stream. In the task, the stream is shut down cleanly when the body
/// returns `Ok`, and just closed otherwise, including when the task is
/// aborted.
///
/// Closing the provided handle waits for the task; the body's own result is
/// discarded, so perform [`Join`](seam_core::ops::task::Join) on the handle
/// to see it.
#[cfg(feature = "rt")]
impl<B> AsyncProvide<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>
    for TcpListener
where
    B: TaskBody<TcpStream>,
    B::Error: From<io::Error>,
{
    async fn provide_async(
        &mut self,
        op: SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>,
        _: Token,
    ) -> io::Result<JoinHandle<Result<B::Output, B::Error>>> {
        let (stream, _) = self.0.accept().await?;
        let body = op.body;
        Ok(JoinHandle::new(tokio::spawn(async move {
            let mut stream = TcpStream(stream);
            let value = body.run(&mut stream).await?;
            stream.commit().await?;
            Ok(value)
        })))
    }

    async fn close_async(
        &mut self,
        task: JoinHandle<Result<B::Output, B::Error>>,
    ) -> io::Result<()> {
        task.finish().await.map_err(io::Error::from)
    }
}

async_op!(inspect [] Tokio, Resolve, |_s, op| {
    Ok(tokio::net::lookup_host((op.host.as_str(), op.port)).await?.collect())
});
// Tokio can only shut down the write half of a stream.
async_op!(perform [] TcpStream, Shutdown, |s, op| match op.0 {
    std::net::Shutdown::Write => tokio::io::AsyncWriteExt::shutdown(&mut s.0).await,
    _ => Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Tokio streams can only shut down the write half",
    )),
});
async_op!(perform ['a] UdpSocket, SendTo<'a>, |s, op| s.0.send_to(op.buf, op.addr).await);
async_op!(perform ['a] UdpSocket, RecvFrom<'a>, |s, op| s.0.recv_from(op.0).await);

macro_rules! addr_queries {
    ($($ty:ty: $($op:ident => $method:ident),*;)*) => {$($(
        impl Inspect<$op> for $ty {
            fn inspect(&self, _: $op) -> io::Result<SocketAddr> {
                self.0.$method()
            }
        }
    )*)*};
}

addr_queries! {
    TcpStream: LocalAddr => local_addr, PeerAddr => peer_addr;
    TcpListener: LocalAddr => local_addr;
    UdpSocket: LocalAddr => local_addr, PeerAddr => peer_addr;
}
