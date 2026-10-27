//! Split streams and children, on std, Tokio and the simulation.
#![cfg(all(
    feature = "std",
    feature = "tokio",
    feature = "test",
    feature = "full",
    unix
))]

use std::process::Command;

use seam::io;
use seam::ops::io::{
    Flush, Halves, Read, Split, Write, read_to_end, read_to_end_async, write_all, write_all_async,
};
use seam::ops::net::{Accept, How, PeerAddr, Shutdown, TcpBind, TcpConnect};
use seam::ops::process::{ChildPipes, CloseStdin, Output, SpawnChild, SplitChild, Wait};
use seam::prelude::*;

/// Sends `request` through the write half while the read half collects the
/// reply, on two scoped threads.
fn exchange<S, R, W>(stream: &mut S, request: &[u8]) -> io::Result<Vec<u8>>
where
    S: Provide<Split<R, W>>,
    R: for<'a> Perform<Read<'a>> + Send,
    W: for<'a> Perform<Write<'a>> + Perform<Shutdown> + Send,
{
    stream.acquire(Split::new(), |_, Halves { read, write }| {
        std::thread::scope(|scope| {
            let sender = scope.spawn(|| {
                write_all(write, request)?;
                write.perform(Shutdown(How::Write))
            });
            let mut reply = Vec::new();
            read_to_end(read, &mut reply)?;
            sender.join().expect("the sender panicked")?;
            Ok(reply)
        })
    })
}

/// The same, async: the two halves run joined in one task.
async fn exchange_async<S, R, W>(stream: &mut S, request: &[u8]) -> io::Result<Vec<u8>>
where
    S: AsyncProvide<Split<R, W>>,
    R: for<'a> AsyncPerform<Read<'a>>,
    W: for<'a> AsyncPerform<Write<'a>> + AsyncPerform<Shutdown>,
{
    stream
        .acquire_async(Split::new(), async |_, Halves { read, write }| {
            let send = async {
                write_all_async(write, request).await?;
                write.perform_async(Shutdown(How::Write)).await
            };
            let receive = async {
                let mut reply = Vec::new();
                read_to_end_async(read, &mut reply).await.map(|_| reply)
            };
            let (sent, reply) = tokio::join!(send, receive);
            sent?;
            reply
        })
        .await
}

#[test]
fn split_std_stream_on_two_threads() -> io::Result<()> {
    use seam::stdlib::Std;
    use seam::stdlib::net::{TcpListener, TcpStream};

    let mut std = Std::new();
    std.acquire(
        TcpBind::<TcpListener>::new(([127, 0, 0, 1], 0)),
        |std, listener| {
            let addr = listener.inspect(seam::ops::net::LocalAddr)?;
            std::thread::scope(|scope| {
                // An echo server, reading until the client shuts down writing.
                scope.spawn(|| {
                    listener.acquire(Accept::<TcpStream>::new(), |_, s| {
                        let mut got = Vec::new();
                        read_to_end(s, &mut got)?;
                        write_all(s, &got.to_ascii_uppercase())?;
                        s.perform(Flush)
                    })
                });
                let reply = std.acquire(TcpConnect::<TcpStream>::new(addr), |_, stream| {
                    let reply = exchange(stream, b"split me")?;
                    // The stream is whole again after the split scope. (Its
                    // peer has closed, so it has no `PeerAddr` any more.)
                    assert!(stream.inspect(seam::ops::net::LocalAddr).is_ok());
                    io::Result::Ok(reply)
                })?;
                assert_eq!(reply, b"SPLIT ME");
                Ok(())
            })
        },
    )
}

#[test]
fn split_tokio_stream_joined_in_one_task() -> io::Result<()> {
    use seam::tokio::Tokio;
    use seam::tokio::net::TcpStream;

    seam::tokio::block_on(async {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut s, _) = listener.accept().await?;
            let mut got = Vec::new();
            s.read_to_end(&mut got).await?;
            s.write_all(&got.to_ascii_uppercase()).await?;
            std::io::Result::Ok(())
        });
        let reply = Tokio
            .acquire_async(TcpConnect::<TcpStream>::new(addr), async |_, stream| {
                let reply = exchange_async(stream, b"split me").await?;
                // Reunited: a stream still in halves fails with
                // `NotConnected`.
                assert!(stream.inspect(seam::ops::net::LocalAddr).is_ok());
                io::Result::Ok(reply)
            })
            .await?;
        server.await.expect("the server panicked")?;
        assert_eq!(reply, b"SPLIT ME");
        Ok(())
    })
}

#[test]
fn split_stream_on_the_simulated_runtime() -> io::Result<()> {
    use seam::ops::task::{Join, Spawn};
    use seam::test::rt::net::{TcpListener, TcpStream};
    use seam::test::rt::{JoinHandle, Runtime};

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 80));
    let reply = Runtime::new(seam::test::Sim::new()).block_on(|mut sim| async move {
        sim.acquire_async(TcpBind::<TcpListener>::new(addr), async |sim, listener| {
            let mut client = sim.clone();
            let client = async move {
                client
                    .acquire_async(TcpConnect::<TcpStream>::new(addr), async |_, stream| {
                        exchange_async(stream, b"split me").await
                    })
                    .await
            };
            sim.acquire_async(
                Spawn::<_, JoinHandle<io::Result<Vec<u8>>>>::new(client),
                async |_, client| {
                    listener
                        .acquire_async(Accept::<TcpStream>::new(), async |_, s| {
                            let mut got = Vec::new();
                            read_to_end_async(s, &mut got).await?;
                            write_all_async(s, &got.to_ascii_uppercase()).await
                        })
                        .await?;
                    client
                        .perform_async(Join::new())
                        .await
                        .map_err(io::Error::from)?
                },
            )
            .await
        })
        .await
    })?;
    assert_eq!(reply, b"SPLIT ME");
    Ok(())
}

/// In the plain simulation nothing waits, so the halves take turns.
#[test]
fn split_stream_in_the_plain_simulation() -> io::Result<()> {
    use seam::test::Sim;
    use seam::test::net::{TcpListener, TcpStream};

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 80));
    let mut sim = Sim::new();
    sim.acquire(TcpBind::<TcpListener>::new(addr), |sim, listener| {
        sim.acquire(TcpConnect::<TcpStream>::new(addr), |_, client| {
            client.acquire(Split::new(), |_, Halves { read, write }| {
                write_all(write, b"ping")?;
                write.perform(Shutdown(How::Write))?;
                listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                    let mut got = Vec::new();
                    read_to_end(server, &mut got)?;
                    write_all(server, &got.to_ascii_uppercase())
                })?;
                let mut reply = Vec::new();
                read_to_end(read, &mut reply)?;
                assert_eq!(reply, b"PING");
                assert_eq!(read.inspect(PeerAddr)?, addr);
                Ok(())
            })
        })
    })
}

/// Feeds a child while draining both of its outputs.
async fn filter_both<C, P, I, O, E>(
    c: &mut C,
    cmd: Command,
    input: &[u8],
) -> io::Result<(Vec<u8>, Vec<u8>)>
where
    C: AsyncProvide<SpawnChild<P>>,
    P: AsyncProvide<SplitChild<I, O, E>> + AsyncPerform<Wait>,
    I: for<'a> AsyncPerform<Write<'a>> + Perform<CloseStdin>,
    O: for<'a> AsyncPerform<Read<'a>>,
    E: for<'a> AsyncPerform<Read<'a>>,
{
    let spawn = SpawnChild::new(cmd).stderr(seam::ops::process::Pipe::Piped);
    c.acquire_async(spawn, async |_, child| {
        let (out, err) = child
            .acquire_async(
                SplitChild::new(),
                async |_,
                       ChildPipes {
                           stdin,
                           stdout,
                           stderr,
                       }| {
                    let feed = async {
                        write_all_async(stdin, input).await?;
                        stdin.perform(CloseStdin);
                        io::Result::Ok(())
                    };
                    let mut out = Vec::new();
                    let mut err = Vec::new();
                    let (fed, read_out, read_err) = tokio::join!(
                        feed,
                        read_to_end_async(stdout, &mut out),
                        read_to_end_async(stderr, &mut err),
                    );
                    fed?;
                    read_out?;
                    read_err?;
                    io::Result::Ok((out, err))
                },
            )
            .await?;
        let status = child.perform_async(Wait).await?;
        if !status.success() {
            return Err(io::Error::other("the filter failed"));
        }
        Ok((out, err))
    })
    .await
}

fn shout() -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "tr a-z A-Z; echo done >&2"]);
    cmd
}

#[test]
fn split_child_on_tokio() -> io::Result<()> {
    use seam::tokio::Tokio;
    use seam::tokio::process::Child;

    let (out, err) = seam::tokio::block_on(async {
        filter_both::<_, Child, _, _, _>(&mut Tokio, shout(), b"quiet").await
    })?;
    assert_eq!(out, b"QUIET");
    assert_eq!(err, b"done\n");
    Ok(())
}

#[test]
fn split_child_on_std_with_threads() -> io::Result<()> {
    use seam::stdlib::Std;
    use seam::stdlib::process::Child;

    let spawn = SpawnChild::<Child>::new(shout()).stderr(seam::ops::process::Pipe::Piped);
    let (out, err) = Std::new().acquire(spawn, |_, child| {
        let pipes = child.acquire(
            SplitChild::new(),
            |_,
             ChildPipes {
                 stdin,
                 stdout,
                 stderr,
             }| {
                std::thread::scope(|scope| {
                    let feed = scope.spawn(|| {
                        write_all(stdin, b"quiet")?;
                        stdin.perform(CloseStdin);
                        io::Result::Ok(())
                    });
                    let drain_err = scope.spawn(|| {
                        let mut err = Vec::new();
                        read_to_end(stderr, &mut err).map(|_| err)
                    });
                    let mut out = Vec::new();
                    read_to_end(stdout, &mut out)?;
                    feed.join().expect("the feeder panicked")?;
                    let err = drain_err.join().expect("the reader panicked")?;
                    io::Result::Ok((out, err))
                })
            },
        )?;
        assert!(child.perform(Wait)?.success());
        io::Result::Ok(pipes)
    })?;
    assert_eq!(out, b"QUIET");
    assert_eq!(err, b"done\n");
    Ok(())
}

#[test]
fn split_child_in_the_simulation() -> io::Result<()> {
    use seam::test::Sim;
    use seam::test::process::Child;

    let mut sim = Sim::new();
    sim.on_command_with_stdin("sh", |_, input| {
        Ok(Output {
            code: Some(0),
            stdout: input.to_ascii_uppercase(),
            stderr: b"done\n".to_vec(),
        })
    });
    let spawn = SpawnChild::<Child>::new(shout()).stderr(seam::ops::process::Pipe::Piped);
    let (out, err) = sim.acquire(spawn, |_, child| {
        child.acquire(
            SplitChild::new(),
            |_,
             ChildPipes {
                 stdin,
                 stdout,
                 stderr,
             }| {
                // The simulated child runs when its output is first read, on
                // the input written by then.
                write_all(stdin, b"quiet")?;
                stdin.perform(CloseStdin);
                let (mut out, mut err) = (Vec::new(), Vec::new());
                read_to_end(stdout, &mut out)?;
                read_to_end(stderr, &mut err)?;
                io::Result::Ok((out, err))
            },
        )
    })?;
    assert_eq!(out, b"QUIET");
    assert_eq!(err, b"done\n");
    assert_eq!(sim.invocations()[0].stdin(), b"quiet");
    Ok(())
}

/// A scope that fails drops the halves. With std the stream lives on.
#[test]
fn a_failed_split_leaves_a_std_stream_usable() -> io::Result<()> {
    use seam::stdlib::Std;
    use seam::stdlib::net::{TcpListener, TcpStream};

    let mut std = Std::new();
    std.acquire(
        TcpBind::<TcpListener>::new(([127, 0, 0, 1], 0)),
        |std, listener| {
            let addr = listener.inspect(seam::ops::net::LocalAddr)?;
            std.acquire(TcpConnect::<TcpStream>::new(addr), |_, client| {
                let failed: io::Result<()> =
                    client.acquire(Split::new(), |_, _| Err(io::Error::other("give up")));
                assert!(failed.is_err());
                write_all(client, b"still here")?;
                client.perform(Shutdown(How::Write))?;
                listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                    let mut got = Vec::new();
                    read_to_end(server, &mut got)?;
                    assert_eq!(got, b"still here");
                    Ok(())
                })
            })
        },
    )
}
