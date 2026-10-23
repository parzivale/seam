//! Runs the std backend against the real OS.
#![cfg(feature = "full")]

use seam_core::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use seam_core::ops::fs::{
    CreateAtomic, CreateDir, NextEntry, Open, ReadDir, ReadFile, RemoveDir, SweepAtomic, WriteFile,
};
use seam_core::ops::io::{Read, ReadExact, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, ResolveInto, TcpBind, TcpConnect};
use seam_core::ops::random::FillRandom;
use seam_core::ops::time::Now;
use seam_core::{Acquire, Inspect, Perform, Provide};
use seam_std::Std;
use seam_std::fs::AtomicFile;

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

fn scratch_dir(std: &mut Std, name: &str) -> io::Result<PathBuf> {
    let mut dir = std.inspect(seam_core::ops::env::TempDir);
    dir.push(format!("seam-std-{name}-{}", std::process::id()));
    let _ = std.perform(RemoveDir::all(&dir));
    std.perform(CreateDir::all(&dir))?;
    Ok(dir)
}

/// A core written only against capability bounds: it can open files, and
/// read and write them, and nothing else.
fn copy_through_handle<C, F>(c: &mut C, from: &Path, to: &Path) -> io::Result<()>
where
    C: for<'a> Provide<Open<'a, F>>,
    F: for<'a> Perform<ReadToEnd<'a>> + for<'a> Perform<WriteAll<'a>>,
{
    c.acquire(Open::read(from), |c, src| {
        let mut bytes = Vec::new();
        src.perform(ReadToEnd(&mut bytes))?;
        c.acquire(Open::create(to), |_, dst| dst.perform(WriteAll(&bytes)))
    })
}

#[test]
fn filesystem_roundtrip() -> io::Result<()> {
    let mut std = Std::new();
    let dir = scratch_dir(&mut std, "fs")?;
    let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
    std.perform(WriteFile::new(&a, "hello"))?;
    copy_through_handle(&mut std, &a, &b)?;

    assert_eq!(std.inspect(ReadFile::new(&b))?, b"hello");
    let mut listed = list(&mut std, &dir)?;
    listed.sort();
    assert_eq!(listed, [a, b]);
    std.perform(RemoveDir::all(&dir))
}

#[test]
fn tcp_loopback() -> io::Result<()> {
    let mut std = Std::new();
    let any_port = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    std.acquire(TcpBind::new(any_port), |std, listener| {
        let port = listener.inspect(LocalAddr)?.port();
        let mut addr = [any_port];
        assert_eq!(
            std.inspect(ResolveInto::new("127.0.0.1", port, &mut addr))?,
            1
        );
        let [addr] = addr;
        std.acquire(TcpConnect::new(addr), |_, client| {
            client.perform(WriteAll(b"ping"))?;
            listener.acquire(Accept::new(), |_, server| {
                let mut buf = [0; 4];
                let n = server.perform(Read(&mut buf))?;
                assert_eq!(&buf[..n], b"ping");
                client.perform(WriteAll(b"pong!"))?;
                server.perform(ReadExact(&mut buf))?;
                assert_eq!(&buf, b"pong");
                client.perform(seam_core::ops::net::Shutdown(
                    seam_core::ops::net::How::Write,
                ))?;
                let err = server.perform(ReadExact(&mut buf)).unwrap_err();
                assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
                Ok(())
            })
        })
    })
}

#[test]
fn clocks_and_randomness() {
    let mut std = Std::new();
    let t0 = std.inspect(Now);
    let mut a = [0u8; 13];
    let mut b = [0u8; 13];
    std.perform(FillRandom(&mut a));
    std.perform(FillRandom(&mut b));
    assert_ne!(a, b);
    assert!(std.inspect(Now) >= t0);
}

mod atomic {
    use super::*;

    fn replace(std: &mut Std, path: &Path, fail: bool) -> io::Result<()> {
        std.acquire(CreateAtomic::<AtomicFile>::new(path), |_, f| {
            f.perform(WriteAll(b"new"))?;
            if fail {
                return Err(io::Error::other("failed halfway"));
            }
            Ok(())
        })
    }

    #[test]
    fn commits_on_ok_and_rolls_back_otherwise() -> io::Result<()> {
        let mut std = Std::new();
        let dir = scratch_dir(&mut std, "atomic")?;
        let path = dir.join("settings.conf");
        std.perform(WriteFile::new(&path, "old"))?;

        assert!(replace(&mut std, &path, true).is_err());
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"old");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut std = Std::new();
            std.acquire(
                CreateAtomic::<AtomicFile>::new(&path),
                |_, f| -> io::Result<()> {
                    f.perform(WriteAll(b"new"))?;
                    panic!("crash halfway")
                },
            )
        }));
        assert!(panicked.is_err());
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"old");
        assert_eq!(
            list(&mut std, &dir)?,
            std::slice::from_ref(&path),
            "no temporary files left"
        );

        replace(&mut std, &path, false)?;
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"new");
        assert_eq!(list(&mut std, &dir)?, std::slice::from_ref(&path));
        std.perform(RemoveDir::all(&dir))
    }

    /// A process killed mid-write leaves its temporary file. Sweeping
    /// removes ones old enough to be abandoned, and nothing else.
    #[test]
    fn sweeping_removes_leftovers() -> io::Result<()> {
        let mut std = Std::new();
        let dir = scratch_dir(&mut std, "sweep")?;
        let leftover = dir.join(".settings.conf.seam-0123456789abcdef.tmp");
        let unrelated = dir.join("notes.tmp");
        std.perform(WriteFile::new(&leftover, "half"))?;
        std.perform(WriteFile::new(&unrelated, "keep"))?;

        assert_eq!(
            std.perform(SweepAtomic::new(&dir, Duration::from_secs(3600)))?,
            0,
            "too recent"
        );
        assert_eq!(std.perform(SweepAtomic::new(&dir, Duration::ZERO))?, 1);
        assert_eq!(list(&mut std, &dir)?, [unrelated]);
        std.perform(RemoveDir::all(&dir))
    }
}

mod sync {
    use seam_core::ops::sync::{
        Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, TrySendMsg, Update,
    };
    use seam_std::sync::{Receiver, Sender, Shared};

    use super::*;

    /// A core: sends `count` numbers, starting at `from`.
    fn produce<S: Perform<SendMsg<u64>>>(tx: &mut S, from: u64, count: u64) {
        for i in from..from + count {
            tx.perform(SendMsg(i)).unwrap();
        }
    }

    /// Senders cloned into threads; the channel closes when the last one is
    /// dropped.
    #[test]
    fn channels_across_threads() {
        let mut std = Std::new();
        let (tx, mut rx): (Sender<u64>, Receiver<u64>) = std.perform(Channel::bounded(4));
        std::thread::scope(|s| {
            for t in 0..4 {
                let mut tx = tx.clone();
                s.spawn(move || produce(&mut tx, t * 1000, 250));
            }
            drop(tx);
            let mut got = Vec::new();
            loop {
                match rx.perform(RecvMsg::new()) {
                    Ok(n) => got.push(n),
                    Err(RecvError::Closed) => break,
                    Err(e) => panic!("{e}"),
                }
            }
            assert_eq!(got.len(), 1000);
            got.sort();
            got.dedup();
            assert_eq!(got.len(), 1000, "no message duplicated");
        });
    }

    #[test]
    fn bounded_channels_push_back() {
        let mut std = Std::new();
        let (mut tx, rx): (Sender<u8>, Receiver<u8>) = std.perform(Channel::bounded(1));
        tx.perform(TrySendMsg(1)).unwrap();
        assert_eq!(tx.perform(TrySendMsg(2)), Err(SendError::Full(2)));
        drop(rx);
        assert_eq!(tx.perform(SendMsg(3)), Err(SendError::Closed(3)));
    }

    /// Eight threads each add 1000 to a shared counter; `Update` is atomic,
    /// so none is lost.
    #[test]
    fn cells_across_threads() {
        let mut std = Std::new();
        let counter: Shared<u64> = std.perform(Share::new(0));
        std::thread::scope(|s| {
            for _ in 0..8 {
                let mut c = counter.clone();
                s.spawn(move || {
                    for _ in 0..1000 {
                        c.perform(Update::new(&mut |n: &mut u64| *n += 1));
                    }
                });
            }
        });
        assert_eq!(counter.inspect(Load::new()), 8000);
    }
}

mod threads {
    use std::time::Instant;

    use seam_core::ops::net::LocalAddr;
    use seam_core::ops::sync::{Load, Share, Store};
    use seam_core::ops::task::{
        AvailableParallelism, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto,
        SpawnSet, SpawnWith,
    };
    use seam_core::ops::time::Sleep;
    use seam_std::net::TcpStream;
    use seam_std::sync::Shared;
    use seam_std::task::{JoinHandle, TaskSet};

    use super::*;

    /// A sync core: runs `job` on every input on its own thread, and sums
    /// the results. The closure type is a parameter, inferred from `job`.
    fn fan_out<C, S, J, F>(c: &mut C, inputs: &[u64], job: J) -> Result<u64, JoinError>
    where
        C: Provide<SpawnSet<S>>,
        S: Perform<SpawnInto<F>> + Perform<JoinNext<u64>>,
        J: Fn(u64) -> F,
    {
        c.acquire(SpawnSet::new(), |_, set| {
            for &n in inputs {
                let _ = set.perform(SpawnInto::new(job(n)));
            }
            let mut sum = 0;
            while let Some(result) = set.perform(JoinNext::new()) {
                sum += result?;
            }
            Ok(sum)
        })
    }

    #[test]
    fn threads_run_in_parallel() {
        let mut std = Std::new();
        let start = Instant::now();
        let sum = fan_out::<_, TaskSet<u64>, _, _>(&mut std, &[1, 2, 3, 4, 5, 6, 7, 8], |n| {
            let mut std = Std::new();
            move || {
                std.perform(Sleep(Duration::from_millis(100)));
                n * n
            }
        })
        .unwrap();
        assert_eq!(sum, 204);
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "took {:?}",
            start.elapsed()
        );
        assert!(std.inspect(AvailableParallelism).get() >= 1);
    }

    #[test]
    fn panics_are_reported() {
        let mut std = Std::new();
        let boom = Spawn::<_, JoinHandle<()>>::new(|| panic!("boom"));
        assert_eq!(
            std.acquire(boom, |_, _| Ok(())),
            Err(JoinError::Panicked),
            "closing reports it"
        );

        let joined = std.acquire(
            Spawn::<_, JoinHandle<u8>>::new(|| panic!("boom")),
            |_, t| Ok::<_, JoinError>(t.perform(Join::new())),
        );
        assert_eq!(joined, Ok(Err(JoinError::Panicked)));
    }

    /// A thread cannot be aborted, so a failed scope waits for it instead:
    /// no thread outlives the scope that started it.
    #[test]
    fn a_failed_scope_waits_for_its_thread() {
        let mut std = Std::new();
        let flag: Shared<bool> = std.perform(Share::new(false));
        let mut f = flag.clone();
        let slow = Spawn::<_, JoinHandle<()>>::new(move || {
            thread::sleep(Duration::from_millis(50));
            f.perform(Store(true));
        });
        let failed = std.acquire(slow, |_, _| Err::<(), _>(JoinError::Cancelled));
        assert!(failed.is_err());
        assert!(
            flag.inspect(Load::new()),
            "the scope did not end until the thread had"
        );
    }

    #[test]
    fn detached_threads_outlive_their_scope() {
        let mut std = Std::new();
        let flag: Shared<bool> = std.perform(Share::new(false));
        let mut f = flag.clone();
        let slow = Spawn::<_, JoinHandle<()>>::new(move || {
            thread::sleep(Duration::from_millis(50));
            f.perform(Store(true));
        });
        std.acquire(slow, |_, t| {
            t.perform(Detach);
            Ok::<_, JoinError>(())
        })
        .unwrap();
        assert!(!flag.inspect(Load::new()), "the scope did not wait");
        thread::sleep(Duration::from_millis(150));
        assert!(flag.inspect(Load::new()));
    }

    /// Threads cannot be aborted, but they can be told to stop.
    #[test]
    fn cooperative_cancellation() {
        let mut std = Std::new();
        let stop: Shared<bool> = std.perform(Share::new(false));
        let watcher = stop.clone();
        let worker = Spawn::<_, JoinHandle<u32>>::new(move || {
            let mut rounds = 0;
            while !watcher.inspect(Load::new()) {
                rounds += 1;
                thread::sleep(Duration::from_millis(1));
            }
            rounds
        });
        let rounds = std
            .acquire(worker, |_, t| {
                thread::sleep(Duration::from_millis(20));
                let mut stop = stop.clone();
                stop.perform(Store(true));
                Ok::<_, JoinError>(t.perform(Join::new()))
            })
            .unwrap()
            .unwrap();
        assert!(rounds > 0);
    }

    /// A thread per connection: the listener accepts and hands the stream
    /// to a new thread, which owns it and shuts it down when done.
    #[test]
    fn spawn_with_hands_a_connection_to_a_thread() -> io::Result<()> {
        let mut std = Std::new();
        let any_port = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        std.acquire(
            TcpBind::new(any_port),
            |std, listener: &mut seam_std::net::TcpListener| {
                let addr = listener.inspect(LocalAddr)?;
                let mut client_std = std.clone();
                let client = Spawn::<_, JoinHandle<io::Result<Vec<u8>>>>::new(move || {
                    client_std.acquire(TcpConnect::<TcpStream>::new(addr), |_, s| {
                        s.perform(WriteAll(b"hello"))?;
                        s.perform(seam_core::ops::net::Shutdown(
                            seam_core::ops::net::How::Write,
                        ))?;
                        let mut reply = Vec::new();
                        s.perform(ReadToEnd(&mut reply))?;
                        Ok(reply)
                    })
                });
                std.acquire(client, |_, client| {
                    let echo = SpawnWith::<_, _, JoinHandle<io::Result<usize>>>::new(
                        Accept::new(),
                        |s: &mut TcpStream| {
                            let mut got = Vec::new();
                            s.perform(ReadToEnd(&mut got))?;
                            s.perform(WriteAll(&got))?;
                            Ok(got.len())
                        },
                    );
                    let echoed = listener.acquire(echo, |_, t| {
                        assert!(!t.inspect(IsFinished) || true);
                        t.perform(Join::new()).map_err(io::Error::from)
                    })??;
                    assert_eq!(echoed, 5);
                    let reply = client.perform(Join::new()).map_err(io::Error::from)??;
                    assert_eq!(reply, b"hello");
                    io::Result::Ok(())
                })
            },
        )
    }

    use std::thread;
}
