//! Sync cores with threads, on the deterministic thread scheduler.
#![cfg(feature = "full")]

use seam_core::io;
use seam_core::ops::fs::{read_to_string, write_file};
use seam_core::ops::io::{read_to_end, write_all};
use seam_core::ops::stdio::print;
use seam_core::ops::sync::store;
use std::net::SocketAddr;
use std::time::Duration;

use seam_core::ops::fs::{NextEntry, ReadDir};

use seam_core::ops::net::{Accept, TcpBind, TcpConnect};
use seam_core::ops::signal::CtrlC;
use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendMsg, Share, Update};
use seam_core::ops::task::{
    Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, SpawnWith, YieldNow,
};
use seam_core::ops::time::{Now, Sleep};
use seam_core::prelude::*;
use seam_test::Sim;
use seam_test::threads::net::{TcpListener, TcpStream};
use seam_test::threads::sync::{Receiver, Sender, Shared};
use seam_test::threads::{Deadlock, JoinHandle, ThreadSet, ThreadSim, Threads};

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

/// Spawns `n` threads that each print their number, and returns what
/// was printed.
fn print_order(seed: u64, n: u8) -> Vec<u8> {
    let threads = Threads::with_seed(Sim::new(), seed);
    threads.run(|mut sim| {
        sim.acquire(SpawnSet::<ThreadSet<()>>::new(), |sim, set| {
            for i in 0..n {
                let mut sim = sim.clone();
                let _ = set.perform(SpawnInto::new(move || {
                    let _ = print(&mut sim, &[b'0' + i]);
                }));
            }
            Ok::<_, JoinError>(())
        })
        .unwrap();
    });
    threads.with_sim(|s| s.take_stdout())
}

#[test]
fn a_seed_fixes_the_interleaving() {
    for seed in 0..8 {
        assert_eq!(print_order(seed, 5), print_order(seed, 5), "seed {seed}");
    }
    let orders: std::collections::BTreeSet<_> = (0..32).map(|seed| print_order(seed, 5)).collect();
    assert!(orders.len() > 4, "seeds explore different interleavings");
}

/// Three threads increment a counter. Done as a load and then a store, a
/// schedule exists that loses a count; the seed that finds it replays it.
/// Done as one `Update`, no schedule does.
#[test]
fn a_lost_update_is_found_replayed_and_fixed() {
    fn run_with(seed: u64, atomic: bool) -> u64 {
        Threads::with_seed(Sim::new(), seed).run(|mut sim| {
            let counter: Shared<u64> = sim.perform(Share::new(0));
            sim.acquire(SpawnSet::<ThreadSet<()>>::new(), |_, set| {
                for _ in 0..3 {
                    let mut counter = counter.clone();
                    let _ = set.perform(SpawnInto::new(move || {
                        if atomic {
                            counter.perform(Update::new(&mut |n: &mut u64| *n += 1));
                        } else {
                            let n = counter.inspect(Load::new());
                            store(&mut counter, n + 1);
                        }
                    }));
                }
                Ok::<_, JoinError>(())
            })
            .unwrap();
            counter.inspect(Load::new())
        })
    }

    let seed = (0..64)
        .find(|&seed| run_with(seed, false) < 3)
        .expect("some schedule loses a count");
    assert!(run_with(seed, false) < 3, "the failing seed replays");
    for seed in 0..64 {
        assert_eq!(
            run_with(seed, true),
            3,
            "seed {seed}: Update never loses one"
        );
    }
}

/// A producer sends on a bounded channel; the consumer adds up. Each
/// blocks on the other.
#[test]
fn channels_block_both_ways() {
    for seed in 0..16 {
        for capacity in [1, 3] {
            let sum = Threads::with_seed(Sim::new(), seed).run(|mut sim| {
                let (tx, mut rx): (Sender<u64>, Receiver<u64>) =
                    sim.perform(Channel::bounded(capacity));
                let mut tx = tx;
                let producer = Spawn::<_, JoinHandle<()>>::new(move || {
                    for i in 1..=40 {
                        tx.perform(SendMsg(i)).unwrap();
                    }
                });
                sim.acquire(producer, |_, _| {
                    let mut sum = 0;
                    loop {
                        match rx.perform(RecvMsg::new()) {
                            Ok(i) => sum += i,
                            Err(RecvError::Closed) => break,
                            Err(e) => panic!("{e}"),
                        }
                    }
                    Ok::<_, JoinError>(sum)
                })
                .unwrap()
            });
            assert_eq!(sum, 820, "seed {seed}, capacity {capacity}");
        }
    }
}

#[test]
fn waiting_forever_is_a_deadlock() {
    // The main thread waits on a channel nobody will send on.
    let result = Threads::new(Sim::new()).try_run(|mut sim| {
        let (_tx, mut rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::unbounded());
        rx.perform(RecvMsg::new())
    });
    assert_eq!(result, Err(Deadlock { threads: 0 }));

    // Two threads each wait for the other to send first.
    let result = Threads::new(Sim::new()).try_run(|mut sim| {
        let (mut a_tx, mut a_rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::unbounded());
        let (mut b_tx, mut b_rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::unbounded());
        let other = Spawn::<_, JoinHandle<()>>::new(move || {
            let v = a_rx.perform(RecvMsg::new()).unwrap();
            b_tx.perform(SendMsg(v)).unwrap();
        });
        sim.acquire(other, |_, _| {
            let v = b_rx.perform(RecvMsg::new());
            a_tx.perform(SendMsg(1)).unwrap();
            Ok::<_, JoinError>(v)
        })
    });
    assert_eq!(result, Err(Deadlock { threads: 1 }));
}

#[test]
#[should_panic(expected = "deadlock")]
fn run_panics_on_a_deadlock() {
    Threads::new(Sim::new()).run(|mut sim| {
        let (_tx, mut rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::unbounded());
        let _ = rx.perform(RecvMsg::new());
    });
}

/// Sleeping threads take no real time, and wake in deadline order.
#[test]
fn sleeps_run_on_virtual_time() {
    let threads = Threads::new(Sim::new());
    let end = threads.run(|mut sim| {
        sim.acquire(SpawnSet::<ThreadSet<()>>::new(), |sim, set| {
            for secs in [30, 10, 20] {
                let mut sim = sim.clone();
                let _ = set.perform(SpawnInto::new(move || {
                    sim.perform(Sleep(Duration::from_secs(secs)));
                    let _ = print(&mut sim, format!("{secs} ").as_bytes());
                }));
            }
            Ok::<_, JoinError>(())
        })
        .unwrap();
        sim.inspect(Now).since_epoch()
    });
    assert_eq!(end, Duration::from_secs(30));
    assert_eq!(threads.with_sim(|s| s.take_stdout()), b"10 20 30 ");
}

#[test]
fn a_panicking_thread_is_reported() {
    Threads::new(Sim::new()).run(|mut sim| {
        let boom = Spawn::<_, JoinHandle<u8>>::new(|| panic!("boom"));
        let joined = sim.acquire(boom, |_, thread| {
            Ok::<_, JoinError>(thread.perform(Join::new()))
        });
        assert_eq!(joined, Ok(Err(JoinError::Panicked)));

        // Not joined in the scope: closing it reports the panic.
        let boom = Spawn::<_, JoinHandle<u8>>::new(|| panic!("boom"));
        assert_eq!(
            sim.acquire(boom, |_, _| Ok::<_, JoinError>(())),
            Err(JoinError::Panicked)
        );
    });
}

/// A scope that fails still waits for its thread, as `std::thread::scope`
/// does: the thread's work is done when `acquire` returns.
#[test]
fn a_failed_scope_joins_its_thread() {
    let threads = Threads::new(Sim::new());
    threads.run(|mut sim| {
        let mut worker = sim.clone();
        let slow = Spawn::<_, JoinHandle<()>>::new(move || {
            worker.perform(Sleep(Duration::from_secs(5)));
            write_file(&mut worker, "/done", "yes").unwrap();
        });
        let result = sim.acquire(slow, |_, thread| {
            assert!(!thread.inspect(IsFinished));
            Err::<(), _>(io::Error::other("gave up"))
        });
        assert_eq!(result.unwrap_err().to_string(), "gave up");
        assert_eq!(
            read_to_string(&mut sim, "/done").unwrap(),
            "yes",
            "the thread finished before acquire returned"
        );
    });
}

/// A detached thread outlives its scope, and is stopped when the main
/// thread returns.
#[test]
fn a_detached_thread_is_stopped_at_the_end() {
    let threads = Threads::new(Sim::new());
    threads.run(|mut sim| {
        let mut worker = sim.clone();
        let forever = Spawn::<_, JoinHandle<()>>::new(move || {
            loop {
                worker.perform(Sleep(Duration::from_secs(1)));
                let _ = print(&mut worker, b"tick\n");
            }
        });
        sim.acquire(forever, |sim, thread| {
            thread.perform(Detach);
            sim.perform(Sleep(Duration::from_millis(2500)));
            Ok::<_, JoinError>(())
        })
        .unwrap();
    });
    assert_eq!(threads.with_sim(|s| s.take_stdout()), b"tick\ntick\n");
}

/// `JoinNext` hands out results as threads finish, in virtual time order.
#[test]
fn join_next_takes_results_as_they_come() {
    let order = Threads::new(Sim::new()).run(|mut sim| {
        sim.acquire(SpawnSet::<ThreadSet<u64>>::new(), |sim, set| {
            for ms in [300, 100, 200] {
                let mut sim = sim.clone();
                let _ = set.perform(SpawnInto::new(move || {
                    sim.perform(Sleep(Duration::from_millis(ms)));
                    ms
                }));
            }
            let mut order = Vec::new();
            while let Some(r) = set.perform(JoinNext::new()) {
                order.push(r?);
            }
            Ok::<_, JoinError>(order)
        })
        .unwrap()
    });
    assert_eq!(order, [100, 200, 300]);
}

/// While one thread holds a cell in an `Update` whose closure yields,
/// another thread touching the cell waits for it.
#[test]
fn an_update_holds_the_cell() {
    for seed in 0..16 {
        let seen = Threads::with_seed(Sim::new(), seed).run(|mut sim| {
            let cell: Shared<Vec<u8>> = sim.perform(Share::new(Vec::new()));
            let reader = cell.clone();
            let mut yielder = sim.clone();
            let mut writer = cell.clone();
            sim.acquire(SpawnSet::<ThreadSet<Vec<u8>>>::new(), |_, set| {
                let _ = set.perform(SpawnInto::new(move || {
                    writer.perform(Update::new(&mut |v: &mut Vec<u8>| {
                        v.push(1);
                        yielder.perform(YieldNow);
                        v.push(2);
                    }));
                    Vec::new()
                }));
                let _ = set.perform(SpawnInto::new(move || reader.inspect(Load::new())));
                let mut seen = Vec::new();
                while let Some(r) = set.perform(JoinNext::new()) {
                    seen.push(r?);
                }
                Ok::<_, JoinError>(seen)
            })
            .unwrap()
        });
        assert!(
            seen.iter().all(|v| v.is_empty() || v == &[1, 2]),
            "seed {seed}: never half-updated, saw {seen:?}"
        );
    }
}

#[test]
fn ctrl_c_wakes_a_waiting_thread() {
    let threads = Threads::new(Sim::new());
    threads.run(|mut sim| {
        let mut waiter = sim.clone();
        let wait = Spawn::<_, JoinHandle<io::Result<()>>>::new(move || {
            waiter.perform(CtrlC)?;
            let _ = print(&mut waiter, b"bye\n");
            Ok(())
        });
        sim.acquire(wait, |sim, thread| {
            sim.perform(Sleep(Duration::from_secs(1)));
            sim.raise_ctrl_c();
            thread.perform(Join::new())
        })
        .unwrap()
        .unwrap();
    });
    assert_eq!(threads.with_sim(|s| s.take_stdout()), b"bye\n");
}

/// A thread per connection: each client thread connects and sends, and the
/// server echoes each connection on a thread `SpawnWith` starts for it.
#[test]
fn a_thread_per_connection_server() {
    fn echo(stream: &mut TcpStream) -> io::Result<usize> {
        let mut buf = Vec::new();
        read_to_end(stream, &mut buf)?;
        write_all(stream, &buf)?;
        Ok(buf.len())
    }

    for seed in 0..8 {
        let threads = Threads::with_seed(Sim::new(), seed);
        let replies = threads.run(|mut sim| {
            let addr: SocketAddr = "10.0.0.1:80".parse().unwrap();
            sim.acquire(TcpBind::<TcpListener>::new(addr), |sim, listener| {
                sim.acquire(
                    SpawnSet::<ThreadSet<io::Result<Vec<u8>>>>::new(),
                    |sim, clients| {
                        for i in 0..3u8 {
                            let mut sim: ThreadSim = sim.clone();
                            let _ = clients.perform(SpawnInto::new(move || {
                                let mut reply = Vec::new();
                                sim.acquire(TcpConnect::<TcpStream>::new(addr), |_, s| {
                                    write_all(s, &[i; 3])?;
                                    s.perform(seam_core::ops::net::Shutdown(
                                        seam_core::ops::net::How::Write,
                                    ))?;
                                    read_to_end(s, &mut reply)?;
                                    io::Result::Ok(())
                                })?;
                                Ok(reply)
                            }));
                        }
                        for _ in 0..3 {
                            let conn = SpawnWith::<
                                Accept<TcpStream>,
                                _,
                                JoinHandle<io::Result<usize>>,
                            >::new(Accept::new(), echo);
                            listener.acquire(conn, |_, handle| {
                                assert_eq!(handle.perform(Join::new())??, 3);
                                io::Result::Ok(())
                            })?;
                        }
                        let mut replies = Vec::new();
                        while let Some(r) = clients.perform(JoinNext::new()) {
                            replies.push(r.map_err(io::Error::from)??);
                        }
                        replies.sort();
                        io::Result::Ok(replies)
                    },
                )
            })
            .unwrap()
        });
        assert_eq!(replies, [[0; 3], [1; 3], [2; 3]], "seed {seed}");
    }
}

#[test]
#[should_panic(expected = "outside Threads::run")]
fn operations_need_a_run() {
    let threads = Threads::new(Sim::new());
    let mut escaped = threads.run(|sim| sim);
    escaped.perform(YieldNow);
}

/// The file the thread wrote is visible to the main thread, and the world
/// is there for assertions after the run.
#[test]
fn threads_share_the_world() {
    let threads = Threads::new(Sim::new());
    threads.run(|mut sim| {
        let mut worker = sim.clone();
        let write = Spawn::<_, JoinHandle<io::Result<()>>>::new(move || {
            write_file(&mut worker, "/note", "hi")
        });
        sim.acquire(write, |_, t| t.perform(Join::new()))
            .unwrap()
            .unwrap();
        assert_eq!(read_to_string(&mut sim, "/note").unwrap(), "hi");
    });
    assert_eq!(
        threads.with_sim(|s| s.file("/note").unwrap().to_vec()),
        b"hi"
    );
}

#[test]
fn files_through_threads() {
    let mut sim = Sim::new();
    sim.add_file("/d/a", "1");
    let listed = Threads::new(sim).run(|mut sim| {
        write_file(&mut sim, "/d/b", "2").unwrap();
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
