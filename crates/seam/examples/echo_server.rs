//! A TCP echo server on Tokio.
//!
//! ```text
//! cargo run -p seam --example echo_server --features tokio,rt,rt-multi-thread,net,env,io-std,test -- 127.0.0.1:7878
//! ```
//!
//! The server spawns a task per connection, through the `Spawn` operation,
//! on Tokio's multithreaded runtime, and detaches it. Each connection's
//! stream is used inside a scope, so it is shut down cleanly when the echo
//! succeeds. The per-connection logic is an async
//! core with `Send` bounds, and the same function is tested against the
//! in-memory network in `seam::test`, with no sockets and no runtime.

use std::io;
use std::net::SocketAddr;

use seam::ops::env::Args;
use seam::ops::io::{Read, WriteAll};
use seam::ops::net::{Accept, LocalAddr, PeerAddr, TcpBind};
use seam::ops::stdio::Print;
use seam::ops::task::{Detach, SpawnWith};
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

/// Binds `addr` and serves forever, each connection in its own task.
///
/// The listener exists only inside its scope, and a connection only inside
/// the task it is accepted into: `SpawnWith` has the listener accept the
/// connection and hand it to a new task, which owns it. Each task gets its
/// own clone of the capability, for logging.
async fn serve<C, L, S, H>(c: &mut C, addr: SocketAddr) -> io::Result<()>
where
    C: AsyncProvide<TcpBind<L>> + for<'a> SendPerform<Print<'a>> + Clone + Send + 'static,
    L: AsyncProvide<SpawnWith<Accept<S>, (), io::Error, H>> + Inspect<LocalAddr> + Send,
    S: for<'a> SendPerform<Read<'a>>
        + for<'a> SendPerform<WriteAll<'a>>
        + Inspect<PeerAddr>
        + Send
        + 'static,
    H: SendPerform<Detach> + Send,
{
    c.acquire_async(TcpBind::new(addr), async |c, listener| {
        let local = listener.inspect(LocalAddr)?;
        c.perform_send(Print(format!("listening on {local}\n").as_bytes()))
            .await?;
        loop {
            let mut log = c.clone();
            let task = SpawnWith::<_, (), io::Error, H>::new(Accept::<S>::new(), move |stream| {
                Box::pin(async move { connection(&mut log, stream).await })
            });
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

    seam::tokio::block_on_multi_thread(|mut tokio| async move {
        let addr = listen_addr(&tokio)?;
        serve::<_, TcpListener, TcpStream, JoinHandle<io::Result<()>>>(&mut tokio, addr).await
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
