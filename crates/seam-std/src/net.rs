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
        Ok(spawn_connection(stream, op.body))
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

/// Timeouts, through the sockets' own read and write timeouts. A timeout
/// of zero polls without blocking. `Accept` has no timeout in std, so a
/// timed accept polls the listener, sleeping up to 10ms between tries.
#[cfg(feature = "time")]
mod timeouts {
    use std::io::{Read as _, Write as _};
    use std::time::{Duration, Instant};

    use seam_core::ops::io::{Read, ReadToEnd, Write, WriteAll};
    use seam_core::ops::time::{Timeout, TimesOut};

    use super::*;

    /// When a timed operation must give up; `None` if never (the timeout
    /// is too far away to represent).
    struct Deadline(Option<Instant>);

    impl Deadline {
        fn after(d: Duration) -> Self {
            Self(Instant::now().checked_add(d))
        }

        /// The time left, or `None` if there is no deadline.
        fn left(&self) -> Option<Duration> {
            self.0.map(|d| d.saturating_duration_since(Instant::now()))
        }
    }

    fn timed_out() -> io::Error {
        io::ErrorKind::TimedOut.into()
    }

    /// A socket whose blocking calls can be given a timeout.
    trait Timed {
        fn set_read(&self, t: Option<Duration>) -> io::Result<()>;
        fn set_write(&self, t: Option<Duration>) -> io::Result<()>;
        fn set_nonblocking(&self, on: bool) -> io::Result<()>;
    }

    macro_rules! timed {
        ($($ty:ty),*) => {$(
            impl Timed for $ty {
                fn set_read(&self, t: Option<Duration>) -> io::Result<()> {
                    self.set_read_timeout(t)
                }
                fn set_write(&self, t: Option<Duration>) -> io::Result<()> {
                    self.set_write_timeout(t)
                }
                fn set_nonblocking(&self, on: bool) -> io::Result<()> {
                    <$ty>::set_nonblocking(self, on)
                }
            }
        )*};
    }

    timed!(std::net::TcpStream, std::net::UdpSocket);

    #[derive(Clone, Copy)]
    enum Dir {
        Read,
        Write,
    }

    /// Runs `f` on `sock` with `left` as its timeout in direction `dir`
    /// (`None`: no timeout), then clears it. A socket that times out
    /// reports `TimedOut`, whatever the platform calls it.
    fn within<S: Timed, R>(
        sock: &S,
        dir: Dir,
        left: Option<Duration>,
        f: impl FnOnce(&S) -> io::Result<R>,
    ) -> io::Result<R> {
        let set = |t| match dir {
            Dir::Read => sock.set_read(t),
            Dir::Write => sock.set_write(t),
        };
        let result = match left {
            None => f(sock),
            Some(left) if left.is_zero() => {
                sock.set_nonblocking(true)?;
                let r = f(sock);
                sock.set_nonblocking(false)?;
                r
            }
            Some(left) => {
                set(Some(left))?;
                let r = f(sock);
                set(None)?;
                r
            }
        };
        result.map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => timed_out(),
            _ => e,
        })
    }

    impl Perform<Timeout<Read<'_>>> for TcpStream {
        fn perform(&mut self, op: Timeout<Read<'_>>) -> io::Result<usize> {
            let buf = op.op.0;
            within(&self.0, Dir::Read, Some(op.after), |mut s| s.read(buf))
        }
    }

    impl Perform<Timeout<ReadToEnd<'_>>> for TcpStream {
        fn perform(&mut self, op: Timeout<ReadToEnd<'_>>) -> io::Result<usize> {
            let deadline = Deadline::after(op.after);
            let out = op.op.0;
            let mut chunk = [0; 8192];
            let mut total = 0;
            loop {
                let n = match within(&self.0, Dir::Read, deadline.left(), |mut s| {
                    s.read(&mut chunk)
                }) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    r => r?,
                };
                if n == 0 {
                    return Ok(total);
                }
                out.extend_from_slice(&chunk[..n]);
                total += n;
            }
        }
    }

    impl Perform<Timeout<Write<'_>>> for TcpStream {
        fn perform(&mut self, op: Timeout<Write<'_>>) -> io::Result<usize> {
            let buf = op.op.0;
            within(&self.0, Dir::Write, Some(op.after), |mut s| s.write(buf))
        }
    }

    impl Perform<Timeout<WriteAll<'_>>> for TcpStream {
        fn perform(&mut self, op: Timeout<WriteAll<'_>>) -> io::Result<()> {
            let deadline = Deadline::after(op.after);
            let mut buf = op.op.0;
            while !buf.is_empty() {
                match within(&self.0, Dir::Write, deadline.left(), |mut s| s.write(buf)) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(n) => buf = &buf[n..],
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }

    impl Perform<Timeout<RecvFrom<'_>>> for UdpSocket {
        fn perform(&mut self, op: Timeout<RecvFrom<'_>>) -> io::Result<(usize, SocketAddr)> {
            let buf = op.op.0;
            within(&self.0, Dir::Read, Some(op.after), |s| s.recv_from(buf))
        }
    }

    impl Provide<Timeout<TcpConnect<TcpStream>>> for Std {
        fn provide(
            &mut self,
            op: Timeout<TcpConnect<TcpStream>>,
            _: Token,
        ) -> io::Result<TcpStream> {
            if op.after.is_zero() {
                return Err(op.op.timed_out()); // a connection cannot be made instantly
            }
            std::net::TcpStream::connect_timeout(&op.op.addr, op.after)
                .map_err(|e| match e.kind() {
                    io::ErrorKind::WouldBlock => timed_out(),
                    _ => e,
                })
                .map(TcpStream)
        }

        fn close(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit()
        }
    }

    impl TcpListener {
        /// Accepts a connection, polling until the deadline.
        fn accept_within(&self, after: Duration) -> io::Result<std::net::TcpStream> {
            let deadline = Deadline::after(after);
            self.0.set_nonblocking(true)?;
            let mut nap = Duration::from_micros(500);
            let accepted = loop {
                match self.0.accept() {
                    Ok((stream, _)) => break Ok(stream),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => break Err(e),
                }
                let left = deadline.left();
                if left.is_some_and(|l| l.is_zero()) {
                    break Err(timed_out());
                }
                std::thread::sleep(left.map_or(nap, |l| l.min(nap)));
                nap = (nap * 2).min(Duration::from_millis(10));
            };
            self.0.set_nonblocking(false)?;
            let stream = accepted?;
            // Some platforms hand out accepted sockets nonblocking too.
            stream.set_nonblocking(false)?;
            Ok(stream)
        }
    }

    impl Provide<Timeout<Accept<TcpStream>>> for TcpListener {
        fn provide(&mut self, op: Timeout<Accept<TcpStream>>, _: Token) -> io::Result<TcpStream> {
            self.accept_within(op.after).map(TcpStream)
        }

        fn close(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit()
        }
    }

    /// Waits at most the timeout for a connection, then handles it on a
    /// new thread, as `SpawnWith` without a timeout does.
    #[cfg(feature = "rt")]
    impl<B, T, E> Provide<Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>>>
        for TcpListener
    where
        B: FnOnce(&mut TcpStream) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<io::Error> + Send + 'static,
    {
        fn provide(
            &mut self,
            op: Timeout<SpawnWith<Accept<TcpStream>, B, JoinHandle<Result<T, E>>>>,
            _: Token,
        ) -> io::Result<JoinHandle<Result<T, E>>> {
            let stream = self.accept_within(op.after)?;
            Ok(spawn_connection(stream, op.op.body))
        }

        fn close(&mut self, task: JoinHandle<Result<T, E>>) -> io::Result<()> {
            task.finish().map_err(io::Error::from)
        }
    }
}

/// Handles `stream` on a new thread with `body`, shutting it down cleanly
/// if `body` succeeds.
#[cfg(feature = "rt")]
fn spawn_connection<B, T, E>(stream: std::net::TcpStream, body: B) -> JoinHandle<Result<T, E>>
where
    B: FnOnce(&mut TcpStream) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    crate::task::spawn(
        move || {
            let mut stream = TcpStream(stream);
            let value = body(&mut stream)?;
            stream.commit()?;
            Ok(value)
        },
        None,
    )
}
