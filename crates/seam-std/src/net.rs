//! Networking operations, and the socket handles.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::{Inspect, Perform, Provide, Token};

#[cfg(feature = "rt")]
use seam_core::ops::task::SpawnWith;

use crate::Std;
#[cfg(feature = "rt")]
use crate::task::JoinHandle;

/// A connected TCP stream, acquired with [`TcpConnect`] or [`Accept`]. Read
/// and write it with the `io` operations.
///
/// When its scope succeeds it is flushed and shut down cleanly; otherwise
/// the socket is just closed.
#[derive(Debug)]
pub struct TcpStream(std::net::TcpStream);

/// A listening TCP socket, acquired with [`TcpBind`]. Acquire [`Accept`]
/// from it to handle a connection.
#[derive(Debug)]
pub struct TcpListener(std::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(std::net::UdpSocket);

crate::io_ops!(TcpStream);

impl TcpStream {
    /// Flushes and shuts down both directions. A peer that has already
    /// closed its end leaves nothing to shut down, which counts as success.
    fn commit(self) -> io::Result<()> {
        io::Write::flush(&mut &self.0)?;
        match self.0.shutdown(std::net::Shutdown::Both) {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            result => result,
        }
    }
}

impl Inspect<Resolve> for Std {
    fn inspect(&self, op: Resolve) -> io::Result<Vec<SocketAddr>> {
        Ok((op.host.as_str(), op.port).to_socket_addrs()?.collect())
    }
}

impl Provide<TcpConnect<TcpStream>> for Std {
    fn provide(&mut self, op: TcpConnect<TcpStream>, _: Token) -> io::Result<TcpStream> {
        std::net::TcpStream::connect(op.addr).map(TcpStream)
    }

    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit()
    }
}

impl Provide<TcpBind<TcpListener>> for Std {
    fn provide(&mut self, op: TcpBind<TcpListener>, _: Token) -> io::Result<TcpListener> {
        std::net::TcpListener::bind(op.addr).map(TcpListener)
    }

    fn close(&mut self, _: TcpListener) -> io::Result<()> {
        Ok(())
    }
}

impl Provide<UdpBind<UdpSocket>> for Std {
    fn provide(&mut self, op: UdpBind<UdpSocket>, _: Token) -> io::Result<UdpSocket> {
        std::net::UdpSocket::bind(op.addr).map(UdpSocket)
    }

    fn close(&mut self, _: UdpSocket) -> io::Result<()> {
        Ok(())
    }
}

impl Provide<Accept<TcpStream>> for TcpListener {
    fn provide(&mut self, _: Accept<TcpStream>, _: Token) -> io::Result<TcpStream> {
        self.0.accept().map(|(stream, _)| TcpStream(stream))
    }

    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit()
    }
}

/// Accepts a connection and handles it on a new thread, which owns the
/// stream: a thread per connection. `body` is a closure; when it returns
/// `Ok` the stream is shut down cleanly, and otherwise just closed.
///
/// Closing the provided handle joins the thread; perform `Join` on it to
/// see the body's result.
#[cfg(feature = "rt")]
impl<B, T, E> Provide<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>> for TcpListener
where
    B: FnOnce(&mut TcpStream) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    fn provide(
        &mut self,
        op: SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>,
        _: Token,
    ) -> io::Result<JoinHandle<Result<T, E>>> {
        let (stream, _) = self.0.accept()?;
        let body = op.body;
        Ok(crate::task::spawn(
            move || {
                let mut stream = TcpStream(stream);
                let value = body(&mut stream)?;
                stream.commit()?;
                Ok(value)
            },
            None,
        ))
    }

    fn close(&mut self, task: JoinHandle<Result<T, E>>) -> io::Result<()> {
        task.finish().map_err(io::Error::from)
    }
}

impl Perform<Shutdown> for TcpStream {
    fn perform(&mut self, op: Shutdown) -> io::Result<()> {
        self.0.shutdown(op.0)
    }
}

impl Perform<SendTo<'_>> for UdpSocket {
    fn perform(&mut self, op: SendTo<'_>) -> io::Result<usize> {
        self.0.send_to(op.buf, op.addr)
    }
}

impl Perform<RecvFrom<'_>> for UdpSocket {
    fn perform(&mut self, op: RecvFrom<'_>) -> io::Result<(usize, SocketAddr)> {
        self.0.recv_from(op.0)
    }
}

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
