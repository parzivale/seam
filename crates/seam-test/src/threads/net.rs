//! Sockets for simulated threads, whose reads and accepts block.
//!
//! These wrap the in-memory sockets of [`crate::net`]. Writing never
//! blocks, since the simulated network has no buffer limit. Reading a
//! stream with no data yet, accepting with no connection pending, and
//! receiving with no datagram block the thread until another one makes it
//! possible. Every operation is a scheduling point.
#![allow(clippy::disallowed_types)] // sockets keep the scheduler

use core::net::SocketAddr;
use std::io;
use std::sync::Arc;

use seam_core::ops::io::{Flush, Read, ReadToEnd, Write, WriteAll};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::ops::task::SpawnWith;
use seam_core::{Inspect, Perform, Provide, Token};

use super::task::spawn;
use super::{JoinHandle, Shared, ThreadSim};
use crate::net as sim;

/// A connected TCP stream between simulated threads, acquired with
/// [`TcpConnect`] or [`Accept`]. Reads block for data or end of input.
///
/// When its scope succeeds its write half is shut down, so the peer sees
/// end of input.
pub struct TcpStream(sim::TcpStream, Arc<Shared>);

/// A listening TCP socket for simulated threads, acquired with
/// [`TcpBind`]. Accepts block for a connection.
pub struct TcpListener(sim::TcpListener, Arc<Shared>);

/// A UDP socket for simulated threads, acquired with [`UdpBind`].
/// Receives block for a datagram.
pub struct UdpSocket(sim::UdpSocket, Arc<Shared>);

macro_rules! debug_as_inner {
    ($($ty:ident),*) => {$(
        impl core::fmt::Debug for $ty {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_tuple(stringify!($ty)).field(&self.0).finish()
            }
        }
    )*};
}

debug_as_inner!(TcpStream, TcpListener, UdpSocket);

impl Provide<TcpBind<TcpListener>> for ThreadSim {
    fn provide(&mut self, op: TcpBind<TcpListener>, token: Token) -> io::Result<TcpListener> {
        self.shared.switch();
        Provide::provide(
            &mut *self.shared.sim(),
            TcpBind::<sim::TcpListener>::new(op.addr),
            token,
        )
        .map(|l| TcpListener(l, Arc::clone(&self.shared)))
    }

    fn close(&mut self, listener: TcpListener) -> io::Result<()> {
        self.shared.switch();
        Provide::<TcpBind<sim::TcpListener>>::close(&mut *self.shared.sim(), listener.0)
    }
}

impl Provide<TcpConnect<TcpStream>> for ThreadSim {
    fn provide(&mut self, op: TcpConnect<TcpStream>, token: Token) -> io::Result<TcpStream> {
        self.shared.switch();
        Provide::provide(
            &mut *self.shared.sim(),
            TcpConnect::<sim::TcpStream>::new(op.addr),
            token,
        )
        .map(|s| TcpStream(s, Arc::clone(&self.shared)))
    }

    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        self.shared.switch();
        stream.0.commit()
    }
}

impl Provide<UdpBind<UdpSocket>> for ThreadSim {
    fn provide(&mut self, op: UdpBind<UdpSocket>, token: Token) -> io::Result<UdpSocket> {
        self.shared.switch();
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
    /// Blocks for a connection, then takes it.
    fn accept(&mut self, token: Token) -> io::Result<TcpStream> {
        self.1.switch();
        self.1
            .wait_until(|w| self.0.poll_acceptable(w).then_some(()));
        self.0
            .provide(Accept::new(), token)
            .map(|s| TcpStream(s, Arc::clone(&self.1)))
    }
}

impl Provide<Accept<TcpStream>> for TcpListener {
    fn provide(&mut self, _: Accept<TcpStream>, token: Token) -> io::Result<TcpStream> {
        self.accept(token)
    }

    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        self.1.switch();
        stream.0.commit()
    }
}

/// Blocks for a connection and runs `body` with it on a new thread, which
/// owns the stream. On that thread, the stream is shut down when the body
/// returns `Ok`, and just closed otherwise.
///
/// Closing the provided handle joins the thread; perform `Join` on it to
/// see the body's result.
impl<B, T, E> Provide<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>> for TcpListener
where
    B: FnOnce(&mut TcpStream) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    fn provide(
        &mut self,
        op: SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>,
        token: Token,
    ) -> io::Result<JoinHandle<Result<T, E>>> {
        let mut stream = self.accept(token)?;
        let body = op.body;
        Ok(spawn(
            &self.1,
            move || {
                let value = body(&mut stream)?;
                stream.1.switch();
                stream.0.commit()?;
                Ok(value)
            },
            None,
        ))
    }

    fn close(&mut self, thread: JoinHandle<Result<T, E>>) -> io::Result<()> {
        thread.finish().map_err(io::Error::from)
    }
}

impl TcpStream {
    /// Blocks until a read would return: data has arrived, or the stream
    /// has ended.
    fn readable(&self) {
        self.1.switch();
        self.1.wait_until(|w| self.0.poll_readable(w).then_some(()));
    }
}

impl Perform<Read<'_>> for TcpStream {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        self.readable();
        self.0.perform(op)
    }
}

/// Reads until the peer closes its end, as a real stream does.
impl Perform<ReadToEnd<'_>> for TcpStream {
    fn perform(&mut self, op: ReadToEnd<'_>) -> io::Result<usize> {
        let mut total = 0;
        loop {
            self.readable();
            let n = self.0.perform(ReadToEnd(&mut *op.0))?;
            total += n;
            if n == 0 {
                return Ok(total);
            }
        }
    }
}

macro_rules! switch_then {
    ($($ty:ty: $op:ty => $out:ty;)*) => {$(
        impl Perform<$op> for $ty {
            fn perform(&mut self, op: $op) -> $out {
                self.1.switch();
                self.0.perform(op)
            }
        }
    )*};
}

switch_then! {
    TcpStream: Write<'_> => io::Result<usize>;
    TcpStream: WriteAll<'_> => io::Result<()>;
    TcpStream: Flush => io::Result<()>;
    TcpStream: Shutdown => io::Result<()>;
    UdpSocket: SendTo<'_> => io::Result<usize>;
}

impl Perform<RecvFrom<'_>> for UdpSocket {
    fn perform(&mut self, op: RecvFrom<'_>) -> io::Result<(usize, SocketAddr)> {
        self.1.switch();
        self.1
            .wait_until(|w| self.0.poll_receivable(w).then_some(()));
        self.0.perform(op)
    }
}

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
