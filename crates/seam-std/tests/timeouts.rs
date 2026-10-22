//! Timeouts on the std backend, against real threads and sockets.
#![cfg(feature = "full")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, TcpBind, TcpConnect};
use seam_core::ops::sync::{Channel, RecvError, RecvMsg, SendError, SendMsg};
use seam_core::ops::task::{Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet};
use seam_core::ops::time::{Sleep, Timeout};
use seam_core::{Acquire, Inspect, Perform};
use seam_std::Std;
use seam_std::net::{TcpListener, TcpStream};
use seam_std::sync::{Receiver, Sender};
use seam_std::task::{JoinHandle, TaskSet};

const SHORT: Duration = Duration::from_millis(50);
const LONG: Duration = Duration::from_secs(10);

/// Runs `f`, checking it took at least `at_least` and well under `LONG`.
fn timed<R>(at_least: Duration, f: impl FnOnce() -> R) -> R {
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let r = f();
    let took = start.elapsed();
    assert!(
        took >= at_least,
        "returned after {took:?}, before the timeout"
    );
    assert!(took < LONG / 2, "took {took:?}");
    r
}

#[test]
fn channels() {
    let mut std = Std::new();
    let (mut tx, mut rx): (Sender<u8>, Receiver<u8>) = std.perform(Channel::bounded(1));

    let got = timed(SHORT, || rx.perform(Timeout::new(SHORT, RecvMsg::new())));
    assert_eq!(got, Err(RecvError::Empty), "nothing arrived");

    tx.perform(SendMsg(1)).unwrap();
    let full = timed(SHORT, || tx.perform(Timeout::new(SHORT, SendMsg(2))));
    assert_eq!(full, Err(SendError::Full(2)), "the value comes back");

    // Zero waits not at all.
    assert_eq!(
        tx.perform(Timeout::new(Duration::ZERO, SendMsg(3))),
        Err(SendError::Full(3))
    );
    assert_eq!(
        rx.perform(Timeout::new(Duration::ZERO, RecvMsg::new())),
        Ok(1)
    );

    // Something that arrives in time is taken, without waiting the timeout.
    let mut late = tx.clone();
    let sender = std::thread::spawn(move || {
        Std::new().perform(Sleep(SHORT));
        late.perform(SendMsg(4)).unwrap();
    });
    assert_eq!(rx.perform(Timeout::new(LONG, RecvMsg::new())), Ok(4));
    sender.join().unwrap();

    drop(tx);
    assert_eq!(
        rx.perform(Timeout::new(LONG, RecvMsg::new())),
        Err(RecvError::Closed)
    );
}

#[test]
fn joins() {
    let mut std = Std::new();
    let slow = Spawn::<_, JoinHandle<u8>>::new(|| {
        Std::new().perform(Sleep(SHORT * 4));
        7
    });
    std.acquire(slow, |_, thread| {
        let first = timed(SHORT, || thread.perform(Timeout::new(SHORT, Join::new())));
        assert_eq!(first, Err(JoinError::TimedOut));
        assert_eq!(
            thread.perform(Timeout::new(LONG, Join::new())),
            Ok(7),
            "joinable again"
        );
        Ok::<_, JoinError>(())
    })
    .unwrap();

    std.acquire(SpawnSet::<TaskSet<u8>>::new(), |_, set| {
        assert_eq!(
            set.perform(Timeout::new(SHORT, JoinNext::new())),
            None,
            "an empty set"
        );
        let _ = set.perform(SpawnInto::new(|| {
            Std::new().perform(Sleep(SHORT * 4));
            1
        }));
        let first = timed(SHORT, || set.perform(Timeout::new(SHORT, JoinNext::new())));
        assert_eq!(first, Some(Err(JoinError::TimedOut)));
        assert_eq!(
            set.perform(Timeout::new(LONG, JoinNext::new())),
            Some(Ok(1))
        );
        Ok::<_, JoinError>(())
    })
    .unwrap();
}

#[test]
fn sockets() -> io::Result<()> {
    let mut std = Std::new();
    let any: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    std.acquire(TcpBind::<TcpListener>::new(any), |std, listener| {
        let addr = listener.inspect(LocalAddr)?;
        let err = timed(SHORT, || {
            listener.acquire(Timeout::new(SHORT, Accept::<TcpStream>::new()), |_, _| {
                io::Result::Ok(())
            })
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "nobody connected");

        std.acquire(
            Timeout::new(LONG, TcpConnect::<TcpStream>::new(addr)),
            |_, client| {
                listener.acquire(
                    Timeout::new(LONG, Accept::<TcpStream>::new()),
                    |_, server| {
                        let mut buf = [0; 8];
                        let err = timed(SHORT, || {
                            server.perform(Timeout::new(SHORT, Read(&mut buf)))
                        })
                        .unwrap_err();
                        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "nothing sent yet");
                        let err = server
                            .perform(Timeout::new(Duration::ZERO, Read(&mut buf)))
                            .unwrap_err();
                        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "zero polls");

                        client.perform(WriteAll(b"hello"))?;
                        let n = server.perform(Timeout::new(LONG, Read(&mut buf)))?;
                        assert_eq!(&buf[..n], b"hello");

                        // ReadToEnd keeps what arrived before the deadline.
                        client.perform(WriteAll(b"partial"))?;
                        let mut all = Vec::new();
                        let err = timed(SHORT, || {
                            server.perform(Timeout::new(SHORT * 2, ReadToEnd(&mut all)))
                        })
                        .unwrap_err();
                        assert_eq!(
                            err.kind(),
                            io::ErrorKind::TimedOut,
                            "the client never closed"
                        );
                        assert_eq!(all, b"partial");

                        // After a timeout the socket blocks normally again.
                        client.perform(WriteAll(b"!"))?;
                        let n = server.perform(Read(&mut buf))?;
                        assert_eq!(&buf[..n], b"!");
                        Ok(())
                    },
                )
            },
        )
    })
}
