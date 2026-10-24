//! Networking operations, and the socket handles.

use std::io;
use std::net::SocketAddr;

use seam_core::ops::net::{
    Accept, How, LocalAddr, PeerAddr, RecvFrom, ResolveInto, SendTo, Shutdown, TcpBind, TcpConnect,
    UdpBind,
};
use seam_core::{Closing, Inspect, Opening};

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
pub struct TcpStream(pub(crate) tokio::net::TcpStream);

/// A listening TCP socket, acquired with [`TcpBind`]. Acquire [`Accept`]
/// from it to handle a connection, or `SpawnWith<Accept<_>, ..>` to handle
/// it in a new task.
#[derive(Debug)]
pub struct TcpListener(pub(crate) tokio::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(pub(crate) tokio::net::UdpSocket);

read_ops!([] TcpStream);
write_ops!([] TcpStream);

impl TcpStream {
    /// Flushes and shuts down the write half, so the peer sees end of input.
    /// A peer that has already closed its end counts as success.
    pub(crate) async fn commit(mut self) -> seam_core::io::Result<()> {
        match tokio::io::AsyncWriteExt::shutdown(&mut self.0).await {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            result => crate::sio(result),
        }
    }
}

async_op!(perform [] Tokio, Opening<TcpConnect<TcpStream>>, |_s, Opening(op, _)| {
    crate::sio(tokio::net::TcpStream::connect(op.addr).await.map(TcpStream))
});
async_op!(perform [] Tokio, Closing<TcpConnect<TcpStream>>, |_s, Closing(stream, _)| {
    stream.commit().await
});

async_op!(perform [] Tokio, Opening<TcpBind<TcpListener>>, |_s, Opening(op, _)| {
    crate::sio(
        tokio::net::TcpListener::bind(op.addr)
            .await
            .map(TcpListener),
    )
});
async_op!(perform [] Tokio, Closing<TcpBind<TcpListener>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] Tokio, Opening<UdpBind<UdpSocket>>, |_s, Opening(op, _)| {
    crate::sio(tokio::net::UdpSocket::bind(op.addr).await.map(UdpSocket))
});
async_op!(perform [] Tokio, Closing<UdpBind<UdpSocket>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] TcpListener, Opening<Accept<TcpStream>>, |s, Opening(_, _)| {
    crate::sio(s.0.accept().await.map(|(stream, _)| TcpStream(stream)))
});
async_op!(perform [] TcpListener, Closing<Accept<TcpStream>>, |_s, Closing(stream, _)| {
    stream.commit().await
});

// Accepts a connection and runs `body` with it in a new task, which owns
// the stream. In the task, the stream is shut down cleanly when the body
// returns `Ok`, and just closed otherwise, including when the task is
// aborted.
//
// Closing the provided handle waits for the task; the body's own result is
// discarded, so perform [`Join`](seam_core::ops::task::Join) on the handle
// to see it.
#[cfg(feature = "rt")]
async_op!(
    perform [B] TcpListener,
    Opening<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>,
    |s, Opening(op, _)| {
        let (stream, _) = s.0.accept().await?;
        seam_core::io::Result::Ok(spawn_connection(stream, op.body))
    },
    where B: TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
);
#[cfg(feature = "rt")]
async_op!(
    perform [B] TcpListener,
    Closing<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>,
    |_s, Closing(task, _)| {
        crate::sio(task.finish().await.map_err(io::Error::from))
    },
    where B: TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
);

async_op!(inspect ['a] Tokio, ResolveInto<'a>, |_s, op| {
    let mut n = 0;
    for addr in tokio::net::lookup_host((op.host, op.port)).await? {
        if let Some(slot) = op.out.get_mut(n) {
            *slot = addr;
        }
        n += 1;
    }
    seam_core::io::Result::Ok(n)
});
// Tokio can only shut down the write half of a stream.
async_op!(perform [] TcpStream, Shutdown, |s, op| match op.0 {
    How::Write => crate::sio(tokio::io::AsyncWriteExt::shutdown(&mut s.0).await),
    _ => Err(seam_core::io::Error::new(
        seam_core::io::ErrorKind::Unsupported,
        "Tokio streams can only shut down the write half",
    )),
});
async_op!(perform ['a] UdpSocket, SendTo<'a>, |s, op| s.0.send_to(op.buf, op.addr).await);
async_op!(perform ['a] UdpSocket, RecvFrom<'a>, |s, op| s.0.recv_from(op.0).await);

macro_rules! addr_queries {
    ($($ty:ty: $($op:ident => $method:ident),*;)*) => {$($(
        impl Inspect<$op> for $ty {
            fn inspect(&self, _: $op) -> seam_core::io::Result<SocketAddr> {
                crate::sio(self.0.$method())
            }
        }
    )*)*};
}

addr_queries! {
    TcpStream: LocalAddr => local_addr, PeerAddr => peer_addr;
    TcpListener: LocalAddr => local_addr;
    UdpSocket: LocalAddr => local_addr, PeerAddr => peer_addr;
}

/// Handles `stream` in a new task with `body`, shutting it down cleanly if
/// `body` succeeds.
#[cfg(feature = "rt")]
pub(crate) fn spawn_connection<B>(
    stream: tokio::net::TcpStream,
    body: B,
) -> JoinHandle<Result<B::Output, B::Error>>
where
    B: TaskBody<TcpStream>,
    B::Error: From<seam_core::io::Error>,
{
    JoinHandle::new(tokio::spawn(async move {
        let mut stream = TcpStream(stream);
        let value = body.run(&mut stream).await?;
        stream.commit().await?;
        Ok(value)
    }))
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for TcpListener {}
impl seam_core::Capability for TcpStream {}
impl seam_core::Capability for UdpSocket {}
