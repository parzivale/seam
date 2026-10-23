//! The in-memory network, and the socket handles.
//!
//! Sockets bound in one [`Sim`] can reach each other. TCP connections are
//! pairs of byte pipes, and UDP datagrams to an address nobody has bound are
//! dropped, as on a real network. Nothing blocks: reading an empty stream,
//! or accepting with no connection pending, fails with `WouldBlock`.

use seam_core::io::{self, ErrorKind};
use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use seam_core::ops::net::How;
use std::string::String;
use std::task::Waker;
use std::vec::Vec;

use seam_core::ops::io::{Flush, Read, ReadToEnd, Write, WriteAll};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::{Inspect, Perform, Provide, Token};

use crate::world::Shared;
use crate::{Sim, error};

/// One direction of a TCP connection.
#[derive(Debug, Default)]
struct Pipe {
    bytes: VecDeque<u8>,
    /// The writer is gone or shut down: drained reads return end of input.
    writer_closed: bool,
    /// The reader is gone or shut down: writes fail with `BrokenPipe`.
    reader_closed: bool,
}

/// A connection waiting to be accepted: the server's ends of its pipes.
#[derive(Debug)]
struct Pending {
    rx: usize,
    tx: usize,
    peer: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct Net {
    hosts: BTreeMap<String, Vec<IpAddr>>,
    next_port: u16,
    listeners: BTreeMap<SocketAddr, VecDeque<Pending>>,
    tcp_ports: BTreeMap<SocketAddr, ()>,
    udp: BTreeMap<SocketAddr, VecDeque<(Vec<u8>, SocketAddr)>>,
    pipes: Vec<Pipe>,
    /// Tasks of the simulated runtime waiting for something to change.
    waiters: BTreeMap<WaitKey, Vec<Waker>>,
}

/// What a task of the simulated runtime can wait for on the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum WaitKey {
    /// Data, or end of input, on a pipe.
    Read(usize),
    /// A connection to a listener bound to this address.
    Accept(SocketAddr),
    /// A datagram for a UDP socket bound to this address.
    Recv(SocketAddr),
}

impl TcpStream {
    /// Whether a read would return now: data has arrived, or the stream
    /// has ended. If not, `waker` is woken when that changes.
    #[cfg_attr(not(feature = "rt"), allow(dead_code))]
    pub(crate) fn poll_readable(&self, waker: &Waker) -> bool {
        let mut world = self.world.lock();
        let pipe = &world.net.pipes[self.rx];
        let ready = !pipe.bytes.is_empty() || pipe.writer_closed || pipe.reader_closed;
        if !ready {
            world.net.wait(WaitKey::Read(self.rx), waker);
        }
        ready
    }
}

impl TcpListener {
    /// Whether a connection is waiting to be accepted. If not, `waker` is
    /// woken when one arrives.
    #[cfg_attr(not(feature = "rt"), allow(dead_code))]
    pub(crate) fn poll_acceptable(&self, waker: &Waker) -> bool {
        let mut world = self.world.lock();
        let ready = world
            .net
            .listeners
            .get(&self.addr)
            .is_some_and(|b| !b.is_empty());
        if !ready {
            world.net.wait(WaitKey::Accept(self.addr), waker);
        }
        ready
    }
}

impl UdpSocket {
    /// Whether a datagram is waiting. If not, `waker` is woken when one
    /// arrives.
    #[cfg_attr(not(feature = "rt"), allow(dead_code))]
    pub(crate) fn poll_receivable(&self, waker: &Waker) -> bool {
        let mut world = self.world.lock();
        let ready = world.net.udp.get(&self.addr).is_some_and(|q| !q.is_empty());
        if !ready {
            world.net.wait(WaitKey::Recv(self.addr), waker);
        }
        ready
    }
}

impl Net {
    /// Registers `waker` to be woken when `key` changes.
    pub(crate) fn wait(&mut self, key: WaitKey, waker: &Waker) {
        let waiters = self.waiters.entry(key).or_default();
        if !waiters.iter().any(|w| w.will_wake(waker)) {
            waiters.push(waker.clone());
        }
    }

    fn wake(&mut self, key: WaitKey) {
        for waker in self.waiters.remove(&key).into_iter().flatten() {
            waker.wake();
        }
    }
}

impl Default for Net {
    fn default() -> Self {
        Self {
            hosts: BTreeMap::from([(
                String::from("localhost"),
                Vec::from([IpAddr::from(Ipv4Addr::LOCALHOST)]),
            )]),
            next_port: 49152,
            listeners: BTreeMap::new(),
            tcp_ports: BTreeMap::new(),
            udp: BTreeMap::new(),
            pipes: Vec::new(),
            waiters: BTreeMap::new(),
        }
    }
}

/// Whether a socket bound to `bound` receives traffic sent to `to`.
fn reaches(bound: SocketAddr, to: SocketAddr) -> bool {
    bound.port() == to.port() && (bound.ip().is_unspecified() || bound.ip() == to.ip())
}

impl Net {
    /// Picks a free port when `addr` asks for port 0.
    fn assign(
        &mut self,
        mut addr: SocketAddr,
        taken: impl Fn(&Self, SocketAddr) -> bool,
    ) -> io::Result<SocketAddr> {
        if addr.port() == 0 {
            for _ in 0..16384 {
                let port = self.next_port;
                self.next_port = if port == u16::MAX { 49152 } else { port + 1 };
                addr.set_port(port);
                if !taken(self, addr) {
                    return Ok(addr);
                }
            }
            return Err(error(ErrorKind::AddrInUse, "no free ports"));
        }
        if taken(self, addr) {
            return Err(error(ErrorKind::AddrInUse, "address in use"));
        }
        Ok(addr)
    }

    fn tcp_taken(&self, addr: SocketAddr) -> bool {
        self.tcp_ports.keys().any(|&b| b.port() == addr.port())
    }

    fn udp_taken(&self, addr: SocketAddr) -> bool {
        self.udp.keys().any(|&b| b.port() == addr.port())
    }

    fn pipe(&mut self) -> usize {
        self.pipes.push(Pipe::default());
        self.pipes.len() - 1
    }
}

impl Sim {
    /// Makes `name` resolve to `ip`, in addition to any addresses it
    /// already has. `localhost` resolves to 127.0.0.1 by default.
    pub fn add_host(&mut self, name: impl Into<String>, ip: impl Into<IpAddr>) -> &mut Self {
        self.world
            .lock()
            .net
            .hosts
            .entry(name.into())
            .or_default()
            .push(ip.into());
        self
    }
}

impl Inspect<Resolve> for Sim {
    fn inspect(&self, op: Resolve) -> io::Result<Vec<SocketAddr>> {
        if let Ok(ip) = op.host.parse::<IpAddr>() {
            return Ok(Vec::from([SocketAddr::new(ip, op.port)]));
        }
        let world = self.world.lock();
        let ips = world
            .net
            .hosts
            .get(&op.host)
            .ok_or_else(|| error(ErrorKind::NotFound, "unknown host"))?;
        Ok(ips.iter().map(|&ip| SocketAddr::new(ip, op.port)).collect())
    }
}

impl Provide<TcpBind<TcpListener>> for Sim {
    fn close(&mut self, _: TcpListener) -> io::Result<()> {
        Ok(())
    }

    fn provide(&mut self, op: TcpBind<TcpListener>, _: Token) -> io::Result<TcpListener> {
        let mut world = self.world.lock();
        let net = &mut world.net;
        let addr = net.assign(op.addr, Net::tcp_taken)?;
        net.tcp_ports.insert(addr, ());
        net.listeners.insert(addr, VecDeque::new());
        Ok(TcpListener {
            world: self.world.clone(),
            addr,
        })
    }
}

impl Provide<TcpConnect<TcpStream>> for Sim {
    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit()
    }

    fn provide(&mut self, op: TcpConnect<TcpStream>, _: Token) -> io::Result<TcpStream> {
        let mut world = self.world.lock();
        let net = &mut world.net;
        let Some(&listener) = net.listeners.keys().find(|&&b| reaches(b, op.addr)) else {
            return Err(error(ErrorKind::ConnectionRefused, "connection refused"));
        };
        let local_ip = if op.addr.ip().is_unspecified() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            op.addr.ip()
        };
        let local = net.assign(SocketAddr::new(local_ip, 0), Net::tcp_taken)?;
        net.tcp_ports.insert(local, ());
        let (up, down) = (net.pipe(), net.pipe());
        let backlog = net
            .listeners
            .get_mut(&listener)
            .expect("listener was just found");
        backlog.push_back(Pending {
            rx: up,
            tx: down,
            peer: local,
        });
        net.wake(WaitKey::Accept(listener));
        Ok(TcpStream {
            world: self.world.clone(),
            rx: down,
            tx: up,
            local,
            peer: op.addr,
            owns_port: true,
        })
    }
}

impl Provide<UdpBind<UdpSocket>> for Sim {
    fn close(&mut self, _: UdpSocket) -> io::Result<()> {
        Ok(())
    }

    fn provide(&mut self, op: UdpBind<UdpSocket>, _: Token) -> io::Result<UdpSocket> {
        let mut world = self.world.lock();
        let addr = world.net.assign(op.addr, Net::udp_taken)?;
        world.net.udp.insert(addr, VecDeque::new());
        Ok(UdpSocket {
            world: self.world.clone(),
            addr,
        })
    }
}

/// A connected in-memory TCP stream. Read and write it with the `io`
/// operations.
#[derive(Debug)]
pub struct TcpStream {
    pub(crate) world: Shared,
    pub(crate) rx: usize,
    tx: usize,
    local: SocketAddr,
    peer: SocketAddr,
    /// Client ends own an ephemeral port; accepted ends share the
    /// listener's.
    owns_port: bool,
}

/// A listening in-memory TCP socket. Perform [`Accept`] on it to get
/// connections.
#[derive(Debug)]
pub struct TcpListener {
    pub(crate) world: Shared,
    pub(crate) addr: SocketAddr,
}

/// An in-memory UDP socket.
#[derive(Debug)]
pub struct UdpSocket {
    pub(crate) world: Shared,
    pub(crate) addr: SocketAddr,
}

impl TcpStream {
    fn read_with<R>(&mut self, f: impl FnOnce(&mut VecDeque<u8>) -> R) -> io::Result<R> {
        let mut world = self.world.lock();
        let pipe = &mut world.net.pipes[self.rx];
        if pipe.bytes.is_empty() && !pipe.writer_closed && !pipe.reader_closed {
            return Err(error(
                ErrorKind::WouldBlock,
                "no data yet, and nothing in the simulation waits",
            ));
        }
        Ok(f(&mut pipe.bytes))
    }

    fn write_bytes(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut world = self.world.lock();
        let pipe = &mut world.net.pipes[self.tx];
        if pipe.writer_closed {
            return Err(error(
                ErrorKind::BrokenPipe,
                "stream was shut down for writing",
            ));
        }
        if pipe.reader_closed {
            return Err(error(
                ErrorKind::BrokenPipe,
                "the peer closed the connection",
            ));
        }
        pipe.bytes.extend(buf);
        world.net.wake(WaitKey::Read(self.tx));
        Ok(buf.len())
    }
}

impl Perform<Read<'_>> for TcpStream {
    fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
        self.read_with(|bytes| {
            let n = bytes.len().min(op.0.len());
            for (dst, src) in op.0.iter_mut().zip(bytes.drain(..n)) {
                *dst = src;
            }
            n
        })
    }
}

/// Reads what has arrived. Unlike a real stream it does not wait for the
/// peer to close; it fails with `WouldBlock` only if nothing has arrived.
impl Perform<ReadToEnd<'_>> for TcpStream {
    fn perform(&mut self, op: ReadToEnd<'_>) -> io::Result<usize> {
        self.read_with(|bytes| {
            let n = bytes.len();
            op.0.extend(bytes.drain(..));
            n
        })
    }
}

impl Perform<Write<'_>> for TcpStream {
    fn perform(&mut self, op: Write<'_>) -> io::Result<usize> {
        self.write_bytes(op.0)
    }
}

impl Perform<WriteAll<'_>> for TcpStream {
    fn perform(&mut self, op: WriteAll<'_>) -> io::Result<()> {
        self.write_bytes(op.0).map(drop)
    }
}

impl Perform<Flush> for TcpStream {
    fn perform(&mut self, _: Flush) -> io::Result<()> {
        Ok(())
    }
}

impl Perform<Shutdown> for TcpStream {
    fn perform(&mut self, op: Shutdown) -> io::Result<()> {
        let mut world = self.world.lock();
        let pipes = &mut world.net.pipes;
        if matches!(op.0, How::Read | How::Both) {
            pipes[self.rx].reader_closed = true;
            pipes[self.rx].bytes.clear();
        }
        if matches!(op.0, How::Write | How::Both) {
            pipes[self.tx].writer_closed = true;
        }
        world.net.wake(WaitKey::Read(self.rx));
        world.net.wake(WaitKey::Read(self.tx));
        Ok(())
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let mut world = self.world.lock();
        let net = &mut world.net;
        net.pipes[self.rx].reader_closed = true;
        net.pipes[self.rx].bytes.clear();
        net.pipes[self.tx].writer_closed = true;
        if self.owns_port {
            net.tcp_ports.remove(&self.local);
        }
        net.wake(WaitKey::Read(self.rx));
        net.wake(WaitKey::Read(self.tx));
    }
}

impl Provide<Accept<TcpStream>> for TcpListener {
    fn close(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.commit()
    }

    fn provide(&mut self, _: Accept<TcpStream>, _: Token) -> io::Result<TcpStream> {
        let mut world = self.world.lock();
        let backlog = world
            .net
            .listeners
            .get_mut(&self.addr)
            .expect("listener is registered while it lives");
        let Pending { rx, tx, peer } = backlog.pop_front().ok_or_else(|| {
            error(
                ErrorKind::WouldBlock,
                "no pending connection, and nothing in the simulation waits",
            )
        })?;
        let stream = TcpStream {
            world: self.world.clone(),
            rx,
            tx,
            local: self.addr,
            peer,
            owns_port: false,
        };
        Ok(stream)
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let mut world = self.world.lock();
        let net = &mut world.net;
        net.tcp_ports.remove(&self.addr);
        // Connections nobody accepted are reset.
        for p in net.listeners.remove(&self.addr).into_iter().flatten() {
            net.pipes[p.rx].reader_closed = true;
            net.pipes[p.tx].writer_closed = true;
            net.wake(WaitKey::Read(p.rx));
            net.wake(WaitKey::Read(p.tx));
        }
        net.wake(WaitKey::Accept(self.addr));
    }
}

impl Perform<SendTo<'_>> for UdpSocket {
    fn perform(&mut self, op: SendTo<'_>) -> io::Result<usize> {
        let mut world = self.world.lock();
        if let Some((&bound, queue)) = world
            .net
            .udp
            .iter_mut()
            .find(|(b, _)| reaches(**b, op.addr))
        {
            queue.push_back((op.buf.to_vec(), self.addr));
            world.net.wake(WaitKey::Recv(bound));
        }
        Ok(op.buf.len())
    }
}

/// Truncates datagrams longer than the buffer, as UDP does.
impl Perform<RecvFrom<'_>> for UdpSocket {
    fn perform(&mut self, op: RecvFrom<'_>) -> io::Result<(usize, SocketAddr)> {
        let mut world = self.world.lock();
        let queue = world
            .net
            .udp
            .get_mut(&self.addr)
            .expect("socket is registered while it lives");
        let (datagram, from) = queue.pop_front().ok_or_else(|| {
            error(
                ErrorKind::WouldBlock,
                "no datagram yet, and nothing in the simulation waits",
            )
        })?;
        let n = datagram.len().min(op.0.len());
        op.0[..n].copy_from_slice(&datagram[..n]);
        Ok((n, from))
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        self.world.lock().net.udp.remove(&self.addr);
    }
}

impl Inspect<LocalAddr> for TcpStream {
    fn inspect(&self, _: LocalAddr) -> io::Result<SocketAddr> {
        Ok(self.local)
    }
}

impl Inspect<PeerAddr> for TcpStream {
    fn inspect(&self, _: PeerAddr) -> io::Result<SocketAddr> {
        Ok(self.peer)
    }
}

impl Inspect<LocalAddr> for TcpListener {
    fn inspect(&self, _: LocalAddr) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

impl Inspect<LocalAddr> for UdpSocket {
    fn inspect(&self, _: LocalAddr) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

/// There is no `connect` for simulated UDP sockets, so they have no peer.
impl Inspect<PeerAddr> for UdpSocket {
    fn inspect(&self, _: PeerAddr) -> io::Result<SocketAddr> {
        Err(error(ErrorKind::NotConnected, "socket is not connected"))
    }
}

impl TcpStream {
    /// Shuts down the write half, so the peer sees end of input.
    pub(crate) fn commit(mut self) -> io::Result<()> {
        self.perform(Shutdown(How::Write))
    }
}

// Every capability type opts in to seam's lifts.
impl seam_core::Capability for TcpListener {}
impl seam_core::Capability for TcpStream {}
impl seam_core::Capability for UdpSocket {}
