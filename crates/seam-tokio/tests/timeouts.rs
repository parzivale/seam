//! Timeouts on a real Tokio runtime.
#![cfg(feature = "full")]

use seam_core::io;
use seam_core::ops::io::write_all_send;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use seam_core::future::SendPerform;
use seam_core::ops::io::Read;
use seam_core::ops::net::{Accept, LocalAddr, TcpBind, TcpConnect};
use seam_core::ops::sync::{Channel, RecvError, RecvMsg, SendError, SendMsg};
use seam_core::ops::task::{Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet};
use seam_core::ops::time::{Sleep, Timeout};
use seam_core::{AsyncAcquire, Inspect, Perform};
use seam_tokio::Tokio;
use seam_tokio::net::{TcpListener, TcpStream};
use seam_tokio::sync::{Receiver, Sender};
use seam_tokio::task::{JoinHandle, TaskSet};

const SHORT: Duration = Duration::from_millis(50);
const LONG: Duration = Duration::from_secs(10);

#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}

#[tokio::test]
async fn channels() {
    let mut tokio = Tokio::new();
    let (mut tx, mut rx): (Sender<u8>, Receiver<u8>) = tokio.perform(Channel::bounded(1));
    let start = now();
    assert_eq!(
        rx.perform_send(Timeout::new(SHORT, RecvMsg::new())).await,
        Err(RecvError::Empty)
    );
    assert!(start.elapsed() >= SHORT);

    tx.perform_send(SendMsg(1)).await.unwrap();
    assert_eq!(
        tx.perform_send(Timeout::new(SHORT, SendMsg(2))).await,
        Err(SendError::Full(2)),
        "the value comes back"
    );
    assert_eq!(
        rx.perform_send(Timeout::new(LONG, RecvMsg::new())).await,
        Ok(1)
    );

    let mut late = tx.clone();
    tokio::spawn(async move {
        Tokio::new().perform_send(Sleep(SHORT)).await;
        late.perform_send(SendMsg(3)).await.unwrap();
    });
    assert_eq!(
        rx.perform_send(Timeout::new(LONG, RecvMsg::new())).await,
        Ok(3)
    );
}

#[tokio::test]
async fn joins() {
    let mut tokio = Tokio::new();
    let slow = Spawn::<_, JoinHandle<u8>>::new(async {
        Tokio::new().perform_send(Sleep(SHORT * 4)).await;
        7
    });
    tokio
        .acquire_async(slow, async |_, task| {
            assert_eq!(
                task.perform_send(Timeout::new(SHORT, Join::new())).await,
                Err(JoinError::TimedOut)
            );
            assert_eq!(
                task.perform_send(Timeout::new(LONG, Join::new())).await,
                Ok(7),
                "joinable again"
            );
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();

    tokio
        .acquire_async(SpawnSet::<TaskSet<u8>>::new(), async |_, set| {
            assert_eq!(
                set.perform_send(Timeout::new(SHORT, JoinNext::new())).await,
                None
            );
            let _ = set
                .perform_send(SpawnInto::new(async {
                    Tokio::new().perform_send(Sleep(SHORT * 4)).await;
                    1
                }))
                .await;
            assert_eq!(
                set.perform_send(Timeout::new(SHORT, JoinNext::new())).await,
                Some(Err(JoinError::TimedOut))
            );
            assert_eq!(
                set.perform_send(Timeout::new(LONG, JoinNext::new())).await,
                Some(Ok(1))
            );
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn sockets() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let any: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
    tokio
        .acquire_async(TcpBind::<TcpListener>::new(any), async |tokio, listener| {
            let addr = listener.inspect(LocalAddr)?;
            let err = listener
                .acquire_async(
                    Timeout::new(SHORT, Accept::<TcpStream>::new()),
                    async |_, _| io::Result::Ok(()),
                )
                .await
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut, "nobody connected");

            tokio
                .acquire_async(
                    Timeout::new(LONG, TcpConnect::<TcpStream>::new(addr)),
                    async |_, client| {
                        listener
                            .acquire_async(
                                Timeout::new(LONG, Accept::<TcpStream>::new()),
                                async |_, server| {
                                    let mut buf = [0; 8];
                                    let err = server
                                        .perform_send(Timeout::new(SHORT, Read(&mut buf)))
                                        .await
                                        .unwrap_err();
                                    assert_eq!(
                                        err.kind(),
                                        io::ErrorKind::TimedOut,
                                        "nothing sent yet"
                                    );

                                    // A timed read returns what has arrived, without
                                    // waiting for the deadline.
                                    write_all_send(client, b"partial").await?;
                                    let n = server
                                        .perform_send(Timeout::new(LONG, Read(&mut buf)))
                                        .await?;
                                    assert_eq!(&buf[..n], b"partial");
                                    Ok(())
                                },
                            )
                            .await
                    },
                )
                .await
        })
        .await
}
