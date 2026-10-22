//! Networking.
//!
//! Operations that create a socket are generic over the backend's socket
//! types. Reading and writing a connected stream use [`io`](super::io).

use core::marker::PhantomData;
use core::net::SocketAddr;
use std::io;
use std::net::Shutdown as How;
use std::string::String;
use std::vec::Vec;

/// Resolve a host name to socket addresses. DNS is an effect too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolve {
    /// The host name or address literal.
    pub host: String,
    /// The port to put in each address.
    pub port: u16,
}

/// Connect a TCP stream, producing a handle of type `S`.
pub struct TcpConnect<S> {
    /// The address to connect to.
    pub addr: SocketAddr,
    handle: PhantomData<fn() -> S>,
}

/// Bind a TCP listener, producing a handle of type `L`.
pub struct TcpBind<L> {
    /// The address to listen on.
    pub addr: SocketAddr,
    handle: PhantomData<fn() -> L>,
}

/// Accept a connection on a listener, producing a stream of type `S` and
/// the peer's address.
pub struct Accept<S> {
    handle: PhantomData<fn() -> S>,
}

/// Bind a UDP socket, producing a handle of type `U`.
pub struct UdpBind<U> {
    /// The address to bind.
    pub addr: SocketAddr,
    handle: PhantomData<fn() -> U>,
}

/// Send a datagram. Returns how many bytes were sent.
#[derive(Debug, Clone, Copy)]
pub struct SendTo<'a> {
    /// The datagram.
    pub buf: &'a [u8],
    /// Where to send it.
    pub addr: SocketAddr,
}

/// Receive a datagram. Returns its length and sender.
#[derive(Debug)]
pub struct RecvFrom<'a>(pub &'a mut [u8]);

/// The socket's own address.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalAddr;

/// The connected peer's address.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerAddr;

/// Shut down the read half, write half, or both halves of a TCP stream.
#[derive(Debug, Clone, Copy)]
pub struct Shutdown(pub How);

impl Resolve {
    /// Describes resolving `host` with `port`.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

macro_rules! addr_ops {
    ($($ty:ident),*) => {$(
        impl<H> $ty<H> {
            /// Describes the operation on `addr`.
            pub fn new(addr: impl Into<SocketAddr>) -> Self {
                Self { addr: addr.into(), handle: PhantomData }
            }
        }

        impl<H> Clone for $ty<H> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<H> Copy for $ty<H> {}

        impl<H> core::fmt::Debug for $ty<H> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!($ty)).field("addr", &self.addr).finish()
            }
        }
    )*};
}
addr_ops!(TcpConnect, TcpBind, UdpBind);

impl<S> Accept<S> {
    /// Describes accepting one connection.
    pub fn new() -> Self {
        Self {
            handle: PhantomData,
        }
    }
}

impl<S> Default for Accept<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Clone for Accept<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S> Copy for Accept<S> {}

impl<S> core::fmt::Debug for Accept<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Accept")
    }
}

operations! {
    #[query] Resolve => io::Result<Vec<SocketAddr>>;
    #[query] LocalAddr => io::Result<SocketAddr>;
    #[query] PeerAddr => io::Result<SocketAddr>;
    SendTo<'_> => io::Result<usize>;
    RecvFrom<'_> => io::Result<(usize, SocketAddr)>;
    Shutdown => io::Result<()>;
}

impl<S> crate::Operation for TcpConnect<S> {
    type Outcome = io::Result<S>;
}

impl<L> crate::Operation for TcpBind<L> {
    type Outcome = io::Result<L>;
}

impl<S> crate::Operation for Accept<S> {
    type Outcome = io::Result<(S, SocketAddr)>;
}

impl<U> crate::Operation for UdpBind<U> {
    type Outcome = io::Result<U>;
}
