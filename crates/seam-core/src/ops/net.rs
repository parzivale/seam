//! Networking.
//!
//! Sockets are acquisitions, generic over the backend's socket types: a
//! core acquires one for the length of a scope (see
//! [`acquire`](crate::acquire)). Reading and writing a connected stream use
//! [`io`](super::io). DNS is an effect too: resolve names with
//! [`ResolveInto`], or with [`resolve`] (`alloc`) to get them all.
//!
//! ```
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_core::ops::io::{Write, write_all};
//! use seam_core::ops::net::{ResolveInto, TcpConnect};
//! use core::net::SocketAddr;
//!
//! /// Sends one line to `host:port`, trying each of the first four
//! /// addresses it resolves to. Allocates nothing.
//! fn notify<C, S>(c: &mut C, host: &str, port: u16, line: &str) -> io::Result<()>
//! where
//!     C: for<'a> Inspect<ResolveInto<'a>> + Provide<TcpConnect<S>>,
//!     S: for<'a> Perform<Write<'a>>,
//! {
//!     let mut addrs = [SocketAddr::from(([0, 0, 0, 0], 0)); 4];
//!     let n = c.inspect(ResolveInto::new(host, port, &mut addrs))?;
//!     let mut last = io::Error::new(io::ErrorKind::NotFound, "no addresses");
//!     for &addr in &addrs[..n.min(addrs.len())] {
//!         // Closing the stream when the scope succeeds shuts it down cleanly.
//!         match c.acquire(TcpConnect::new(addr), |_c, s| write_all(s, line.as_bytes())) {
//!             Ok(()) => return Ok(()),
//!             Err(e) => last = e,
//!         }
//!     }
//!     Err(last)
//! }
//! ```

use crate::io;
#[allow(unused_imports)]
use crate::three_ways::three_ways;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::net::SocketAddr;

/// Resolve a host name into a buffer of socket addresses, without
/// allocating. Returns how many addresses the name has, which can be more
/// than fit: the buffer holds the first ones.
#[derive(Debug)]
pub struct ResolveInto<'a> {
    /// The host name or address literal.
    pub host: &'a str,
    /// The port to put in each address.
    pub port: u16,
    /// Where to put the addresses.
    pub out: &'a mut [SocketAddr],
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

/// Accept a connection on a listener, producing a stream of type `S`.
/// Acquired from the listener; [`PeerAddr`] on the stream says who it is.
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

/// Which half of a stream to shut down: the same choices as
/// `std::net::Shutdown`, which converts into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum How {
    /// Reading: further reads return end of input.
    Read,
    /// Writing: the peer sees end of input.
    Write,
    /// Both.
    Both,
}

#[cfg(feature = "std")]
impl From<std::net::Shutdown> for How {
    fn from(how: std::net::Shutdown) -> Self {
        match how {
            std::net::Shutdown::Read => Self::Read,
            std::net::Shutdown::Write => Self::Write,
            std::net::Shutdown::Both => Self::Both,
        }
    }
}

#[cfg(feature = "std")]
impl From<How> for std::net::Shutdown {
    fn from(how: How) -> Self {
        match how {
            How::Read => Self::Read,
            How::Write => Self::Write,
            How::Both => Self::Both,
        }
    }
}

impl<'a> ResolveInto<'a> {
    /// Describes resolving `host` with `port` into `out`.
    pub fn new(host: &'a str, port: u16, out: &'a mut [SocketAddr]) -> Self {
        Self { host, port, out }
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
    #[query] LocalAddr => io::Result<SocketAddr>;
    #[query] PeerAddr => io::Result<SocketAddr>;
    SendTo<'_> => io::Result<usize>;
    RecvFrom<'_> => io::Result<(usize, SocketAddr)>;
    Shutdown => io::Result<()>;
    #[query] ResolveInto<'_> => io::Result<usize>;
}

impl<S> crate::Acquisition for TcpConnect<S> {
    type Resource = S;
    type Error = io::Error;
}

impl<L> crate::Acquisition for TcpBind<L> {
    type Resource = L;
    type Error = io::Error;
}

impl<S> crate::Acquisition for Accept<S> {
    type Resource = S;
    type Error = io::Error;
}

impl<U> crate::Acquisition for UdpBind<U> {
    type Resource = U;
    type Error = io::Error;
}

/// Each gives up with `io::ErrorKind::TimedOut`.
#[cfg(feature = "time")]
mod timeouts {
    use super::*;
    use crate::ops::time::{TimesOut, io_timed_out};

    /// No connection is taken from the backlog.
    impl<S> TimesOut for Accept<S> {
        type Expired = io::Error;
        fn timed_out(self) -> io::Error {
            io_timed_out()
        }
    }

    impl<S> TimesOut for TcpConnect<S> {
        type Expired = io::Error;
        fn timed_out(self) -> io::Error {
            io_timed_out()
        }
    }

    /// No datagram is taken.
    impl TimesOut for RecvFrom<'_> {
        type Expired = io::Result<(usize, SocketAddr)>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }
}

crate::root::in_kind!(crate::kinds::Net: [S] TcpConnect<S>, [L] TcpBind<L>, [U] UdpBind<U>, ['a] ResolveInto<'a>);

#[cfg(feature = "alloc")]
three_ways! {
    /// Every address `host` resolves to, with `port`. Needs `alloc`;
    /// [`ResolveInto`] is the primitive, filling a buffer instead. Asks
    /// twice if the name has more than 8 addresses.
    pub fn resolve, resolve_async, resolve_send
    [C] (c: &C, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>
    where
        sync { C: for<'a> crate::Inspect<ResolveInto<'a>> }
        async { C: for<'a> crate::future::AsyncInspect<ResolveInto<'a>> }
        send { C: for<'a> crate::future::SendInspect<ResolveInto<'a>> + Sync }
    {
        let unspecified = SocketAddr::from(([0, 0, 0, 0], 0));
        let mut first = [unspecified; 8];
        let n = ask!(c, ResolveInto::new(host, port, &mut first))?;
        if n <= first.len() {
            return Ok(first[..n].to_vec());
        }
        let mut all = alloc::vec![unspecified; n];
        let n = ask!(c, ResolveInto::new(host, port, &mut all))?;
        all.truncate(n);
        Ok(all)
    }
}
