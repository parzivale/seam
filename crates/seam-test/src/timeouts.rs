//! Timeouts in the plain simulation.
//!
//! Used directly, nothing in a `Sim` waits, so nothing can arrive during a
//! timeout either: a timed operation that cannot finish at once expires at
//! once, without moving the clock. Blocking I/O reports `TimedOut` rather
//! than `WouldBlock`, a send to a full channel `Full`, and a receive from
//! an empty one `Empty`. The runtime and the thread scheduler wait for real,
//! on the virtual clock.

#[allow(unused_imports)]
use seam_core::ops::time::Timeout;
#[allow(unused_imports)]
use seam_core::{Perform, Provide, Token};

/// Turns a `WouldBlock` into the `TimedOut` of an expired wait.
#[allow(dead_code)]
fn expired<T>(result: seam_core::io::Result<T>) -> seam_core::io::Result<T> {
    result.map_err(|e| match e.kind() {
        seam_core::io::ErrorKind::WouldBlock => seam_core::io::ErrorKind::TimedOut.into(),
        _ => e,
    })
}

#[cfg(feature = "net")]
mod net {
    use seam_core::io;
    use std::net::SocketAddr;

    use seam_core::ops::io::{Read, ReadToEnd, Write, WriteAll};
    use seam_core::ops::net::{Accept, RecvFrom, TcpConnect};

    use super::*;
    use crate::Sim;
    use crate::net::{TcpListener, TcpStream, UdpSocket};

    macro_rules! at_once {
        ($($ty:ty: $op:ty => $out:ty;)*) => {$(
            impl Perform<Timeout<$op>> for $ty {
                fn perform(&mut self, op: Timeout<$op>) -> $out {
                    expired(self.perform(op.op))
                }
            }
        )*};
    }

    at_once! {
        TcpStream: Read<'_> => io::Result<usize>;
        TcpStream: ReadToEnd<'_> => io::Result<usize>;
        TcpStream: Write<'_> => io::Result<usize>;
        TcpStream: WriteAll<'_> => io::Result<()>;
        UdpSocket: RecvFrom<'_> => io::Result<(usize, SocketAddr)>;
    }

    impl Provide<Timeout<Accept<TcpStream>>> for TcpListener {
        fn provide(
            &mut self,
            op: Timeout<Accept<TcpStream>>,
            token: Token,
        ) -> io::Result<TcpStream> {
            expired(self.provide(op.op, token))
        }

        fn close(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit()
        }
    }

    /// Connecting never waits in the simulation, so the timeout is moot.
    impl Provide<Timeout<TcpConnect<TcpStream>>> for Sim {
        fn provide(
            &mut self,
            op: Timeout<TcpConnect<TcpStream>>,
            token: Token,
        ) -> io::Result<TcpStream> {
            self.provide(op.op, token)
        }

        fn close(&mut self, stream: TcpStream) -> io::Result<()> {
            stream.commit()
        }
    }
}

#[cfg(feature = "signal")]
impl Perform<Timeout<seam_core::ops::signal::CtrlC>> for crate::Sim {
    fn perform(&mut self, op: Timeout<seam_core::ops::signal::CtrlC>) -> seam_core::io::Result<()> {
        expired(self.perform(op.op))
    }
}

#[cfg(feature = "sync")]
mod sync {
    use seam_core::ops::sync::{RecvError, RecvMsg, SendError, SendMsg};

    use super::*;
    use crate::sync::{Receiver, Sender};

    impl<T> Perform<Timeout<SendMsg<T>>> for Sender<T> {
        fn perform(&mut self, op: Timeout<SendMsg<T>>) -> Result<(), SendError<T>> {
            self.perform(op.op)
        }
    }

    impl<T> Perform<Timeout<RecvMsg<T>>> for Receiver<T> {
        fn perform(&mut self, op: Timeout<RecvMsg<T>>) -> Result<T, RecvError> {
            self.perform(op.op)
        }
    }
}
