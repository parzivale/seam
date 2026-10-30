//! Cores written against seam bounds, run in the simulation.
#![cfg(feature = "full")]

use seam_core::io::{self, ErrorKind};
use seam_core::ops::fs::{
    create_dir_all, read_file, read_to_string, sweep_atomic, write_file, write_file_send,
};

use seam_core::ops::io::{
    BufReader, Line, Write, read_exact, read_to_end, write_all, write_all_send,
};
use seam_core::ops::net::resolve;
use seam_core::ops::random::{FillRandom, random_u64, random_u64_send};
use seam_core::ops::stdio::{OpenStdin, OpenStdout, print};
use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use seam_core::future::{SendPerform, block_on};
use seam_core::ops::env::{Args, Var};
use seam_core::ops::fs::{
    Append, OpenOptions, ReadOnly, ReadWrite, SetLen, Stat, SyncAll, SyncData,
};
use seam_core::ops::fs::{
    CreateDir, Metadata, NextEntry, Open, ReadDir, RemoveDir, RemoveFile, Rename,
};
use seam_core::ops::io::Read;
use seam_core::ops::io::{Seek, SeekFrom};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, ResolveInto, SendTo, Shutdown, TcpBind, TcpConnect,
    UdpBind,
};
use seam_core::ops::process::{Exit, Output, RunCommand};
use seam_core::ops::signal::CtrlC;

use seam_core::ops::fs::{ReadAt, View};
use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{AsyncProvide, Inspect, Lend, Perform, Provide};
use seam_test::fs::File;
use seam_test::net::{TcpListener, TcpStream, UdpSocket};
use seam_test::{Sim, catch_exit};

/// Edits a file in place through one handle: every file handle operation.
/// Generic, so it runs on any handler.
fn edit_in_place<C, F, R>(c: &mut C, path: &std::path::Path) -> io::Result<()>
where
    C: for<'a> Provide<Open<'a, ReadWrite, F>> + for<'a> Lend<View<'a, R>>,
    R: for<'a> Inspect<ReadAt<'a>>,
    F: Perform<Seek>
        + for<'a> Perform<Write<'a>>
        + for<'a> Perform<Read<'a>>
        + Perform<SetLen>
        + Perform<SyncAll>
        + Perform<SyncData>
        + Inspect<Stat>,
{
    let options = OpenOptions {
        create: true,
        truncate: true,
        ..OpenOptions::default()
    };
    c.acquire(Open::<ReadWrite, F>::new(path, options), |_, f| {
        write_all(f, b"hello world")?;
        assert_eq!(f.perform(Seek(SeekFrom::Start(6)))?, 6);
        let mut word = [0; 5];
        read_exact(f, &mut word)?;
        assert_eq!(&word, b"world");
        assert_eq!(f.perform(Seek(SeekFrom::End(-5)))?, 6);
        write_all(f, b"WORLD")?;
        assert_eq!(f.perform(Seek(SeekFrom::Current(-11)))?, 0);
        f.perform(SetLen(5))?;
        assert_eq!(f.inspect(Stat)?.len, 5);
        // Past the end: the gap fills with zeros.
        assert_eq!(f.perform(Seek(SeekFrom::End(2)))?, 7);
        write_all(f, b"!")?;
        f.perform(SyncData)?;
        f.perform(SyncAll)?;
        let err = f.perform(Seek(SeekFrom::Current(-100))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let info = f.inspect(Stat)?;
        assert!(info.is_file());
        assert_eq!(info.len, 8);
        io::Result::Ok(())
    })?;
    assert_eq!(read_file(c, path)?, b"hello\0\0!");
    // Truncating a file opened read-only does not compile: `File<ReadOnly>`
    // has no `SetLen`.
    Ok(())
}

/// Lists `dir` through `ReadDir`, one entry at a time.
fn list<C, D>(c: &mut C, dir: impl AsRef<std::path::Path>) -> io::Result<Vec<std::path::PathBuf>>
where
    C: for<'a> seam_core::Provide<ReadDir<'a, D>>,
    D: seam_core::Perform<NextEntry>,
{
    seam_core::Provide::acquire(c, ReadDir::new(dir.as_ref()), |_, entries| {
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
    check::<File<ReadOnly>>();
    check::<TcpStream>();
}

#[test]
fn same_seed_same_world() {
    let draw = |seed| {
        let mut sim = Sim::with_seed(seed);
        [random_u64(&mut sim), random_u64(&mut sim)]
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
    C: for<'a> Provide<Open<'a, Append, F>>
        + for<'a> Inspect<Metadata<'a>>
        + for<'a> Perform<Rename<'a>>
        + for<'a> Perform<CreateDir<'a>>,
    F: for<'a> Perform<Write<'a>>,
{
    create_dir_all(c, dir)?;
    let log = dir.join("app.log");
    if c.inspect(Metadata::new(&log)).is_ok_and(|m| m.len >= max) {
        c.perform(Rename::new(&log, &dir.join("app.log.1")))?;
    }
    c.acquire(Open::append(&log), |_, f| write_all(f, line.as_bytes()))
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
    assert_eq!(kind(read_file(&sim, "/missing")), ErrorKind::NotFound);
    assert_eq!(kind(read_file(&sim, "/a")), ErrorKind::IsADirectory);
    assert_eq!(kind(read_file(&sim, "/a/file/x")), ErrorKind::NotADirectory);
    assert_eq!(
        kind(write_file(&mut sim, "/nope/f", "x")),
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
        kind(sim.acquire(Open::<_, File<_>>::create("/a"), |_, _| Ok(()))),
        ErrorKind::IsADirectory
    );

    sim.add_file("/bad", [0xff]);
    assert_eq!(kind(read_to_string(&sim, "/bad")), ErrorKind::InvalidData);

    sim.perform(RemoveDir::all("/a")).unwrap();
    assert!(!sim.exists("/a/file"));
}

#[test]
fn relative_paths_and_open_handles() {
    let mut sim = Sim::new();
    sim.set_current_dir("/home/me");
    sim.add_dir("/home/me");
    write_file(&mut sim, "notes/../a.txt", "hi").unwrap_err();
    sim.add_dir("notes");
    write_file(&mut sim, "notes/../a.txt", "hi").unwrap();
    assert_eq!(sim.file("/home/me/a.txt").unwrap(), b"hi");

    // A handle survives its path being renamed, as on Unix. The body gets
    // the capability back, so it can rename while it holds the file.
    sim.acquire(Open::<_, File<_>>::read("a.txt"), |sim, f| {
        sim.perform(Rename::new("a.txt", "b.txt"))?;
        let mut out = Vec::new();
        read_to_end(f, &mut out)?;
        assert_eq!(out, b"hi");
        // Writing to it does not compile: it was opened `ReadOnly`.
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

        let addr = resolve(sim, "localhost", 8080)?[0];
        sim.acquire(TcpConnect::<TcpStream>::new(addr), |_, client| {
            listener.acquire(Accept::new(), |_, server| {
                assert_eq!(server.inspect(PeerAddr)?, client.inspect(LocalAddr)?);
                assert_eq!(client.inspect(PeerAddr)?, addr);

                let mut buf = [0; 8];
                assert_eq!(kind(server.perform(Read(&mut buf))), ErrorKind::WouldBlock);
                write_all(client, b"ping")?;
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
            assert_eq!(kind(write_all(client, b"x")), ErrorKind::BrokenPipe);
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
                        write_all(server, b"bye")
                    })?;
                    let mut reply = Vec::new();
                    read_to_end(client, &mut reply)?;
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
    let to = resolve(&sim, "peer.test", 53)?[0];
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
fn shell<C, I, O>(c: &mut C) -> io::Result<()>
where
    C: Provide<OpenStdin<I>> + Provide<OpenStdout<O>> + Perform<RunCommand> + Perform<Exit>,
    I: for<'a> Perform<Read<'a>>,
    O: for<'a> Perform<Write<'a>>,
{
    let mut reader = BufReader::<256>::new();
    let mut buf = [0; 128];
    c.acquire(OpenStdin::new(), |c, stdin| {
        while let Some(line) = reader.read_line(stdin, &mut buf)? {
            let line = String::from_utf8_lossy(&buf[..line.len]).into_owned();
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
                        Ok(out) => print(c, &out.stdout)?,
                        Err(e) => print(c, format!("{program}: {e}\n").as_bytes())?,
                    }
                }
                None => {}
            }
        }
        Ok(())
    })
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
        sim.invocations().len(),
        2,
        "`exit` stopped it before `never`"
    );
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
    async fn retry<C, W>(c: &mut C) -> io::Result<()>
    where
        C: for<'r> SendPerform<FillRandom<'r>>
            + SendPerform<Sleep>
            + for<'a> seam_core::SendProvide<Open<'a, seam_core::ops::fs::WriteOnly, W>>
            + Send,
        W: for<'a> SendPerform<Write<'a>> + Send,
    {
        for attempt in 0.. {
            let ms = 10 << attempt;
            let jitter = random_u64_send(c).await % 10;
            c.perform_send(Sleep(Duration::from_millis(ms + jitter)))
                .await;
            if attempt == 3 {
                return write_file_send(c, "/tmp/done", "ok").await;
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
    use seam_core::ops::fs::CreateAtomic;
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
            write_all(f, b"new")?;
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
            write_all(f, b"half")?;
            Err::<(), _>(io::Error::other("validation failed"))
        });
        assert!(err.is_err());

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sim.acquire(create(), |_, f| -> io::Result<()> {
                write_all(f, b"half")?;
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
                write_all_send(f, b"half").await?;
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
            write_all(f, b"new")?;
            assert_eq!(sweep_atomic(sim, "/etc", Duration::from_secs(60))?, 1);
            Ok::<_, io::Error>(())
        })
        .unwrap();
        assert_eq!(listing(&mut sim), [PathBuf::from("/etc/app.conf")]);
        assert_eq!(sim.file("/etc/app.conf").unwrap(), b"new");
    }
}

#[test]
fn lines_from_stdin_through_a_reader() {
    let mut sim = Sim::new();
    sim.feed_stdin("ab\ncdefg\nh");
    // One reader across scopes: it keeps what it read past a line.
    let mut reader = BufReader::<3>::new();
    let mut buf = [0; 4];
    let mut read = |sim: &mut Sim| {
        let line = sim
            .acquire(OpenStdin::<seam_test::stdio::Stdin>::new(), |_, stdin| {
                reader.read_line(stdin, &mut buf)
            })
            .unwrap();
        line.map(|Line { len, complete }| (buf[..len].to_vec(), complete))
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
    assert_eq!(out, resolve(&sim, "many.test", 80)?[..2]);
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
    sim.acquire(Open::<_, File<_>>::read("/data"), |_, f| {
        let mut buf = [0; 4];
        read_exact(f, &mut buf)?;
        assert_eq!(&buf, b"abcd");
        assert_eq!(kind(read_exact(f, &mut buf)), ErrorKind::UnexpectedEof);
        assert_eq!(&buf[..2], b"ef");
        io::Result::Ok(())
    })?;

    // On a stream, a read takes what has arrived.
    sim.acquire(
        TcpBind::<TcpListener>::new(([127, 0, 0, 1], 80)),
        |sim, listener| {
            sim.acquire(
                TcpConnect::<TcpStream>::new(([127, 0, 0, 1], 80)),
                |_, client| {
                    listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                        let mut buf = [0; 4];
                        write_all(client, b"pi")?;
                        // Nothing in the plain simulation waits: what arrived is
                        // kept, then the read would block.
                        assert_eq!(kind(read_exact(server, &mut buf)), ErrorKind::WouldBlock);
                        assert_eq!(&buf[..2], b"pi");
                        write_all(client, b"ng")?;
                        read_exact(server, &mut buf[2..])?;
                        assert_eq!(&buf, b"ping");
                        client.perform(Shutdown(seam_core::ops::net::How::Write))?;
                        assert_eq!(kind(read_exact(server, &mut buf)), ErrorKind::UnexpectedEof);
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

#[test]
fn file_handles() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.add_dir("/d");
    edit_in_place(&mut sim, std::path::Path::new("/d/f"))?;
    // A handle's metadata follows the file, not the path.
    sim.acquire(Open::<_, File<_>>::read("/d/f"), |sim, f| {
        sim.perform(RemoveFile::new("/d/f"))?;
        assert_eq!(f.inspect(Stat)?.len, 8);
        io::Result::Ok(())
    })?;
    // An atomic file has them too, on its temporary file.
    sim.acquire(
        seam_core::ops::fs::CreateAtomic::<seam_test::fs::AtomicFile>::new("/d/g"),
        |_, f| {
            write_all(f, b"abc")?;
            f.perform(SetLen(1))?;
            assert_eq!(f.perform(Seek(SeekFrom::Current(0)))?, 3);
            assert_eq!(f.inspect(Stat)?.len, 1);
            io::Result::Ok(())
        },
    )?;
    assert_eq!(sim.file("/d/g").unwrap(), b"a");
    Ok(())
}

#[test]
fn setting_and_removing_variables() {
    use seam_core::ops::env::{RemoveVar, SetVar};
    let mut sim = Sim::new();
    sim.perform(SetVar::new("MODE", "test")).unwrap();
    assert_eq!(sim.inspect(Var::new("MODE")), Some("test".into()));
    sim.perform(RemoveVar::new("MODE")).unwrap();
    assert_eq!(sim.inspect(Var::new("MODE")), None);
    sim.perform(RemoveVar::new("NEVER_SET")).unwrap();
    for (name, value) in [("", "x"), ("A=B", "x"), ("A\0", "x"), ("A", "x\0")] {
        assert_eq!(
            kind(sim.perform(SetVar::new(name, value))),
            ErrorKind::InvalidInput,
            "{name:?}={value:?}"
        );
    }
    assert_eq!(
        kind(sim.perform(RemoveVar::new("A=B"))),
        ErrorKind::InvalidInput
    );
}

#[test]
fn create_dir_all_climbs_only_past_what_is_missing() {
    let mut sim = Sim::new();
    create_dir_all(&mut sim, "/a/b").unwrap();
    // Some of it there already, then all of it.
    create_dir_all(&mut sim, "/a/b/c/d").unwrap();
    create_dir_all(&mut sim, "/a/b/c/d").unwrap();
    assert!(sim.inspect(Metadata::new("/a/b/c/d")).unwrap().is_dir());
    write_file(&mut sim, "/a/file", b"x").unwrap();
    assert_eq!(
        kind(create_dir_all(&mut sim, "/a/file/e")),
        ErrorKind::NotADirectory
    );
    assert_eq!(
        kind(create_dir_all(&mut sim, "/a/file")),
        ErrorKind::NotADirectory
    );
}
