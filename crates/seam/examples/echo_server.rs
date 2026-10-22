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

use std::future::Future;
use std::io;
use std::net::SocketAddr;

use seam::ops::env::Args;
use seam::ops::io::{Read, WriteAll};
use seam::ops::net::{Accept, LocalAddr, TcpBind};
use seam::ops::stdio::Print;
use seam::ops::task::{Detach, Spawn};
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

/// Echoes one connection inside a scope: if the echo succeeds the stream is
/// closed cleanly (the peer sees end of input); if it fails, or the task is
/// aborted, the stream is just dropped.
async fn handle_stream<S>(stream: S) -> io::Result<u64>
where
    S: for<'a> SendPerform<Read<'a>>
        + for<'a> SendPerform<WriteAll<'a>>
        + AsyncFinalize<Error = io::Error>
        + Send,
{
    stream.scope_async(async |s| echo(s).await).await
}

/// One connection's task: echo, then log how it went. It owns a clone of
/// the root capability, since a spawned task cannot borrow the server's.
async fn connection<C, S>(mut c: C, stream: S, peer: SocketAddr)
where
    C: for<'a> SendPerform<Print<'a>> + Send,
    S: for<'a> SendPerform<Read<'a>>
        + for<'a> SendPerform<WriteAll<'a>>
        + AsyncFinalize<Error = io::Error>
        + Send,
{
    let line = match handle_stream(stream).await {
        Ok(n) => format!("{peer}: echoed {n} bytes\n"),
        Err(e) => format!("{peer}: {e}\n"),
    };
    let _ = c.perform_send(Print(line.as_bytes())).await;
}

/// Binds `addr` and spawns `handle` for every connection, forever.
///
/// `handle` is passed in rather than called by name so that its future's
/// type, `Fut`, is a type parameter: that is what lets the `Spawn` bound
/// name it without boxing.
async fn serve<C, L, S, F, Fut, H>(c: &mut C, addr: SocketAddr, handle: F) -> io::Result<()>
where
    C: SendPerform<TcpBind<L>>
        + SendPerform<Spawn<Fut, H>>
        + for<'a> SendPerform<Print<'a>>
        + Clone
        + Send,
    L: SendPerform<Accept<S>> + Inspect<LocalAddr> + Send,
    F: Fn(C, S, SocketAddr) -> Fut + Send,
    Fut: Future<Output = ()> + Send,
    H: SendPerform<Detach> + Send,
{
    let mut listener = c.perform_send(TcpBind::new(addr)).await?;
    let local = listener.inspect(LocalAddr)?;
    c.perform_send(Print(format!("listening on {local}\n").as_bytes()))
        .await?;
    loop {
        let (stream, peer) = listener.perform_send(Accept::new()).await?;
        let mut task = c
            .perform_send(Spawn::new(handle(c.clone(), stream, peer)))
            .await;
        // Dropping a handle aborts its task, so let this one run on.
        task.perform_send(Detach).await;
    }
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
    use seam::tokio::Tokio;
    use seam::tokio::net::TcpStream;

    seam::tokio::block_on_multi_thread(|mut tokio| async move {
        let addr = listen_addr(&tokio)?;
        serve(&mut tokio, addr, connection::<Tokio, TcpStream>).await
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

    #[test]
    fn echoes_until_the_client_closes() {
        let mut sim = Sim::new();
        let mut listener: TcpListener = sim.perform(TcpBind::new(([127, 0, 0, 1], 7))).unwrap();
        let mut client: TcpStream = sim.perform(TcpConnect::new(([127, 0, 0, 1], 7))).unwrap();
        client.perform(WriteAll(b"hello, ")).unwrap();
        client.perform(WriteAll(b"seam")).unwrap();
        client.perform(Shutdown(How::Write)).unwrap();

        let (server, _) = listener.perform(Accept::<TcpStream>::new()).unwrap();
        assert_eq!(block_on(handle_stream(server)).unwrap(), 11);

        let mut reply = Vec::new();
        client.perform(ReadToEnd(&mut reply)).unwrap();
        assert_eq!(reply, b"hello, seam");
        // The scope closed the server's end cleanly: end of input, not an
        // error.
        assert_eq!(client.perform(seam::ops::io::Read(&mut [0; 8])).unwrap(), 0);
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
