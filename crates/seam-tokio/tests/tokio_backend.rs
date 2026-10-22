//! Runs the Tokio backend against the real OS.
#![cfg(feature = "full")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use seam_core::Inspect;
use seam_core::future::{SendInspect, SendPerform};
use seam_core::ops::fs::{CreateDir, Open, ReadFile, RemoveDir, WriteFile};
use seam_core::ops::io::{Flush, Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, TcpBind, TcpConnect};
use seam_core::ops::time::{Now, Sleep};
use seam_tokio::Tokio;
use seam_tokio::fs::File;
use seam_tokio::net::{TcpListener, TcpStream};

/// A core written only against `Send` capability bounds, so it can be
/// spawned on a multithreaded runtime.
async fn copy_file<C, F>(
    c: &mut C,
    from: std::path::PathBuf,
    to: std::path::PathBuf,
) -> io::Result<()>
where
    C: SendPerform<Open<F>> + Send,
    F: for<'a> SendPerform<ReadToEnd<'a>>
        + for<'a> SendPerform<WriteAll<'a>>
        + SendPerform<Flush>
        + Send,
{
    let mut bytes = Vec::new();
    c.perform_send(Open::read(from))
        .await?
        .perform_send(ReadToEnd(&mut bytes))
        .await?;
    let mut out = c.perform_send(Open::create(to)).await?;
    out.perform_send(WriteAll(&bytes)).await?;
    out.perform_send(Flush).await
}

#[tokio::test(flavor = "multi_thread")]
async fn spawned_core_on_multithreaded_runtime() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let mut dir = std::env::temp_dir();
    dir.push(format!("seam-tokio-fs-{}", std::process::id()));
    tokio.perform_send(CreateDir::all(dir.clone())).await?;
    let (a, b) = (dir.join("a"), dir.join("b"));
    tokio.perform_send(WriteFile::new(&a, "hello")).await?;

    let handle = tokio::spawn(async move {
        let mut tokio = Tokio::new();
        copy_file::<_, File>(&mut tokio, a, b.clone()).await?;
        tokio.inspect_send(ReadFile::new(b)).await
    });
    assert_eq!(handle.await.unwrap()?, b"hello");

    tokio.perform_send(RemoveDir::all(dir)).await
}

#[tokio::test]
async fn tcp_loopback() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let mut listener: TcpListener = tokio
        .perform_send(TcpBind::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))))
        .await?;
    let addr = listener.inspect(LocalAddr)?;

    let accept = tokio::spawn(async move {
        let (mut server, _) = listener.perform_send(Accept::<TcpStream>::new()).await?;
        let mut buf = [0; 4];
        let n = server.perform_send(Read(&mut buf)).await?;
        io::Result::Ok(buf[..n].to_vec())
    });
    let mut client: TcpStream = tokio.perform_send(TcpConnect::new(addr)).await?;
    client.perform_send(WriteAll(b"ping")).await?;
    assert_eq!(accept.await.unwrap()?, b"ping");
    Ok(())
}

#[tokio::test]
async fn sync_ops_lift_and_time_advances() {
    let mut tokio = Tokio::new();
    let t0 = tokio.inspect_send(Now).await;
    tokio.perform_send(Sleep(Duration::from_millis(5))).await;
    assert!(tokio.inspect(Now) - t0 >= Duration::from_millis(5));
}

#[test]
fn block_on_hands_out_the_root() {
    let n = seam_tokio::block_on(|mut tokio| async move {
        tokio.perform_send(Sleep(Duration::from_millis(1))).await;
        7
    });
    assert_eq!(n, 7);
}
