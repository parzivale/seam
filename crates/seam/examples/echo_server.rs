//! A TCP echo server on Tokio.
//!
//! ```text
//! cargo run -p seam --example echo_server --features tokio,rt,net,env,io-std,test -- 127.0.0.1:7878
//! ```
//!
//! The per-connection logic is an async core with `Send` bounds, so it can
//! run on Tokio's multithreaded runtime, and the same function is tested
//! against the in-memory network in `seam::test`, with no sockets and no
//! runtime.
//!
//! seam has no operation for spawning tasks yet, so the server handles one
//! connection at a time.

use std::io;
use std::net::SocketAddr;

use seam::ops::env::Args;
use seam::ops::io::{Read, WriteAll};
use seam::ops::net::{Accept, LocalAddr, PeerAddr, TcpBind};
use seam::ops::stdio::Print;
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

/// Binds `addr` and echoes connections, one at a time, forever.
async fn serve<C, L, S>(c: &mut C, addr: SocketAddr) -> io::Result<()>
where
    C: SendPerform<TcpBind<L>> + for<'a> SendPerform<Print<'a>> + Send,
    L: SendPerform<Accept<S>> + Inspect<LocalAddr> + Send,
    S: for<'a> SendPerform<Read<'a>> + for<'a> SendPerform<WriteAll<'a>> + Inspect<PeerAddr> + Send,
{
    let mut listener = c.perform_send(TcpBind::new(addr)).await?;
    let local = listener.inspect(LocalAddr)?;
    c.perform_send(Print(format!("listening on {local}\n").as_bytes()))
        .await?;
    loop {
        let (mut stream, peer) = listener.perform_send(Accept::new()).await?;
        let line = match echo(&mut stream).await {
            Ok(n) => format!("{peer}: echoed {n} bytes\n"),
            Err(e) => format!("{peer}: {e}\n"),
        };
        c.perform_send(Print(line.as_bytes())).await?;
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
    seam::tokio::block_on(|mut tokio| async move {
        let addr = listen_addr(&tokio)?;
        serve::<_, _, seam::tokio::net::TcpStream>(&mut tokio, addr).await
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

        let (mut server, _) = listener.perform(Accept::<TcpStream>::new()).unwrap();
        assert_eq!(block_on(echo(&mut server)).unwrap(), 11);

        let mut reply = Vec::new();
        client.perform(ReadToEnd(&mut reply)).unwrap();
        assert_eq!(reply, b"hello, seam");
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
