//! Cores written against seam bounds, run in the simulation.
#![cfg(feature = "full")]

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use seam_core::future::{SendPerform, block_on};
use seam_core::ops::env::{Args, Var};
use seam_core::ops::fs::{
    CreateDir, Metadata, Open, ReadDir, ReadFile, ReadToString, RemoveDir, RemoveFile, Rename,
    WriteFile,
};
use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{
    Accept, LocalAddr, PeerAddr, RecvFrom, Resolve, SendTo, Shutdown, TcpBind, TcpConnect, UdpBind,
};
use seam_core::ops::process::{Exit, Output, RunCommand};
use seam_core::ops::random::RandomU64;
use seam_core::ops::signal::CtrlC;
use seam_core::ops::stdio::{Print, ReadLine};
use seam_core::ops::time::{Now, Sleep, WallClock};
use seam_core::{Inspect, Perform, Scoped};
use seam_test::fs::File;
use seam_test::net::{TcpListener, TcpStream, UdpSocket};
use seam_test::{Invocation, Sim, catch_exit};

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
    C: Perform<Open<F>> + Inspect<Metadata> + Perform<Rename> + Perform<CreateDir>,
    F: Scoped + for<'a> Perform<WriteAll<'a>>,
{
    c.perform(CreateDir::all(dir))?;
    let log = dir.join("app.log");
    if c.inspect(Metadata::new(&log)).is_ok_and(|m| m.len >= max) {
        c.perform(Rename::new(&log, dir.join("app.log.1")))?;
    }
    c.perform(Open::<F>::append(&log))?
        .scope(|f| f.perform(WriteAll(line.as_bytes())))
}

#[test]
fn filesystem_core() {
    let mut sim = Sim::new();
    let dir = Path::new("/var/log");
    for line in ["one\n", "two\n", "three\n"] {
        append_log::<_, File>(&mut sim, dir, line, 8).unwrap();
    }
    assert_eq!(sim.file("/var/log/app.log.1").unwrap(), b"one\ntwo\n");
    assert_eq!(sim.file("/var/log/app.log").unwrap(), b"three\n");
    assert_eq!(
        sim.inspect(ReadDir::new("/var/log")).unwrap(),
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
        kind(sim.perform(Open::<File>::create("/a"))),
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

    // A handle survives its path being renamed, as on Unix.
    let mut f = sim.perform(Open::<File>::read("a.txt")).unwrap();
    sim.perform(Rename::new("a.txt", "b.txt")).unwrap();
    let mut out = Vec::new();
    f.perform(ReadToEnd(&mut out)).unwrap();
    assert_eq!(out, b"hi");
    assert_eq!(
        kind(f.perform(WriteAll(b"no"))),
        ErrorKind::PermissionDenied
    );
}

#[test]
fn tcp_in_memory() {
    let mut sim = Sim::new();
    let mut listener: TcpListener = sim
        .perform(TcpBind::new(SocketAddr::from((
            Ipv4Addr::UNSPECIFIED,
            8080,
        ))))
        .unwrap();
    assert_eq!(
        kind(sim.perform(TcpBind::<TcpListener>::new(([0, 0, 0, 0], 8080)))),
        ErrorKind::AddrInUse
    );
    assert_eq!(
        kind(listener.perform(Accept::<TcpStream>::new())),
        ErrorKind::WouldBlock
    );

    let addr = sim.inspect(Resolve::new("localhost", 8080)).unwrap()[0];
    let mut client: TcpStream = sim.perform(TcpConnect::new(addr)).unwrap();
    let (mut server, peer) = listener.perform(Accept::<TcpStream>::new()).unwrap();
    assert_eq!(peer, client.inspect(LocalAddr).unwrap());
    assert_eq!(client.inspect(PeerAddr).unwrap(), addr);

    let mut buf = [0; 8];
    assert_eq!(kind(server.perform(Read(&mut buf))), ErrorKind::WouldBlock);
    client.perform(WriteAll(b"ping")).unwrap();
    client.perform(Shutdown(std::net::Shutdown::Write)).unwrap();
    assert_eq!(server.perform(Read(&mut buf)).unwrap(), 4);
    assert_eq!(&buf[..4], b"ping");
    assert_eq!(
        server.perform(Read(&mut buf)).unwrap(),
        0,
        "end of stream after shutdown"
    );

    drop(server);
    assert_eq!(
        kind(
            client
                .perform(Shutdown(std::net::Shutdown::Read))
                .and_then(|()| { client.perform(WriteAll(b"x")) })
        ),
        ErrorKind::BrokenPipe
    );
    assert_eq!(
        kind(sim.perform(TcpConnect::<TcpStream>::new(([127, 0, 0, 1], 9)))),
        ErrorKind::ConnectionRefused
    );
}

#[test]
fn udp_in_memory() {
    let mut sim = Sim::new();
    sim.add_host("peer.test", Ipv4Addr::new(10, 0, 0, 2));
    let mut a: UdpSocket = sim.perform(UdpBind::new(([10, 0, 0, 1], 0))).unwrap();
    let mut b: UdpSocket = sim.perform(UdpBind::new(([10, 0, 0, 2], 53))).unwrap();
    let to = sim.inspect(Resolve::new("peer.test", 53)).unwrap()[0];

    a.perform(SendTo {
        buf: b"hello",
        addr: to,
    })
    .unwrap();
    let mut buf = [0; 3];
    let (n, from) = b.perform(RecvFrom(&mut buf)).unwrap();
    assert_eq!((n, &buf, from), (3, b"hel", a.inspect(LocalAddr).unwrap()));
    assert_eq!(kind(b.perform(RecvFrom(&mut buf))), ErrorKind::WouldBlock);
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
    assert_eq!(
        sim.invocations()[0],
        Invocation {
            program: "echo".into(),
            args: vec![OsString::from("hi"), OsString::from("there")],
        }
    );
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
    async fn retry<C: SendPerform<RandomU64> + SendPerform<Sleep> + SendPerform<WriteFile>>(
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
