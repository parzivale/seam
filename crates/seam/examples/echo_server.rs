//! A TCP echo server on Tokio.
//!
//! ```text
//! cargo run -p seam-rs --example echo_server --features tokio,std,rt,rt-multi-thread,net,env,io-std,test -- 127.0.0.1:7878
//! ```
//!
//! The server spawns a task per connection, through `SpawnWith`,
//! on Tokio's multithreaded runtime, and detaches it. Each connection's
//! stream is used inside a scope, so it is shut down cleanly when the echo
//! succeeds. The per-connection logic is an async
//! core with `Send` bounds, and the same function is tested against the
//! in-memory network in `seam::test`, with no sockets and no runtime.
//!
//! `main` builds a root of two backends: Tokio for the network and stdio,
//! and std for the command-line arguments, which have nothing to do with
//! Tokio.

use std::io;
use std::net::SocketAddr;

use seam::ops::env::Args;
use seam::ops::io::{Read, WriteAll};
use seam::ops::net::{Accept, LocalAddr, PeerAddr, TcpBind};
use seam::ops::stdio::Print;
use seam::ops::task::{Detach, SpawnWith, TaskBody};
use seam::prelude::*;

/// Echoes everything it reads until the peer closes its end. Returns the
/// number of bytes echoed.
async fn echo<S>(stream: &mut S) -> io::Result<u64>
where
    S: for<'a> SendPerform<Read<'a>> + for<'a> SendPerform<WriteAll<'a>> + Send,
{
    let mut buf = [0; 1024];
    let mut total = 0;
    loop {
        let n = stream.perform_send(Read(&mut buf)).await?;
        if n == 0 {
            return Ok(total);
        }
        stream.perform_send(WriteAll(&buf[..n])).await?;
        total += n as u64;
    }
}

/// One connection: echo it, then log how it went. Runs inside the
/// connection's scope, so the stream is shut down cleanly if this returns
/// `Ok`, and just closed if it returns `Err` or the task is aborted.
async fn connection<C, S>(log: &mut C, stream: &mut S) -> io::Result<()>
where
    C: for<'a> SendPerform<Print<'a>> + Send,
    S: for<'a> SendPerform<Read<'a>> + for<'a> SendPerform<WriteAll<'a>> + Inspect<PeerAddr> + Send,
{
    let peer = stream.inspect(PeerAddr)?;
    let result = echo(stream).await;
    let line = match &result {
        Ok(n) => format!("{peer}: echoed {n} bytes\n"),
        Err(e) => format!("{peer}: {e}\n"),
    };
    log.perform_send(Print(line.as_bytes())).await?;
    result.map(drop)
}

/// The task that handles one connection: owns a clone of the capability,
/// for logging, and borrows the stream the listener accepted for it. A named
/// type, so that `serve`'s bounds can name it, and nothing is boxed.
struct Echo<C>(C);

impl<C, S> TaskBody<S> for Echo<C>
where
    C: for<'a> SendPerform<Print<'a>> + Send + 'static,
    S: for<'a> SendPerform<Read<'a>> + for<'a> SendPerform<WriteAll<'a>> + Inspect<PeerAddr> + Send,
{
    type Output = ();
    type Error = io::Error;

    async fn run(mut self, stream: &mut S) -> io::Result<()> {
        connection(&mut self.0, stream).await
    }
}

/// Binds `addr` and serves forever, each connection in its own task.
///
/// The listener exists only inside its scope, and a connection only inside
/// the task it is accepted into: `SpawnWith` has the listener accept the
/// connection and hand it to a new task running `Echo`, which owns it.
async fn serve<C, L, S, H>(c: &mut C, addr: SocketAddr) -> io::Result<()>
where
    C: AsyncProvide<TcpBind<L>> + for<'a> SendPerform<Print<'a>> + Clone + Send + 'static,
    L: AsyncProvide<SpawnWith<Accept<S>, Echo<C>, H>> + Inspect<LocalAddr> + Send,
    H: SendPerform<Detach> + Send,
{
    c.acquire_async(TcpBind::new(addr), async |c, listener| {
        let local = listener.inspect(LocalAddr)?;
        c.perform_send(Print(format!("listening on {local}\n").as_bytes()))
            .await?;
        loop {
            let task = SpawnWith::new(Accept::new(), Echo(c.clone()));
            // The handle's scope would wait for the task; detach it so the
            // server can accept the next connection meanwhile.
            listener
                .acquire_async(task, async |_, task| {
                    task.perform_send(Detach).await;
                    io::Result::Ok(())
                })
                .await?;
        }
    })
    .await
}

/// Reads the address to listen on from the arguments.
fn listen_addr<C: Inspect<Args>>(c: &C) -> io::Result<SocketAddr> {
    match c.inspect(Args).get(1) {
        None => Ok(SocketAddr::from(([127, 0, 0, 1], 7878))),
        Some(arg) => arg.to_str().and_then(|a| a.parse().ok()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: echo_server [addr:port]",
            )
        }),
    }
}

fn main() -> io::Result<()> {
    use seam::tokio::net::{TcpListener, TcpStream};
    use seam::tokio::task::JoinHandle;

    seam::tokio::block_on_multi_thread(|tokio| async move {
        let mut root = seam::root! {
            std: seam::stdlib::Std::new() => [seam::kinds::Env],
            tokio: tokio => [seam::kinds::Net, seam::kinds::Stdio],
        };
        let addr = listen_addr(&root)?;
        serve::<_, TcpListener, TcpStream, JoinHandle<io::Result<()>>>(&mut root, addr).await
    })
}

#[cfg(test)]
mod tests {
    use std::net::Shutdown as How;

    use seam::ops::io::ReadToEnd;
    use seam::ops::net::{Shutdown, TcpConnect};
    use seam::test::Sim;
    use seam::test::net::{TcpListener, TcpStream};

    use super::*;

    /// The connection handler runs in the simulation, with no sockets and
    /// no runtime.
    #[test]
    fn echoes_until_the_client_closes() -> io::Result<()> {
        let mut sim = Sim::new();
        let addr = SocketAddr::from(([127, 0, 0, 1], 7));
        sim.acquire(TcpBind::<TcpListener>::new(addr), |sim, listener| {
            sim.acquire(TcpConnect::<TcpStream>::new(addr), |sim, client| {
                client.perform(WriteAll(b"hello, "))?;
                client.perform(WriteAll(b"seam"))?;
                client.perform(Shutdown(How::Write))?;

                listener.acquire(Accept::<TcpStream>::new(), |_, server| {
                    block_on(connection(sim, server))
                })?;

                let mut reply = Vec::new();
                client.perform(ReadToEnd(&mut reply))?;
                assert_eq!(reply, b"hello, seam");
                // The scope closed the server's end cleanly: end of input,
                // not an error.
                assert_eq!(client.perform(Read(&mut [0; 8]))?, 0);
                io::Result::Ok(())
            })
        })?;
        let log = String::from_utf8(sim.take_stdout()).unwrap();
        assert!(log.ends_with(": echoed 11 bytes\n"), "{log}");
        Ok(())
    }

    /// The whole server, accept loop and spawned tasks included, with three
    /// clients connecting at once, on the deterministic runtime. Each seed
    /// schedules the tasks differently; every one must echo correctly.
    #[test]
    fn serves_concurrent_clients_under_every_schedule() {
        use seam::ops::task::{Detach, JoinNext, Spawn, SpawnInto, SpawnSet};
        use seam::ops::time::Sleep;
        use seam::test::rt::net::{TcpListener as RtListener, TcpStream as RtStream};
        use seam::test::rt::{AsyncSim, JoinHandle, Runtime, TaskSet};

        let addr = SocketAddr::from(([127, 0, 0, 1], 7878));
        for seed in 0..16 {
            let rt = Runtime::with_seed(Sim::new(), seed);
            rt.block_on(|mut sim| async move {
                let mut server = sim.clone();
                let serving = Spawn::<_, JoinHandle<io::Result<()>>>::new(async move {
                    serve::<AsyncSim, RtListener, RtStream, JoinHandle<io::Result<()>>>(
                        &mut server,
                        addr,
                    )
                    .await
                });
                sim.acquire_async(serving, async |sim, server| {
                    // Let the server bind before the clients connect.
                    sim.perform_send(Sleep(std::time::Duration::from_millis(1)))
                        .await;
                    let clients = SpawnSet::<TaskSet<io::Result<(String, Vec<u8>)>>>::new();
                    sim.acquire_async(clients, async |sim, clients| {
                        for name in ["alice", "bob", "carol"] {
                            let mut c = sim.clone();
                            let client = async move {
                                c.acquire_async(TcpConnect::<RtStream>::new(addr), async |_, s| {
                                    for part in [name, " says hi"] {
                                        s.perform_send(WriteAll(part.as_bytes())).await?;
                                    }
                                    s.perform_send(Shutdown(How::Write)).await?;
                                    let mut reply = Vec::new();
                                    s.perform_send(ReadToEnd(&mut reply)).await?;
                                    io::Result::Ok((name.to_owned(), reply))
                                })
                                .await
                            };
                            let _ = clients.perform_send(SpawnInto::new(client)).await;
                        }
                        while let Some(result) = clients.perform_send(JoinNext::new()).await {
                            let (name, reply) = result.map_err(io::Error::from)??;
                            assert_eq!(
                                reply,
                                format!("{name} says hi").into_bytes(),
                                "seed {seed}"
                            );
                        }
                        io::Result::Ok(())
                    })
                    .await?;
                    // The server runs forever; let the runtime drop it.
                    server.perform_send(Detach).await;
                    io::Result::Ok(())
                })
                .await
                .unwrap();
            });
            let log = String::from_utf8(rt.with_sim(|s| s.take_stdout())).unwrap();
            assert!(
                log.starts_with("listening on 127.0.0.1:7878\n"),
                "seed {seed}: {log}"
            );
            assert_eq!(log.matches("echoed").count(), 3, "seed {seed}: {log}");
        }
    }

    #[test]
    fn address_comes_from_the_arguments() {
        let mut sim = Sim::new();
        assert_eq!(listen_addr(&sim).unwrap().port(), 7878);
        sim.set_args(["echo_server", "0.0.0.0:9000"]);
        assert_eq!(
            listen_addr(&sim).unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 9000))
        );
        sim.set_args(["echo_server", "nope"]);
        assert!(listen_addr(&sim).is_err());
    }
}
