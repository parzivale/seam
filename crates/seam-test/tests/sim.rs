//! Cores written against seam bounds, run in the simulation.
#![cfg(feature = "full")]

use seam_core::io::{self, ErrorKind};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use seam_core::future::{SendPerform, block_on};
use seam_core::ops::env::{Args, Var};
use seam_core::ops::fs::{
    CreateDir, Metadata, NextEntry, Open, ReadDir, ReadFile, ReadToString, RemoveDir, RemoveFile,
    Rename, WriteFile,
};
use seam_core::ops::io::{Read, ReadExact, ReadToEnd, WriteAll};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, ResolveInto, SendTo, Shutdown, TcpBind,
    TcpConnect, UdpBind,
};
use seam_core::ops::process::{Exit, Output, RunCommand};
use seam_core::ops::random::RandomU64;
use seam_core::ops::signal::CtrlC;
use seam_core::ops::stdio::{Line, Print, ReadLine, ReadLineInto};
use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{Acquire, AsyncAcquire, Inspect, Perform, Provide};
use seam_test::fs::File;
use seam_test::net::{TcpListener, TcpStream, UdpSocket};
use seam_test::{Sim, catch_exit};

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

fn kind<T: std::fmt::Debug>(r: io::Result<T>) -> ErrorKind {
    r.unwrap_err().kind()
}

#[test]
fn sim_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Sim>();
    check::<File>();
    check::<TcpStream>();
}

#[test]
fn same_seed_same_world() {
    let draw = |seed| {
        let mut sim = Sim::with_seed(seed);
        [sim.perform(RandomU64), sim.perform(RandomU64)]
    };
    assert_eq!(draw(1), draw(1));
    assert_ne!(draw(1), draw(2));
}

#[test]
fn clocks_are_virtual() {
    let mut sim = Sim::new();
    let t0 = sim.inspect(Now);
    sim.perform(Sleep(Duration::from_secs(3600)));
    assert_eq!(sim.inspect(Now) - t0, Duration::from_secs(3600));
    assert_eq!(
        sim.inspect(WallClock),
        std::time::UNIX_EPOCH + Duration::from_secs(3600)
    );
}

/// A core: an append-only log with rotation, written only against bounds.
fn append_log<C, F>(c: &mut C, dir: &Path, line: &str, max: u64) -> io::Result<()>
where
    C: for<'a> Provide<Open<'a, F>>
        + for<'a> Inspect<Metadata<'a>>
        + for<'a> Perform<Rename<'a>>
        + for<'a> Perform<CreateDir<'a>>,
    F: for<'a> Perform<WriteAll<'a>>,
{
    c.perform(CreateDir::all(dir))?;
    let log = dir.join("app.log");
    if c.inspect(Metadata::new(&log)).is_ok_and(|m| m.len >= max) {
        c.perform(Rename::new(&log, &dir.join("app.log.1")))?;
    }
    c.acquire(Open::append(&log), |_, f| {
        f.perform(WriteAll(line.as_bytes()))
    })
}

#[test]
fn filesystem_core() {
    let mut sim = Sim::new();
    let dir = Path::new("/var/log");
    for line in ["one\n", "two\n", "three\n"] {
        append_log(&mut sim, dir, line, 8).unwrap();
    }
    assert_eq!(sim.file("/var/log/app.log.1").unwrap(), b"one\ntwo\n");
    assert_eq!(sim.file("/var/log/app.log").unwrap(), b"three\n");
    assert_eq!(
        list(&mut sim, "/var/log").unwrap(),
        [
            PathBuf::from("/var/log/app.log"),
            PathBuf::from("/var/log/app.log.1")
        ]
    );
}

#[test]
fn filesystem_errors_match_a_real_system() {
    let mut sim = Sim::new();
    sim.add_file("/a/file", "x");
    assert_eq!(
        kind(sim.inspect(ReadFile::new("/missing"))),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(sim.inspect(ReadFile::new("/a"))),
        ErrorKind::IsADirectory
    );
    assert_eq!(
        kind(sim.inspect(ReadFile::new("/a/file/x"))),
        ErrorKind::NotADirectory
    );
    assert_eq!(
        kind(sim.perform(WriteFile::new("/nope/f", "x"))),
        ErrorKind::NotFound
    );
    assert_eq!(
        kind(sim.perform(CreateDir::new("/a"))),
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        kind(sim.perform(RemoveDir::new("/a"))),
        ErrorKind::DirectoryNotEmpty
    );
    assert_eq!(
        kind(sim.perform(RemoveFile::new("/a"))),
        ErrorKind::IsADirectory
    );
    assert_eq!(
        kind(sim.perform(Rename::new("/a", "/a/b"))),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        kind(sim.acquire(Open::<File>::create("/a"), |_, _| Ok(()))),
        ErrorKind::IsADirectory
    );

    sim.add_file("/bad", [0xff]);
    assert_eq!(
        kind(sim.inspect(ReadToString::new("/bad"))),
        ErrorKind::InvalidData
    );

    sim.perform(RemoveDir::all("/a")).unwrap();
    assert!(!sim.exists("/a/file"));
}

#[test]
fn relative_paths_and_open_handles() {
    let mut sim = Sim::new();
    sim.set_current_dir("/home/me");
    sim.add_dir("/home/me");
    sim.perform(WriteFile::new("notes/../a.txt", "hi"))
        .unwrap_err();
    sim.add_dir("notes");
    sim.perform(WriteFile::new("notes/../a.txt", "hi")).unwrap();
    assert_eq!(sim.file("/home/me/a.txt").unwrap(), b"hi");

    // A handle survives its path being renamed, as on Unix. The body gets
    // the capability back, so it can rename while it holds the file.
    sim.acquire(Open::<File>::read("a.txt"), |sim, f| {
        sim.perform(Rename::new("a.txt", "b.txt"))?;
        let mut out = Vec::new();
        f.perform(ReadToEnd(&mut out))?;
        assert_eq!(out, b"hi");
        assert_eq!(
            kind(f.perform(WriteAll(b"no"))),
            ErrorKind::PermissionDenied
        );
        io::Result::Ok(())
    })
    .unwrap();
}

#[test]
fn tcp_in_memory() -> io::Result<()> {
    let mut sim = Sim::new();
    let bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080));
    sim.acquire(TcpBind::<TcpListener>::new(bind), |sim, listener| {
        assert_eq!(
            kind(sim.acquire(TcpBind::<TcpListener>::new(bind), |_, _| Ok(()))),
            ErrorKind::AddrInUse
        );
        assert_eq!(
            kind(listener.acquire(Accept::<TcpStream>::new(), |_, _| Ok(()))),
            ErrorKind::WouldBlock
        );

        let addr = sim.inspect(Resolve::new("localhost", 8080))?[0];
        sim.acquire(TcpConnect::<TcpStream>::new(addr), |_, client| {
            listener.acquire(Accept::new(), |_, server| {
                assert_eq!(server.inspect(PeerAddr)?, client.inspect(LocalAddr)?);
                assert_eq!(client.inspect(PeerAddr)?, addr);

                let mut buf = [0; 8];
                assert_eq!(kind(server.perform(Read(&mut buf))), ErrorKind::WouldBlock);
                client.perform(WriteAll(b"ping"))?;
                client.perform(Shutdown(seam_core::ops::net::How::Write))?;
                assert_eq!(server.perform(Read(&mut buf))?, 4);
                assert_eq!(&buf[..4], b"ping");
                assert_eq!(
                    server.perform(Read(&mut buf))?,
                    0,
                    "end of stream after shutdown"
                );
                io::Result::Ok(())
            })?;
            // The server's end is gone now that its scope is over.
            client.perform(Shutdown(seam_core::ops::net::How::Read))?;
            assert_eq!(kind(client.perform(WriteAll(b"x"))), ErrorKind::BrokenPipe);
            io::Result::Ok(())
        })
    })?;
    assert_eq!(
        kind(sim.acquire(
            TcpConnect::<TcpStream>::new(([127, 0, 0, 1], 9)),
            |_, _| Ok(())
        )),
        ErrorKind::ConnectionRefused
    );
    Ok(())
}

/// When a stream's scope succeeds it is shut down cleanly, so the peer sees
/// end of input.
#[test]
fn closing_a_stream_shuts_it_down() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.acquire(
        TcpBind::<TcpListener>::new(([127, 0, 0, 1], 80)),
        |sim, listener| {
            sim.acquire(
                TcpConnect::<TcpStream>::new(([127, 0, 0, 1], 80)),
                |_, client| {
                    listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                        server.perform(WriteAll(b"bye"))
                    })?;
                    let mut reply = Vec::new();
                    client.perform(ReadToEnd(&mut reply))?;
                    assert_eq!(reply, b"bye");
                    assert_eq!(client.perform(Read(&mut [0; 4]))?, 0);
                    Ok(())
                },
            )
        },
    )
}

#[test]
fn udp_in_memory() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_host("peer.test", Ipv4Addr::new(10, 0, 0, 2));
    let to = sim.inspect(Resolve::new("peer.test", 53))?[0];
    sim.acquire(UdpBind::<UdpSocket>::new(([10, 0, 0, 1], 0)), |sim, a| {
        sim.acquire(UdpBind::<UdpSocket>::new(([10, 0, 0, 2], 53)), |_, b| {
            a.perform(SendTo {
                buf: b"hello",
                addr: to,
            })?;
            let mut buf = [0; 3];
            let (n, from) = b.perform(RecvFrom(&mut buf))?;
            assert_eq!((n, &buf, from), (3, b"hel", a.inspect(LocalAddr)?));
            assert_eq!(kind(b.perform(RecvFrom(&mut buf))), ErrorKind::WouldBlock);
            Ok(())
        })
    })
}

/// A core: a tiny shell that runs the commands it reads until `exit`.
fn shell<C>(c: &mut C) -> io::Result<()>
where
    C: Perform<ReadLine> + for<'a> Perform<Print<'a>> + Perform<RunCommand> + Perform<Exit>,
{
    while let Some(line) = c.perform(ReadLine)? {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("exit") => {
                let code = words.next().map_or(Ok(0), str::parse).unwrap_or(2);
                c.perform(Exit(code));
            }
            Some(program) => {
                let mut cmd = Command::new(program);
                cmd.args(words);
                match c.perform(RunCommand(cmd)) {
                    Ok(out) => c.perform(Print(&out.stdout))?,
                    Err(e) => c.perform(Print(format!("{program}: {e}\n").as_bytes()))?,
                }
            }
            None => {}
        }
    }
    Ok(())
}

#[test]
fn scripted_processes_stdio_and_exit() {
    let mut sim = Sim::new();
    sim.feed_stdin("echo hi there\nnope\nexit 3\nnever read\n")
        .on_command("echo", |cmd| {
            let words: Vec<_> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            Ok(Output {
                code: Some(0),
                stdout: format!("{}\n", words.join(" ")).into_bytes(),
                stderr: Vec::new(),
            })
        });

    assert_eq!(catch_exit(|| shell(&mut sim)).map(drop), Err(3));
    assert_eq!(
        String::from_utf8(sim.take_stdout()).unwrap(),
        "hi there\nnope: no handler scripted for this program\n"
    );
    let ran = &sim.invocations()[0];
    assert_eq!(ran.program, "echo");
    assert_eq!(ran.args, [OsString::from("hi"), OsString::from("there")]);
    assert_eq!(
        sim.perform(ReadLine).unwrap().as_deref(),
        Some("never read\n")
    );
    assert_eq!(sim.perform(ReadLine).unwrap(), None);
}

#[test]
fn env_and_signals() {
    let mut sim = Sim::new();
    sim.set_var("HOME", "/home/me")
        .set_args(["prog", "--verbose"]);
    assert_eq!(sim.inspect(Var::new("HOME")), Some("/home/me".into()));
    assert_eq!(sim.inspect(Var::new("NOPE")), None);
    assert_eq!(sim.inspect(Args).len(), 2);

    assert_eq!(kind(sim.perform(CtrlC)), ErrorKind::WouldBlock);
    sim.raise_ctrl_c();
    sim.perform(CtrlC).unwrap();
}

/// An async core with `Send` bounds, the kind that runs on Tokio, runs in
/// the simulation through the blanket lifts.
#[test]
fn async_send_core() {
    async fn retry<
        C: SendPerform<RandomU64> + SendPerform<Sleep> + for<'a> SendPerform<WriteFile<'a>>,
    >(
        c: &mut C,
    ) -> io::Result<()> {
        for attempt in 0.. {
            let ms = 10 << attempt;
            let jitter = c.perform_send(RandomU64).await % 10;
            c.perform_send(Sleep(Duration::from_millis(ms + jitter)))
                .await;
            if attempt == 3 {
                return c.perform_send(WriteFile::new("/tmp/done", "ok")).await;
            }
        }
        unreachable!()
    }

    let mut sim = Sim::with_seed(42);
    block_on(retry(&mut sim)).unwrap();
    assert_eq!(sim.file("/tmp/done").unwrap(), b"ok");
    let waited = sim.inspect(Now).since_epoch();
    assert!(waited >= Duration::from_millis(150) && waited < Duration::from_millis(190));
}

mod atomic {
    use seam_core::ops::fs::{CreateAtomic, SweepAtomic};
    use seam_test::fs::AtomicFile;

    use super::*;

    fn sim() -> Sim {
        let mut sim = Sim::new();
        sim.add_file("/etc/app.conf", "old");
        sim
    }

    fn listing(sim: &mut Sim) -> Vec<PathBuf> {
        list(sim, "/etc").unwrap()
    }

    fn create() -> CreateAtomic<'static, AtomicFile> {
        CreateAtomic::new("/etc/app.conf")
    }

    #[test]
    fn the_temporary_file_is_visible_until_the_scope_succeeds() {
        let mut sim = sim();
        sim.acquire(create(), |sim, f| {
            f.perform(WriteAll(b"new"))?;
            assert_eq!(
                listing(sim),
                [
                    PathBuf::from("/etc/.app.conf.seam-0.tmp"),
                    PathBuf::from("/etc/app.conf")
                ]
            );
            assert_eq!(sim.file("/etc/app.conf").unwrap(), b"old");
            io::Result::Ok(())
        })
        .unwrap();
        assert_eq!(listing(&mut sim), [PathBuf::from("/etc/app.conf")]);
        assert_eq!(sim.file("/etc/app.conf").unwrap(), b"new");
    }

    #[test]
    fn errors_and_panics_roll_back() {
        let mut sim = sim();
        let err = sim.acquire(create(), |_, f| {
            f.perform(WriteAll(b"half"))?;
            Err::<(), _>(io::Error::other("validation failed"))
        });
        assert!(err.is_err());

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sim.acquire(create(), |_, f| -> io::Result<()> {
                f.perform(WriteAll(b"half"))?;
                panic!("crash")
            })
        }));
        assert!(panicked.is_err());

        assert_eq!(sim.file("/etc/app.conf").unwrap(), b"old");
        assert_eq!(listing(&mut sim), [PathBuf::from("/etc/app.conf")]);
    }

    /// Dropping the scope's future at an `.await` is a rollback.
    #[test]
    fn cancellation_rolls_back() {
        struct Suspend(bool);
        impl std::future::Future for Suspend {
            type Output = ();
            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<()> {
                if std::mem::replace(&mut self.0, true) {
                    return std::task::Poll::Ready(());
                }
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }

        let mut sim = sim();
        {
            let fut = sim.acquire_async(create(), async |_, f| {
                f.perform_send(WriteAll(b"half")).await?;
                Suspend(false).await;
                io::Result::Ok(())
            });
            let mut fut = std::pin::pin!(fut);
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(fut.as_mut().poll(&mut cx).is_pending());
        }
        assert_eq!(sim.file("/etc/app.conf").unwrap(), b"old");
        assert_eq!(listing(&mut sim), [PathBuf::from("/etc/app.conf")]);
    }

    /// A leftover from a process killed mid-write is swept once it is old
    /// enough; a write in progress is not.
    #[test]
    fn sweeping_removes_only_old_leftovers() {
        let mut sim = sim();
        sim.add_file("/etc/.app.conf.seam-crashed.tmp", "half");
        sim.advance(Duration::from_secs(600));
        sim.acquire(create(), |sim, f| {
            f.perform(WriteAll(b"new"))?;
            assert_eq!(
                sim.perform(SweepAtomic::new("/etc", Duration::from_secs(60)))?,
                1
            );
            Ok::<_, io::Error>(())
        })
        .unwrap();
        assert_eq!(listing(&mut sim), [PathBuf::from("/etc/app.conf")]);
        assert_eq!(sim.file("/etc/app.conf").unwrap(), b"new");
    }
}

#[test]
fn read_line_into_a_buffer() {
    let mut sim = Sim::new();
    sim.feed_stdin("ab\ncdefg\nh");
    let mut buf = [0; 4];
    let mut read = |sim: &mut Sim| {
        sim.perform(ReadLineInto(&mut buf))
            .unwrap()
            .map(|Line { len, complete }| (buf[..len].to_vec(), complete))
    };
    assert_eq!(read(&mut sim), Some((b"ab\n".to_vec(), true)));
    // Too long for the buffer: the rest comes with the next read.
    assert_eq!(read(&mut sim), Some((b"cdef".to_vec(), false)));
    assert_eq!(read(&mut sim), Some((b"g\n".to_vec(), true)));
    // The input ends without a newline, which ends the line too.
    assert_eq!(read(&mut sim), Some((b"h".to_vec(), true)));
    assert_eq!(read(&mut sim), None);

    // A line that fills the buffer exactly is not known to have ended.
    sim.feed_stdin("abcd");
    assert_eq!(read(&mut sim), Some((b"abcd".to_vec(), false)));
    assert_eq!(read(&mut sim), None);
}

#[test]
fn resolve_into_a_buffer() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_host("many.test", Ipv4Addr::new(10, 0, 0, 1))
        .add_host("many.test", Ipv4Addr::new(10, 0, 0, 2))
        .add_host("many.test", Ipv4Addr::new(10, 0, 0, 3));
    let none = SocketAddr::from(([0, 0, 0, 0], 0));
    let mut out = [none; 2];
    // Three addresses; the buffer holds the first two.
    assert_eq!(sim.inspect(ResolveInto::new("many.test", 80, &mut out))?, 3);
    assert_eq!(out, sim.inspect(Resolve::new("many.test", 80))?[..2]);
    assert_eq!(sim.inspect(ResolveInto::new("10.9.9.9", 7, &mut out))?, 1);
    assert_eq!(out[0], SocketAddr::from(([10, 9, 9, 9], 7)));
    assert_eq!(
        kind(sim.inspect(ResolveInto::new("nowhere.test", 80, &mut out))),
        ErrorKind::NotFound
    );
    Ok(())
}

#[test]
fn read_exact_fills_or_fails() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_file("/data", "abcdef");
    sim.acquire(Open::<File>::read("/data"), |_, f| {
        let mut buf = [0; 4];
        f.perform(ReadExact(&mut buf))?;
        assert_eq!(&buf, b"abcd");
        assert_eq!(
            kind(f.perform(ReadExact(&mut buf))),
            ErrorKind::UnexpectedEof
        );
        assert_eq!(&buf[..2], b"ef");
        io::Result::Ok(())
    })?;

    // A stream takes nothing until the whole buffer is there.
    sim.acquire(
        TcpBind::<TcpListener>::new(([127, 0, 0, 1], 80)),
        |sim, listener| {
            sim.acquire(
                TcpConnect::<TcpStream>::new(([127, 0, 0, 1], 80)),
                |_, client| {
                    listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                        let mut buf = [0; 4];
                        client.perform(WriteAll(b"pi"))?;
                        assert_eq!(
                            kind(server.perform(ReadExact(&mut buf))),
                            ErrorKind::WouldBlock
                        );
                        client.perform(WriteAll(b"ng!"))?;
                        server.perform(ReadExact(&mut buf))?;
                        assert_eq!(&buf, b"ping");
                        client.perform(Shutdown(seam_core::ops::net::How::Write))?;
                        assert_eq!(
                            kind(server.perform(ReadExact(&mut buf))),
                            ErrorKind::UnexpectedEof
                        );
                        assert_eq!(buf[0], b'!');
                        io::Result::Ok(())
                    })
                },
            )
        },
    )
}

#[test]
fn listing_a_directory_one_entry_at_a_time() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_file("/d/a", "1")
        .add_file("/d/b", "2")
        .add_dir("/d/sub");
    // A core can stop after any entry.
    let first = sim.acquire(ReadDir::new("/d"), |_, d| d.perform(NextEntry))?;
    assert_eq!(first, Some(PathBuf::from("/d/a")));
    assert_eq!(
        list(&mut sim, "/d")?,
        [
            PathBuf::from("/d/a"),
            PathBuf::from("/d/b"),
            PathBuf::from("/d/sub")
        ]
    );
    assert_eq!(kind(list(&mut sim, "/nope")), ErrorKind::NotFound);
    assert_eq!(kind(list(&mut sim, "/d/a")), ErrorKind::NotADirectory);
    Ok(())
}
