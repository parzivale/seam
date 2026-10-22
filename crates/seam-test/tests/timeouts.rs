//! Timeouts in the plain simulation, on the runtime, and on simulated
//! threads. The last two wait on the virtual clock, so the tests can check
//! exactly when a timeout fired.
#![cfg(feature = "full")]

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use seam_core::future::SendPerform;
use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, TcpBind, TcpConnect};
use seam_core::ops::signal::CtrlC;
use seam_core::ops::sync::{Channel, RecvError, RecvMsg, SendError, SendMsg};
use seam_core::ops::task::{Join, JoinError, Spawn};
use seam_core::ops::time::{Now, Sleep, Timeout};
use seam_core::prelude::*;
use seam_test::Sim;

const SEC: Duration = Duration::from_secs(1);

fn now<C: Inspect<Now>>(c: &C) -> Duration {
    c.inspect(Now).since_epoch()
}

/// Receives until nothing has arrived for `quiet`. Generic, so it runs on
/// any backend's channels.
fn drain_until_quiet<R>(rx: &mut R, quiet: Duration) -> Vec<u32>
where
    R: Perform<Timeout<RecvMsg<u32>>>,
{
    let mut got = Vec::new();
    while let Ok(v) = rx.perform(Timeout::new(quiet, RecvMsg::new())) {
        got.push(v);
    }
    got
}

mod plain {
    use super::*;
    use seam_test::net::{TcpListener, TcpStream};
    use seam_test::sync::{Receiver, Sender};

    /// Nothing waits in a plain `Sim`, so nothing arrives during a timeout:
    /// a timed wait that cannot finish at once expires at once, and the
    /// clock does not move.
    #[test]
    fn expires_at_once() {
        let mut sim = Sim::new();
        let (mut tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::bounded(1));
        assert_eq!(
            rx.perform(Timeout::new(SEC, RecvMsg::new())),
            Err(RecvError::Empty)
        );
        tx.perform(SendMsg(1)).unwrap();
        assert_eq!(
            tx.perform(Timeout::new(SEC, SendMsg(2))),
            Err(SendError::Full(2))
        );
        assert_eq!(drain_until_quiet(&mut rx, SEC), [1]);

        let err = sim.perform(Timeout::new(SEC, CtrlC)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        sim.raise_ctrl_c();
        sim.perform(Timeout::new(SEC, CtrlC)).unwrap();

        let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
        sim.acquire(TcpBind::<TcpListener>::new(addr), |sim, listener| {
            let err = listener
                .acquire(Timeout::new(SEC, Accept::<TcpStream>::new()), |_, _| {
                    io::Result::Ok(())
                })
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            sim.acquire(
                Timeout::new(SEC, TcpConnect::<TcpStream>::new(addr)),
                |_, _client| {
                    listener.acquire(
                        Timeout::new(SEC, Accept::<TcpStream>::new()),
                        |_, server| {
                            let err = server
                                .perform(Timeout::new(SEC, Read(&mut [0; 4])))
                                .unwrap_err();
                            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
                            io::Result::Ok(())
                        },
                    )
                },
            )
        })
        .unwrap();
        assert_eq!(now(&sim), Duration::ZERO);
    }
}

mod runtime {
    use super::*;
    use seam_test::rt::net::{TcpListener, TcpStream};
    use seam_test::rt::sync::{Receiver, Sender};
    use seam_test::rt::{Deadlock, JoinHandle, Runtime};

    #[test]
    fn a_timeout_fires_on_the_virtual_clock() {
        Runtime::new(Sim::new()).block_on(|mut sim| async move {
            let (mut tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::bounded(1));
            let got = rx.perform_send(Timeout::new(5 * SEC, RecvMsg::new())).await;
            assert_eq!(got, Err(RecvError::Empty));
            assert_eq!(now(&sim), 5 * SEC, "exactly the timeout passed");

            tx.perform_send(SendMsg(1)).await.unwrap();
            let full = tx.perform_send(Timeout::new(SEC, SendMsg(2))).await;
            assert_eq!(full, Err(SendError::Full(2)), "the value comes back");
            assert_eq!(now(&sim), 6 * SEC);
        });
    }

    /// A message that arrives first wins, and the timer it beat is removed:
    /// with nothing left to wait for, the clock stays where the message
    /// arrived instead of jumping to the old deadline.
    #[test]
    fn what_arrives_in_time_wins() {
        Runtime::new(Sim::new()).block_on(|mut sim| async move {
            let (tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
            let mut worker = sim.clone();
            let mut tx = tx;
            let late = Spawn::<_, JoinHandle<()>>::new(async move {
                worker.perform_send(Sleep(2 * SEC)).await;
                tx.perform_send(SendMsg(9)).await.unwrap();
            });
            sim.acquire_async(late, async |sim, task| {
                let got = rx
                    .perform_send(Timeout::new(60 * SEC, RecvMsg::new()))
                    .await;
                assert_eq!(got, Ok(9));
                assert_eq!(now(sim), 2 * SEC);
                task.perform_send(Join::new()).await
            })
            .await
            .unwrap();
            assert_eq!(now(&sim), 2 * SEC, "the 60s timer did not move the clock");
        });
    }

    /// A timer that a message beat is removed. Here the program then waits
    /// on something nothing can satisfy: the deadlock is reported at the
    /// time it happened (2s), not after the clock jumps to the beaten
    /// timer's deadline (60s).
    #[test]
    fn a_beaten_timer_is_removed() {
        let rt = Runtime::new(Sim::new());
        let result = rt.try_block_on(|mut sim| async move {
            let (tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
            let _keep_open = tx.clone();
            let (mut tx, mut worker) = (tx, sim.clone());
            let late = Spawn::<_, JoinHandle<()>>::new(async move {
                worker.perform_send(Sleep(2 * SEC)).await;
                tx.perform_send(SendMsg(1)).await.unwrap();
            });
            sim.acquire_async(late, async |_, task| {
                assert_eq!(
                    rx.perform_send(Timeout::new(60 * SEC, RecvMsg::new()))
                        .await,
                    Ok(1)
                );
                task.perform_send(Join::new()).await?;
                Ok::<_, JoinError>(rx.perform_send(RecvMsg::new()).await) // nothing more comes
            })
            .await
        });
        assert_eq!(result, Err(Deadlock { tasks: 0 }));
        assert_eq!(rt.with_sim(|s| now(s)), 2 * SEC);
    }

    /// A wait that would deadlock without a timeout just expires with one.
    #[test]
    fn a_timeout_is_not_a_deadlock() {
        let result = Runtime::new(Sim::new()).try_block_on(|mut sim| async move {
            let (_tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
            rx.perform_send(Timeout::new(SEC, RecvMsg::new())).await
        });
        assert_eq!(result, Ok(Err(RecvError::Empty)));
        let result = Runtime::new(Sim::new()).try_block_on(|mut sim| async move {
            let (_tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
            rx.perform_send(RecvMsg::new()).await
        });
        assert_eq!(result, Err(Deadlock { tasks: 0 }));
    }

    #[test]
    fn joins_and_sockets() {
        Runtime::new(Sim::new()).block_on(|mut sim| async move {
            let mut worker = sim.clone();
            let slow = Spawn::<_, JoinHandle<u8>>::new(async move {
                worker.perform_send(Sleep(10 * SEC)).await;
                7
            });
            sim.acquire_async(slow, async |sim, task| {
                let first = task.perform_send(Timeout::new(SEC, Join::new())).await;
                assert_eq!(first, Err(JoinError::TimedOut));
                assert_eq!(now(sim), SEC);
                assert_eq!(
                    task.perform_send(Join::new()).await,
                    Ok(7),
                    "joinable again"
                );
                Ok::<_, JoinError>(())
            })
            .await
            .unwrap();

            let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
            sim.acquire_async(TcpBind::<TcpListener>::new(addr), async |sim, listener| {
                let start = now(sim);
                let err = listener
                    .acquire_async(
                        Timeout::new(SEC, Accept::<TcpStream>::new()),
                        async |_, _| io::Result::Ok(()),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::TimedOut);
                assert_eq!(now(sim) - start, SEC);
                sim.acquire_async(TcpConnect::<TcpStream>::new(addr), async |_, client| {
                    listener
                        .acquire_async(
                            Timeout::new(SEC, Accept::<TcpStream>::new()),
                            async |_, server| {
                                client.perform_send(WriteAll(b"partial")).await?;
                                let mut all = Vec::new();
                                let err = server
                                    .perform_send(Timeout::new(SEC, ReadToEnd(&mut all)))
                                    .await
                                    .unwrap_err();
                                assert_eq!(
                                    err.kind(),
                                    io::ErrorKind::TimedOut,
                                    "the client never closed"
                                );
                                assert_eq!(all, b"partial", "what arrived is kept");
                                io::Result::Ok(())
                            },
                        )
                        .await
                })
                .await
            })
            .await
            .unwrap();
        });
    }
}

mod threads {
    use super::*;
    use seam_test::threads::net::{TcpListener, TcpStream};
    use seam_test::threads::sync::{Receiver, Sender};
    use seam_test::threads::{JoinHandle, Threads};

    /// The generic core, on simulated threads: messages at 1s, 2s and 3s,
    /// then quiet. It stops exactly 5s after the last one, whatever the
    /// interleaving.
    #[test]
    fn a_quiet_period_ends_the_drain() {
        for seed in 0..16 {
            Threads::with_seed(Sim::new(), seed).run(|mut sim| {
                let (tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
                let mut worker = sim.clone();
                let mut tx = tx;
                let producer = Spawn::<_, JoinHandle<()>>::new(move || {
                    for i in 1..=3 {
                        worker.perform(Sleep(SEC));
                        tx.perform(SendMsg(i)).unwrap();
                    }
                    worker.perform(Sleep(60 * SEC)); // alive, but quiet
                });
                sim.acquire(producer, |sim, producer| {
                    assert_eq!(
                        drain_until_quiet(&mut rx, 5 * SEC),
                        [1, 2, 3],
                        "seed {seed}"
                    );
                    assert_eq!(now(sim), 8 * SEC, "seed {seed}");
                    producer.perform(Join::new())
                })
                .unwrap();
            });
        }
    }

    /// As on the runtime: a timer that a message beat is removed, so the
    /// deadlock that follows is reported at 2s, not 60s.
    #[test]
    fn a_beaten_timer_is_removed() {
        let threads = Threads::new(Sim::new());
        let result = threads.try_run(|mut sim| {
            let (tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
            let _keep_open = tx.clone();
            let (mut tx, mut worker) = (tx, sim.clone());
            let late = Spawn::<_, JoinHandle<()>>::new(move || {
                worker.perform(Sleep(2 * SEC));
                tx.perform(SendMsg(1)).unwrap();
            });
            sim.acquire(late, |_, thread| {
                assert_eq!(rx.perform(Timeout::new(60 * SEC, RecvMsg::new())), Ok(1));
                thread.perform(Join::new())?;
                Ok::<_, JoinError>(rx.perform(RecvMsg::new())) // nothing more comes
            })
        });
        assert_eq!(result, Err(seam_test::threads::Deadlock { threads: 0 }));
        assert_eq!(threads.with_sim(|s| now(s)), 2 * SEC);
    }

    #[test]
    fn joins_signals_and_sockets() {
        let threads = Threads::new(Sim::new());
        threads.run(|mut sim| {
            let mut worker = sim.clone();
            let slow = Spawn::<_, JoinHandle<u8>>::new(move || {
                worker.perform(Sleep(10 * SEC));
                7
            });
            sim.acquire(slow, |sim, thread| {
                assert_eq!(
                    thread.perform(Timeout::new(SEC, Join::new())),
                    Err(JoinError::TimedOut)
                );
                assert_eq!(now(sim), SEC);
                assert_eq!(thread.perform(Timeout::new(60 * SEC, Join::new())), Ok(7));
                assert_eq!(now(sim), 10 * SEC);
                Ok::<_, JoinError>(())
            })
            .unwrap();

            let err = sim.perform(Timeout::new(SEC, CtrlC)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            assert_eq!(now(&sim), 11 * SEC);

            let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
            sim.acquire(TcpBind::<TcpListener>::new(addr), |sim, listener| {
                let err = listener
                    .acquire(Timeout::new(SEC, Accept::<TcpStream>::new()), |_, _| {
                        io::Result::Ok(())
                    })
                    .unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::TimedOut);
                sim.acquire(TcpConnect::<TcpStream>::new(addr), |_, _client| {
                    listener.acquire(
                        Timeout::new(SEC, Accept::<TcpStream>::new()),
                        |_, server| {
                            let err = server
                                .perform(Timeout::new(SEC, Read(&mut [0; 4])))
                                .unwrap_err();
                            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
                            io::Result::Ok(())
                        },
                    )
                })
            })
            .unwrap();
            assert_eq!(now(&sim), 13 * SEC);
        });
    }
}
