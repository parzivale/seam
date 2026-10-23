//! The deterministic runtime: scheduling, virtual time, waiting, tasks.
#![cfg(feature = "full")]

use seam_core::io;
use std::future::{Future, poll_fn};
use std::net::SocketAddr;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use seam_core::future::{SendInspect, SendPerform};
use seam_core::ops::fs::{CreateAtomic, NextEntry, ReadDir, ReadToString, WriteFile};
use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, RecvFrom, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind};
use seam_core::ops::signal::CtrlC;
use seam_core::ops::stdio::Print;
use seam_core::ops::task::{
    Detach, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, YieldNow,
};
use seam_core::ops::time::{Now, Sleep};
use seam_core::{AsyncAcquire, Inspect};
use seam_test::Sim;
use seam_test::fs::AtomicFile;
use seam_test::rt::net::{TcpListener, TcpStream, UdpSocket};
use seam_test::rt::{AsyncSim, Deadlock, JoinHandle, Runtime, TaskSet};

/// Lists `dir` through `ReadDir`, one entry at a time.
fn list<C, D>(c: &mut C, dir: impl AsRef<std::path::Path>) -> io::Result<Vec<std::path::PathBuf>>
where
    C: for<'a> seam_core::Provide<ReadDir<'a, D>>,
    D: seam_core::Perform<NextEntry>,
{
    seam_core::Acquire::acquire(c, ReadDir::new(dir.as_ref()), |_, entries| {
        let mut paths = Vec::new();
        while let Some(path) = seam_core::Perform::perform(entries, NextEntry)? {
            paths.push(path);
        }
        Ok(paths)
    })
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

async fn say(sim: &mut AsyncSim, text: &str) {
    sim.perform_send(Print(text.as_bytes())).await.unwrap();
}

fn stdout(rt: &Runtime) -> String {
    String::from_utf8(rt.with_sim(|s| s.take_stdout())).unwrap()
}

/// Whichever of `a` and `b` finishes first; the other is dropped, which is
/// how a timeout cancels.
async fn race<T>(a: impl Future<Output = T>, b: impl Future<Output = T>) -> T {
    let (mut a, mut b) = (pin!(a), pin!(b));
    poll_fn(|cx| {
        if let Poll::Ready(v) = a.as_mut().poll(cx) {
            return Poll::Ready(v);
        }
        b.as_mut().poll(cx)
    })
    .await
}

#[test]
#[allow(clippy::disallowed_methods)] // measuring real time is the point
fn sleeping_costs_no_real_time() {
    let rt = Runtime::new(Sim::new());
    let start = std::time::Instant::now();
    let elapsed = rt.block_on(|mut sim| async move {
        let t0 = sim.inspect_send(Now).await;
        sim.perform_send(Sleep(Duration::from_secs(3600))).await;
        sim.inspect_send(Now).await - t0
    });
    assert_eq!(elapsed, Duration::from_secs(3600));
    assert!(start.elapsed() < Duration::from_secs(1));
}

/// Timers fire in deadline order, whatever order they were set in and
/// whatever the seed.
#[test]
fn timers_fire_in_deadline_order() {
    for seed in 0..10 {
        let rt = Runtime::with_seed(Sim::new(), seed);
        rt.block_on(|mut sim| async move {
            let tasks = SpawnSet::<TaskSet<()>>::new();
            sim.acquire_async(tasks, async |sim, set| {
                for (secs, name) in [(3, "c"), (1, "a"), (2, "b")] {
                    let mut sim = sim.clone();
                    let task = async move {
                        sim.perform_send(Sleep(Duration::from_secs(secs))).await;
                        say(&mut sim, name).await;
                    };
                    let _ = set.perform_send(SpawnInto::new(task)).await;
                }
                Ok::<_, JoinError>(())
            })
            .await
            .unwrap();
        });
        assert_eq!(stdout(&rt), "abc", "seed {seed}");
    }
}

/// Three tasks that each log a few steps, yielding in between.
fn interleaving(seed: u64) -> String {
    let rt = Runtime::with_seed(Sim::new(), seed);
    rt.block_on(|mut sim| async move {
        sim.acquire_async(SpawnSet::<TaskSet<()>>::new(), async |sim, set| {
            for name in ["x", "y", "z"] {
                let mut sim = sim.clone();
                let task = async move {
                    for _ in 0..3 {
                        say(&mut sim, name).await;
                        sim.perform_send(YieldNow).await;
                    }
                };
                let _ = set.perform_send(SpawnInto::new(task)).await;
            }
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();
    });
    stdout(&rt)
}

#[test]
fn the_seed_decides_the_interleaving() {
    assert_eq!(interleaving(7), interleaving(7), "same seed, same schedule");
    let schedules: std::collections::BTreeSet<_> = (0..20).map(interleaving).collect();
    assert!(
        schedules.len() > 5,
        "different seeds explore different schedules: {schedules:?}"
    );
    for s in &schedules {
        assert_eq!(s.matches('x').count(), 3);
    }
}

/// A classic lost update: two tasks read a counter, yield, and write it
/// back plus one. Some schedules lose an increment. The runtime finds them,
/// and replays one exactly from its seed.
#[test]
fn seeds_find_and_replay_a_race() {
    fn run(seed: u64) -> String {
        let mut sim = Sim::new();
        sim.add_file("/counter", "0");
        let rt = Runtime::with_seed(sim, seed);
        rt.block_on(|mut sim| async move {
            sim.acquire_async(
                SpawnSet::<TaskSet<io::Result<()>>>::new(),
                async |sim, set| {
                    for _ in 0..2 {
                        let mut sim = sim.clone();
                        let increment = async move {
                            let n: u32 = sim
                                .inspect_send(ReadToString::new("/counter"))
                                .await?
                                .parse()
                                .unwrap();
                            sim.perform_send(YieldNow).await; // the window
                            sim.perform_send(WriteFile::new("/counter", &(n + 1).to_string()))
                                .await
                        };
                        let _ = set.perform_send(SpawnInto::new(increment)).await;
                    }
                    while let Some(r) = set.perform_send(JoinNext::new()).await {
                        r?.unwrap();
                    }
                    Ok::<_, JoinError>(())
                },
            )
            .await
            .unwrap();
        });
        rt.with_sim(|s| String::from_utf8(s.file("/counter").unwrap()).unwrap())
    }

    let lost = (0..32)
        .find(|&seed| run(seed) == "1")
        .expect("some schedule loses an update");
    let kept = (0..32)
        .find(|&seed| run(seed) == "2")
        .expect("some schedule keeps both");
    assert_eq!(
        run(lost),
        "1",
        "replaying seed {lost} loses the update again"
    );
    assert_eq!(run(kept), "2");
}

#[test]
fn reads_and_accepts_wait_for_the_other_side() {
    let rt = Runtime::new(Sim::new());
    let reply = rt.block_on(|mut sim| async move {
        sim.acquire_async(
            TcpBind::<TcpListener>::new(addr(80)),
            async |sim, listener| {
                let mut sim2 = sim.clone();
                // The client starts late; the server's accept and read must wait.
                let client = async move {
                    sim2.perform_send(Sleep(Duration::from_secs(5))).await;
                    sim2.acquire_async(TcpConnect::<TcpStream>::new(addr(80)), async |sim, c| {
                        c.perform_send(WriteAll(b"ping")).await?;
                        sim.perform_send(Sleep(Duration::from_secs(5))).await;
                        c.perform_send(WriteAll(b" pong")).await?;
                        c.perform_send(Shutdown(seam_core::ops::net::How::Write))
                            .await?;
                        let mut reply = Vec::new();
                        c.perform_send(ReadToEnd(&mut reply)).await?;
                        io::Result::Ok(reply)
                    })
                    .await
                };
                sim.acquire_async(
                    Spawn::<_, JoinHandle<io::Result<Vec<u8>>>>::new(client),
                    async |_, client| {
                        listener
                            .acquire_async(Accept::<TcpStream>::new(), async |_, s| {
                                let mut got = Vec::new();
                                s.perform_send(ReadToEnd(&mut got)).await?; // waits for the client to finish
                                s.perform_send(WriteAll(&got)).await
                            })
                            .await?;
                        client
                            .perform_send(Join::new())
                            .await
                            .map_err(io::Error::from)?
                    },
                )
                .await
            },
        )
        .await
    });
    assert_eq!(reply.unwrap(), b"ping pong");
    assert_eq!(
        rt.with_sim(|s| s.inspect(Now)).since_epoch(),
        Duration::from_secs(10)
    );
}

#[test]
fn udp_receives_wait() {
    let rt = Runtime::new(Sim::new());
    let got = rt.block_on(|mut sim| async move {
        sim.acquire_async(UdpBind::<UdpSocket>::new(addr(53)), async |sim, server| {
            let mut sim2 = sim.clone();
            let sender = async move {
                sim2.perform_send(Sleep(Duration::from_millis(30))).await;
                sim2.acquire_async(UdpBind::<UdpSocket>::new(addr(0)), async |_, s| {
                    s.perform_send(SendTo {
                        buf: b"query",
                        addr: addr(53),
                    })
                    .await
                    .map(drop)
                })
                .await
            };
            sim.acquire_async(
                Spawn::<_, JoinHandle<io::Result<()>>>::new(sender),
                async |_, _| {
                    let mut buf = [0; 16];
                    let (n, _) = server.perform_send(RecvFrom(&mut buf)).await?;
                    io::Result::Ok(buf[..n].to_vec())
                },
            )
            .await
        })
        .await
    });
    assert_eq!(got.unwrap(), b"query");
}

#[test]
fn deadlocks_are_reported() {
    let rt = Runtime::new(Sim::new());
    let result = rt.try_block_on(|mut sim| async move {
        sim.acquire_async(
            TcpBind::<TcpListener>::new(addr(80)),
            async |_, listener| {
                // Nobody will ever connect.
                listener
                    .acquire_async(Accept::<TcpStream>::new(), async |_, _| io::Result::Ok(()))
                    .await
            },
        )
        .await
    });
    assert_eq!(result.unwrap_err(), Deadlock { tasks: 0 });
}

#[test]
fn ctrl_c_wakes_a_waiting_task() {
    let rt = Runtime::new(Sim::new());
    rt.block_on(|mut sim| async move {
        let mut waiter = sim.clone();
        let shutdown = async move {
            waiter.perform_send(CtrlC).await.unwrap();
            say(&mut waiter, "shutting down").await;
        };
        sim.acquire_async(
            Spawn::<_, JoinHandle<()>>::new(shutdown),
            async |sim, task| {
                sim.perform_send(Sleep(Duration::from_secs(1))).await;
                sim.raise_ctrl_c();
                task.perform_send(Join::new()).await
            },
        )
        .await
        .unwrap();
    });
    assert_eq!(stdout(&rt), "shutting down");
}

#[test]
fn task_panics_aborts_and_detaches() {
    let rt = Runtime::new(Sim::new());
    rt.block_on(|mut sim| async move {
        let boom = Spawn::<_, JoinHandle<()>>::new(async { panic!("boom") });
        let closed = sim
            .acquire_async(boom, async |_, _| Ok::<_, JoinError>(()))
            .await;
        assert_eq!(closed, Err(JoinError::Panicked));

        // A failed scope aborts its task: it never prints.
        let mut s = sim.clone();
        let doomed = Spawn::<_, JoinHandle<()>>::new(async move {
            s.perform_send(Sleep(Duration::from_secs(1))).await;
            say(&mut s, "doomed ran").await;
        });
        let failed = sim
            .acquire_async(doomed, async |_, _| Err::<(), _>(JoinError::Cancelled))
            .await;
        assert!(failed.is_err());

        // A detached task outlives its scope.
        let mut s = sim.clone();
        let free = Spawn::<_, JoinHandle<()>>::new(async move {
            s.perform_send(Sleep(Duration::from_secs(1))).await;
            say(&mut s, "detached ran").await;
        });
        sim.acquire_async(free, async |_, h| {
            h.perform_send(Detach).await;
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();
        sim.perform_send(Sleep(Duration::from_secs(2))).await;
    });
    assert_eq!(stdout(&rt), "detached ran");
}

/// A timeout, in virtual time, cancels an atomic write partway: the target
/// is untouched and the temporary file is gone.
#[test]
fn a_timeout_cancels_an_atomic_write_cleanly() {
    let mut sim = Sim::new();
    sim.add_file("/etc/app.conf", "old");
    let rt = Runtime::new(sim);
    rt.block_on(|mut sim| async move {
        let mut clock = sim.clone();
        let slow_write = sim.acquire_async(
            CreateAtomic::<AtomicFile>::new("/etc/app.conf"),
            async |sim, f| {
                f.perform_send(WriteAll(b"half")).await?;
                sim.perform_send(Sleep(Duration::from_secs(10))).await;
                f.perform_send(WriteAll(b" and the rest")).await
            },
        );
        let timeout = async move {
            clock.perform_send(Sleep(Duration::from_secs(1))).await;
            Err(io::ErrorKind::TimedOut.into())
        };
        let result: io::Result<()> = race(slow_write, timeout).await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    });
    rt.with_sim(|s| {
        assert_eq!(s.file("/etc/app.conf").unwrap(), b"old");
        assert_eq!(
            list(s, "/etc").unwrap(),
            [std::path::PathBuf::from("/etc/app.conf")]
        );
    });
}

/// The runtime's capability works on the same world as the `Sim` it was
/// given: set up before, inspect after.
#[test]
fn the_world_carries_over() {
    let mut sim = Sim::new();
    sim.add_file("/in", "hello");
    let rt = Runtime::new(sim);
    rt.block_on(|mut sim| async move {
        let text = sim.inspect_send(ReadToString::new("/in")).await.unwrap();
        sim.perform_send(WriteFile::new("/out", &text.to_uppercase()))
            .await
            .unwrap();
    });
    assert_eq!(rt.with_sim(|s| s.file("/out")).unwrap(), b"HELLO");
}

#[allow(dead_code)]
fn reads_are_send(stream: &mut TcpStream) -> impl Future + Send + '_ {
    stream.perform_send(Read(&mut []))
}

#[test]
fn files_through_the_runtime() {
    let mut sim = Sim::new();
    sim.add_file("/d/a", "1");
    let rt = Runtime::new(sim);
    let listed = rt.block_on(|mut sim| async move {
        seam_core::Perform::perform(&mut sim, WriteFile::new("/d/b", "2")).unwrap();
        list(&mut sim, "/d").unwrap()
    });
    assert_eq!(
        listed,
        [
            std::path::PathBuf::from("/d/a"),
            std::path::PathBuf::from("/d/b")
        ]
    );
}
