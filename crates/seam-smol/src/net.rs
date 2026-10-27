//! Networking operations, and the socket handles.

use std::io;
use std::net::SocketAddr;

use seam_core::ops::io::{Halves, Split};
#[cfg(feature = "time")]
use seam_core::ops::io::{Read, Write};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, ResolveInto, SendTo, Shutdown, TcpBind, TcpConnect,
    UdpBind,
};
use seam_core::{Closing, Inspect, Opening};

#[cfg(feature = "rt")]
use seam_core::ops::task::{SpawnWith, TaskBody};

use crate::Smol;
#[cfg(feature = "rt")]
use crate::task::JoinHandle;

/// A connected TCP stream, acquired with [`TcpConnect`] or [`Accept`]. Read
/// and write it with the `io` operations.
///
/// When its scope succeeds, its write half is flushed and shut down, so the
/// peer sees end of input; otherwise it is just closed. [`Split`] lends it
/// as a [`TcpReadHalf`] and a [`TcpWriteHalf`], each a clone of the
/// stream, so a failed split loses nothing.
#[derive(Debug)]
pub struct TcpStream(pub(crate) smol::net::TcpStream);

/// The half of a [`Split`] [`TcpStream`] that reads.
#[derive(Debug)]
pub struct TcpReadHalf(pub(crate) smol::net::TcpStream);

/// The half of a [`Split`] [`TcpStream`] that writes.
#[derive(Debug)]
pub struct TcpWriteHalf(pub(crate) smol::net::TcpStream);

/// A listening TCP socket, acquired with [`TcpBind`]. Acquire [`Accept`]
/// from it to handle a connection, or `SpawnWith<Accept<_>, ..>` to handle
/// it in a new task.
#[derive(Debug)]
pub struct TcpListener(pub(crate) smol::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(pub(crate) smol::net::UdpSocket);

read_ops!([] TcpStream);
write_ops!([] TcpStream);
read_ops!([] TcpReadHalf);
write_ops!([] TcpWriteHalf);

/// Flushes and shuts down the write half, so the peer sees end of input. A
/// peer that has already closed its end counts as success.
async fn commit(mut stream: smol::net::TcpStream) -> seam_core::io::Result<()> {
    smol::io::AsyncWriteExt::flush(&mut stream).await?;
    match stream.shutdown(std::net::Shutdown::Write) {
        Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
        result => crate::sio(result),
    }
}

impl TcpStream {
    pub(crate) async fn commit(self) -> seam_core::io::Result<()> {
        commit(self.0).await
    }
}

async_op!(perform [] Smol, Opening<TcpConnect<TcpStream>>, |_s, Opening(op, _)| {
    crate::sio(smol::net::TcpStream::connect(op.addr).await.map(TcpStream))
});
async_op!(perform [] Smol, Closing<TcpConnect<TcpStream>>, |_s, Closing(stream, _)| {
    stream.commit().await
});

async_op!(perform [] Smol, Opening<TcpBind<TcpListener>>, |_s, Opening(op, _)| {
    crate::sio(smol::net::TcpListener::bind(op.addr).await.map(TcpListener))
});
async_op!(perform [] Smol, Closing<TcpBind<TcpListener>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] Smol, Opening<UdpBind<UdpSocket>>, |_s, Opening(op, _)| {
    crate::sio(smol::net::UdpSocket::bind(op.addr).await.map(UdpSocket))
});
async_op!(perform [] Smol, Closing<UdpBind<UdpSocket>>, |_s, Closing(_, _)| {
    seam_core::io::Result::Ok(())
});

async_op!(perform [] TcpListener, Opening<Accept<TcpStream>>, |s, Opening(_, _)| {
    crate::sio(s.0.accept().await.map(|(stream, _)| TcpStream(stream)))
});
async_op!(perform [] TcpListener, Closing<Accept<TcpStream>>, |_s, Closing(stream, _)| {
    stream.commit().await
});

// Two clones of the stream: smol's streams are shared handles.
async_op!(perform [] TcpStream, Opening<Split<TcpReadHalf, TcpWriteHalf>>, |s, Opening(_, _)| {
    io::Result::Ok(Halves { read: TcpReadHalf(s.0.clone()), write: TcpWriteHalf(s.0.clone()) })
});
async_op!(perform [] TcpStream, Closing<Split<TcpReadHalf, TcpWriteHalf>>, |_s, Closing(halves, _)| {
    let mut write = halves.write.0;
    smol::io::AsyncWriteExt::flush(&mut write).await
});

// Accepts a connection and runs `body` with it in a new task, which owns
// the stream. In the task, the stream is shut down cleanly when the body
// returns `Ok`, and just closed otherwise, including when the task is
// cancelled.
//
// Closing the provided handle waits for the task; the body's own result is
// discarded, so perform `Join` on the handle to see it.
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

/// Handles `stream` in a new task with `body`, shutting it down cleanly if
/// `body` succeeds.
#[cfg(feature = "rt")]
pub(crate) fn spawn_connection<B>(
    stream: smol::net::TcpStream,
    body: B,
) -> JoinHandle<Result<B::Output, B::Error>>
where
    B: TaskBody<TcpStream>,
    B::Error: From<seam_core::io::Error>,
{
    JoinHandle::spawn(async move {
        let mut stream = TcpStream(stream);
        let value = body.run(&mut stream).await?;
        stream.commit().await?;
        Ok(value)
    })
}

async_op!(inspect ['a] Smol, ResolveInto<'a>, |_s, op| {
    let addrs = smol::net::resolve((op.host, op.port)).await?;
    for (slot, addr) in op.out.iter_mut().zip(&addrs) {
        *slot = *addr;
    }
    seam_core::io::Result::Ok(addrs.len())
});

async_op!(perform [] TcpStream, Shutdown, |s, op| crate::sio(s.0.shutdown(op.0.into())));
async_op!(perform [] TcpWriteHalf, Shutdown, |s, op| crate::sio(s.0.shutdown(op.0.into())));
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
    TcpReadHalf: LocalAddr => local_addr, PeerAddr => peer_addr;
    TcpWriteHalf: LocalAddr => local_addr, PeerAddr => peer_addr;
    TcpListener: LocalAddr => local_addr;
    UdpSocket: LocalAddr => local_addr, PeerAddr => peer_addr;
}

/// Timeouts: each operation races a timer, and each is cancel-safe in
/// smol, so one that times out had no effect.
#[cfg(feature = "time")]
mod timeouts {
    use seam_core::ops::time::Timeout;
    use smol::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{timed_out, within};

    async_op!(perform ['a] TcpStream, Timeout<Read<'a>>, |s, op| {
        within(op.after, s.0.read(op.op.0)).await.unwrap_or_else(|| Err(timed_out()))
    });
    async_op!(perform ['a] TcpStream, Timeout<Write<'a>>, |s, op| {
        within(op.after, s.0.write(op.op.0)).await.unwrap_or_else(|| Err(timed_out()))
    });
    async_op!(perform ['a] TcpReadHalf, Timeout<Read<'a>>, |s, op| {
        within(op.after, s.0.read(op.op.0)).await.unwrap_or_else(|| Err(timed_out()))
    });
    async_op!(perform ['a] TcpWriteHalf, Timeout<Write<'a>>, |s, op| {
        within(op.after, s.0.write(op.op.0)).await.unwrap_or_else(|| Err(timed_out()))
    });
    async_op!(perform ['a] UdpSocket, Timeout<RecvFrom<'a>>, |s, op| {
        within(op.after, s.0.recv_from(op.op.0)).await.unwrap_or_else(|| Err(timed_out()))
    });

    async_op!(perform [] Smol, Opening<Timeout<TcpConnect<TcpStream>>>, |_s, Opening(op, _)| {
        crate::sio(
            within(op.after, smol::net::TcpStream::connect(op.op.addr))
                .await
                .unwrap_or_else(|| Err(timed_out()))
                .map(TcpStream),
        )
    });
    async_op!(perform [] Smol, Closing<Timeout<TcpConnect<TcpStream>>>, |_s, Closing(stream, _)| {
        stream.commit().await
    });

    impl TcpListener {
        pub(crate) async fn accept_within(
            &self,
            after: core::time::Duration,
        ) -> seam_core::io::Result<smol::net::TcpStream> {
            let accepted = within(after, self.0.accept())
                .await
                .unwrap_or_else(|| Err(timed_out()));
            crate::sio(accepted.map(|(stream, _)| stream))
        }
    }

    async_op!(perform [] TcpListener, Opening<Timeout<Accept<TcpStream>>>, |s, Opening(op, _)| {
        s.accept_within(op.after).await.map(TcpStream)
    });
    async_op!(perform [] TcpListener, Closing<Timeout<Accept<TcpStream>>>, |_s, Closing(stream, _)| {
        stream.commit().await
    });

    // Waits at most the timeout for a connection, then handles it in a new
    // task, as `SpawnWith` without a timeout does.
    #[cfg(feature = "rt")]
    async_op!(
        perform [B] TcpListener,
        Opening<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |s, Opening(op, _)| {
            let stream = s.accept_within(op.after).await?;
            seam_core::io::Result::Ok(spawn_connection(stream, op.op.body))
        },
        where B: TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
    );
    #[cfg(feature = "rt")]
    async_op!(
        perform [B] TcpListener,
        Closing<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<B::Output, B::Error>>>>>,
        |_s, Closing(task, _)| {
            crate::sio(task.finish().await.map_err(io::Error::from))
        },
        where B: TaskBody<TcpStream>, B::Error: From<seam_core::io::Error>
    );
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for TcpListener {}
impl seam_core::Capability for TcpStream {}
impl seam_core::Capability for TcpReadHalf {}
impl seam_core::Capability for TcpWriteHalf {}
impl seam_core::Capability for UdpSocket {}
