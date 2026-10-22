//! Networking operations, and the socket handles.

use std::io;
use std::net::SocketAddr;

use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::{AsyncFinalize, Inspect};

use crate::Tokio;

/// A connected TCP stream. Read and write it with the `io` operations.
#[derive(Debug)]
pub struct TcpStream(tokio::net::TcpStream);

/// A listening TCP socket. Perform [`Accept`] on it to get connections.
#[derive(Debug)]
pub struct TcpListener(tokio::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(tokio::net::UdpSocket);

io_ops!(TcpStream);

impl AsyncFinalize for TcpStream {
    type Error = io::Error;

    /// Flushes and shuts down the write half, so the peer sees end of input.
    async fn close_async(mut self) -> io::Result<()> {
        tokio::io::AsyncWriteExt::shutdown(&mut self.0).await
    }
}

impl AsyncFinalize for TcpListener {
    type Error = io::Error;

    async fn close_async(self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncFinalize for UdpSocket {
    type Error = io::Error;

    async fn close_async(self) -> io::Result<()> {
        Ok(())
    }
}

async_op!(inspect [] Tokio, Resolve, |_s, op| {
    Ok(tokio::net::lookup_host((op.host.as_str(), op.port)).await?.collect())
});
async_op!(perform [] Tokio, TcpConnect<TcpStream>, |_s, op| {
    tokio::net::TcpStream::connect(op.addr).await.map(TcpStream)
});
async_op!(perform [] Tokio, TcpBind<TcpListener>, |_s, op| {
    tokio::net::TcpListener::bind(op.addr).await.map(TcpListener)
});
async_op!(perform [] Tokio, UdpBind<UdpSocket>, |_s, op| {
    tokio::net::UdpSocket::bind(op.addr).await.map(UdpSocket)
});
async_op!(perform [] TcpListener, Accept<TcpStream>, |s, _op| {
    let (stream, addr) = s.0.accept().await?;
    Ok((TcpStream(stream), addr))
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
