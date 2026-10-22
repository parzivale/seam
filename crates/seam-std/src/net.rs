//! Networking operations, and the socket handles.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::{Finalize, Inspect, Perform};

use crate::Std;

/// A connected TCP stream. Read and write it with the `io` operations.
///
/// [`Finalize::close`] shuts down both directions; [`Finalize::abandon`]
/// drops it.
#[derive(Debug)]
pub struct TcpStream(std::net::TcpStream);

/// A listening TCP socket. Perform [`Accept`] on it to get connections.
#[derive(Debug)]
pub struct TcpListener(std::net::TcpListener);

/// A UDP socket.
#[derive(Debug)]
pub struct UdpSocket(std::net::UdpSocket);

crate::io_ops!(TcpStream);

impl Finalize for TcpStream {
    fn close(self) {
        let _ = io::Write::flush(&mut &self.0);
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl Finalize for TcpListener {
    fn close(self) {}
}

impl Finalize for UdpSocket {
    fn close(self) {}
}

impl Inspect<Resolve> for Std {
    fn inspect(&self, op: Resolve) -> io::Result<Vec<SocketAddr>> {
        Ok((op.host.as_str(), op.port).to_socket_addrs()?.collect())
    }
}

impl Perform<TcpConnect<TcpStream>> for Std {
    fn perform(&mut self, op: TcpConnect<TcpStream>) -> io::Result<TcpStream> {
        std::net::TcpStream::connect(op.addr).map(TcpStream)
    }
}

impl Perform<TcpBind<TcpListener>> for Std {
    fn perform(&mut self, op: TcpBind<TcpListener>) -> io::Result<TcpListener> {
        std::net::TcpListener::bind(op.addr).map(TcpListener)
    }
}

impl Perform<UdpBind<UdpSocket>> for Std {
    fn perform(&mut self, op: UdpBind<UdpSocket>) -> io::Result<UdpSocket> {
        std::net::UdpSocket::bind(op.addr).map(UdpSocket)
    }
}

impl Perform<Accept<TcpStream>> for TcpListener {
    fn perform(&mut self, _: Accept<TcpStream>) -> io::Result<(TcpStream, SocketAddr)> {
        let (stream, addr) = self.0.accept()?;
        Ok((TcpStream(stream), addr))
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
