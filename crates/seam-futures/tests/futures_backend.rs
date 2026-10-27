//! The futures-rs backend: under futures-rs's executors, Tokio and seam's
//! simulated runtime.
#![cfg(feature = "full")]

use core::time::Duration;
use std::net::SocketAddr;
use std::process::Command;

use futures::StreamExt;
use futures::executor::{LocalPool, ThreadPool, block_on};
use futures::io::{AsyncReadExt, AsyncWriteExt, Cursor};
use seam_core::effects as fx;
use seam_core::io;
use seam_core::ops::fs::ReadDir;
use seam_core::ops::io::{Seek, SeekFrom, read_to_end_async, write_all_async};
use seam_core::ops::net::{Accept, How, Shutdown, TcpBind, TcpConnect};
use seam_core::ops::process::Output;
use seam_core::ops::sync::{
    Channel, Load, RecvError, RecvMsg, SendMsg, Share, Update, try_recv, try_send,
};
use seam_core::ops::task::{
    Abort, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet,
};
use seam_core::ops::time::{Now, Timeout};
use seam_core::prelude::*;
use seam_futures::io::{AsyncReader, AsyncWriter, Compat};
use seam_futures::sync::{Receiver, Sender, Shared};
use seam_futures::task::{JoinHandle, TaskSet};
use seam_futures::{Executor, Futures};
use seam_test::Sim;
use seam_test::rt::Runtime;

type U32s = Channel<Sender<u32>, Receiver<u32>>;

/// A producer task and a consumer, through a bounded futures-rs channel,
/// on the simulated runtime: the producer waits for room each time.
#[test]
fn channels_on_the_simulated_runtime() {
    let sum = Runtime::new(Sim::new()).block_on(|mut sim| async move {
        let (mut tx, mut rx) = Futures.perform(U32s::bounded(1));
        let producer = async move {
            for i in 1..=10 {
                tx.perform_async(SendMsg(i)).await.unwrap();
            }
        };
        sim.acquire_async(
            Spawn::<_, seam_test::rt::JoinHandle<()>>::new(producer),
            async |_, _| {
                let all: Vec<u32> = seam_futures::stream::messages(&mut rx).collect().await;
                Ok::<_, JoinError>(all.iter().sum::<u32>())
            },
        )
        .await
    });
    assert_eq!(sum.unwrap(), 55);
}

#[test]
fn try_send_and_try_recv_are_a_timeout_of_zero() {
    let (mut tx, mut rx) = Futures.perform(U32s::bounded(1));
    assert_eq!(try_recv(&mut rx), Err(RecvError::Empty));
    try_send(&mut tx, 1).unwrap();
    assert!(matches!(
        try_send(&mut tx, 2),
        Err(seam_core::ops::sync::SendError::Full(2))
    ));
    assert_eq!(try_recv(&mut rx), Ok(1));
    drop(tx);
    assert_eq!(try_recv(&mut rx), Err(RecvError::Closed));
}

#[test]
#[should_panic(expected = "futures-rs has no clock")]
fn a_timeout_that_would_wait_panics() {
    let (_tx, mut rx) = Futures.perform(U32s::unbounded());
    let _ = rx.perform(Timeout::new(Duration::from_secs(1), RecvMsg::<u32>::new()));
}

#[test]
fn shared_cells() {
    block_on(async {
        let mut cell: Shared<Vec<u8>> = Futures.perform(Share::new(Vec::new()));
        let mut other = cell.clone();
        other
            .perform_async(Update::new(&mut |v: &mut Vec<u8>| v.push(1)))
            .await;
        let len = cell
            .perform_async(Update::new(&mut |v: &mut Vec<u8>| {
                v.push(2);
                v.len()
            }))
            .await;
        assert_eq!(len, 2);
        assert_eq!(cell.inspect_async(Load::new()).await, [1, 2]);
    });
}

/// `within` gives a futures-rs receive a time limit on the virtual clock.
#[test]
fn within_races_against_the_virtual_clock() {
    use seam_futures::time::within;

    let (waited, got) = Runtime::new(Sim::new()).block_on(|mut sim| async move {
        let (mut tx, mut rx) = Futures.perform(U32s::unbounded());
        let start = sim.inspect(Now);
        let none = within(
            &mut sim,
            Duration::from_secs(3),
            rx.perform_async(RecvMsg::new()),
        )
        .await;
        assert!(none.is_none());
        let waited = sim.inspect(Now).duration_since(start);
        tx.perform_async(SendMsg(9)).await.unwrap();
        let got = within(
            &mut sim,
            Duration::from_secs(3),
            rx.perform_async(RecvMsg::new()),
        )
        .await;
        (waited, got)
    });
    assert_eq!(waited, Duration::from_secs(3));
    assert_eq!(got, Some(Ok(9)));
}

#[test]
fn tasks_on_a_thread_pool() {
    let pool = ThreadPool::new().unwrap();
    let mut root = seam_core::root![Executor < fx::Spawn, fx::SpawnSet > (pool)];
    block_on(async {
        let doubled = root
            .acquire_async(
                Spawn::<_, JoinHandle<u32>>::new(async { 21 * 2 }),
                async |_, task| task.perform_async(Join::new()).await,
            )
            .await;
        assert_eq!(doubled, Ok(42));

        let panicked = root
            .acquire_async(
                Spawn::<_, JoinHandle<u32>>::new(async { panic!("boom") }),
                async |_, task| task.perform_async(Join::new()).await,
            )
            .await;
        assert_eq!(panicked, Err(JoinError::Panicked));

        let aborted = root
            .acquire_async(
                Spawn::<_, JoinHandle<()>>::new(futures::future::pending()),
                async |_, task| {
                    assert!(!task.inspect(IsFinished));
                    task.perform(Abort);
                    task.perform_async(Join::new()).await
                },
            )
            .await;
        assert_eq!(aborted, Err(JoinError::Cancelled));

        let mut outputs = root
            .acquire_async(SpawnSet::<TaskSet<u32>>::new(), async |_, set| {
                for i in 0..4 {
                    set.perform(SpawnInto::new(async move { i * 10 }))
                        .map_err(|_| JoinError::Cancelled)?;
                }
                let mut outputs = Vec::new();
                while let Some(output) = set.perform_async(JoinNext::new()).await {
                    outputs.push(output?);
                }
                Ok::<_, JoinError>(outputs)
            })
            .await
            .unwrap();
        outputs.sort();
        assert_eq!(outputs, [0, 10, 20, 30]);
    });
}

/// A `LocalPool`'s spawner runs tasks on the pool's own thread.
#[test]
fn tasks_on_a_local_pool() {
    let mut pool = LocalPool::new();
    let mut executor = Executor(pool.spawner());
    let got = pool.run_until(async move {
        executor
            .acquire_async(
                Spawn::<_, JoinHandle<&str>>::new(async { "local" }),
                async |_, task| task.perform_async(Join::new()).await,
            )
            .await
    });
    assert_eq!(got, Ok("local"));
}

#[test]
fn futures_io_as_seam_handles() -> io::Result<()> {
    block_on(async {
        let mut file = Compat(Cursor::new(Vec::new()));
        write_all_async(&mut file, b"hello, world").await?;
        file.perform_async(Seek(SeekFrom::Start(7))).await?;
        let mut tail = Vec::new();
        read_to_end_async(&mut file, &mut tail).await?;
        assert_eq!(tail, b"world");
        Ok(())
    })
}

#[test]
fn seam_handles_as_futures_io() -> io::Result<()> {
    block_on(async {
        let text = "a long line ".repeat(2000);
        let mut src = Compat(Cursor::new(text.clone().into_bytes()));
        let mut dst = Compat(Cursor::new(Vec::new()));
        let mut writer = AsyncWriter::new(&mut dst);
        futures::io::copy(AsyncReader::new(&mut src), &mut writer).await?;
        writer.close().await?;
        drop(writer);
        assert_eq!(dst.0.into_inner(), text.as_bytes());

        let mut small = Compat(Cursor::new(b"abcdef".to_vec()));
        let mut reader = AsyncReader::new(&mut small);
        let mut two = [0; 2];
        reader.read_exact(&mut two).await?;
        assert_eq!(&two, b"ab");
        let mut rest = String::new();
        reader.read_to_string(&mut rest).await?;
        assert_eq!(rest, "cdef");
        Ok(())
    })
}

#[test]
fn lines_and_entries_as_streams() -> io::Result<()> {
    let mut text = Compat(Cursor::new(b"one\ntwo\r\nthree".to_vec()));
    let lines: Vec<String> = block_on(
        seam_futures::stream::lines(&mut text)
            .map(Result::unwrap)
            .collect(),
    );
    assert_eq!(lines, ["one", "two", "three"]);

    let mut sim = Sim::new();
    sim.add_file("/d/a", "").add_file("/d/b", "");
    let mut paths = sim.acquire(ReadDir::new("/d"), |_, dir| {
        block_on(seam_futures::stream::entries(dir).collect::<Vec<_>>())
            .into_iter()
            .collect::<io::Result<Vec<_>>>()
    })?;
    paths.sort();
    assert_eq!(paths, [std::path::PathBuf::from("/d/a"), "/d/b".into()]);
    Ok(())
}

/// A proxy on the simulated runtime: the client talks to the backend
/// through `copy_bidirectional`.
#[test]
fn copy_bidirectional_proxies_a_connection() {
    use seam_test::rt::JoinHandle;
    use seam_test::rt::net::{TcpListener, TcpStream};

    let front = SocketAddr::from(([127, 0, 0, 1], 80));
    let back = SocketAddr::from(([127, 0, 0, 1], 81));
    let result = Runtime::new(Sim::new()).block_on(|mut sim| async move {
        sim.acquire_async(TcpBind::<TcpListener>::new(back), async |sim, backend| {
            sim.acquire_async(TcpBind::<TcpListener>::new(front), async |sim, proxy| {
                let mut client = sim.clone();
                let client = async move {
                    client
                        .acquire_async(TcpConnect::<TcpStream>::new(front), async |_, s| {
                            write_all_async(s, b"through the proxy").await?;
                            s.perform_async(Shutdown(How::Write)).await?;
                            let mut reply = Vec::new();
                            read_to_end_async(s, &mut reply).await?;
                            io::Result::Ok(reply)
                        })
                        .await
                };
                let server = async move {
                    backend
                        .acquire_async(Accept::<TcpStream>::new(), async |_, s| {
                            let mut got = Vec::new();
                            read_to_end_async(s, &mut got).await?;
                            write_all_async(s, &got.to_ascii_uppercase()).await
                        })
                        .await
                };
                let mut upstream_sim = sim.clone();
                let proxied = async move {
                    proxy
                        .acquire_async(Accept::<TcpStream>::new(), async |_, downstream| {
                            upstream_sim
                                .acquire_async(TcpConnect::<TcpStream>::new(back), async |_, up| {
                                    seam_futures::net::copy_bidirectional_async(downstream, up)
                                        .await
                                })
                                .await
                        })
                        .await
                };
                sim.acquire_async(
                    Spawn::<_, JoinHandle<io::Result<Vec<u8>>>>::new(client),
                    async |_, client| {
                        let (served, proxied) = futures::future::join(server, proxied).await;
                        served?;
                        let counts = proxied?;
                        let reply = client
                            .perform_async(Join::new())
                            .await
                            .map_err(io::Error::from)??;
                        io::Result::Ok((reply, counts))
                    },
                )
                .await
            })
            .await
        })
        .await
    });
    let (reply, counts) = result.unwrap();
    assert_eq!(reply, b"THROUGH THE PROXY");
    assert_eq!(counts, (17, 17));
}

fn shout() -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "tr a-z A-Z; echo done >&2; exit 3"]);
    cmd
}

#[cfg(unix)]
#[test]
fn pipe_through_on_tokio() -> io::Result<()> {
    let out = seam_tokio::block_on(async {
        seam_futures::process::pipe_through_send::<_, seam_tokio::process::Child, _, _, _>(
            &mut seam_tokio::Tokio,
            shout(),
            b"quiet",
        )
        .await
    })?;
    assert_eq!(
        out,
        Output {
            code: Some(3),
            stdout: b"QUIET".to_vec(),
            stderr: b"done\n".to_vec()
        }
    );
    Ok(())
}

#[test]
fn pipe_through_in_the_simulation() -> io::Result<()> {
    let mut sim = Sim::new();
    sim.on_command_with_stdin("sh", |_, input| {
        Ok(Output {
            code: Some(3),
            stdout: input.to_ascii_uppercase(),
            stderr: b"done\n".to_vec(),
        })
    });
    let out = block_on(seam_futures::process::pipe_through_async::<
        _,
        seam_test::process::Child,
        _,
        _,
        _,
    >(&mut sim, shout(), b"quiet"))?;
    assert_eq!(out.code, Some(3));
    assert_eq!(out.stdout, b"QUIET");
    assert_eq!(out.stderr, b"done\n");
    assert_eq!(sim.invocations()[0].stdin(), b"quiet");
    Ok(())
}
