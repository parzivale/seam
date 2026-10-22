//! Runs the std backend against the real OS.
#![cfg(feature = "full")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use seam_core::ops::fs::{CreateDir, Open, ReadDir, ReadFile, RemoveDir, WriteFile};
use seam_core::ops::io::{Flush, Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, TcpBind, TcpConnect};
use seam_core::ops::random::FillRandom;
use seam_core::ops::time::Now;
use seam_core::{Inspect, Perform, Scoped};
use seam_std::Std;
use seam_std::fs::File;
use seam_std::net::{TcpListener, TcpStream};

fn scratch_dir(std: &Std, name: &str) -> std::path::PathBuf {
    let mut dir = std.inspect(seam_core::ops::env::TempDir);
    dir.push(format!("seam-std-{name}-{}", std::process::id()));
    dir
}

/// A core written only against capability bounds.
fn copy_through_handle<C, F>(
    c: &mut C,
    from: &std::path::Path,
    to: &std::path::Path,
) -> io::Result<()>
where
    C: Perform<Open<F>>,
    F: Scoped + for<'a> Perform<ReadToEnd<'a>> + for<'a> Perform<WriteAll<'a>> + Perform<Flush>,
{
    let mut bytes = Vec::new();
    c.perform(Open::read(from))?
        .scope(|f| f.perform(ReadToEnd(&mut bytes)))?;
    c.perform(Open::create(to))?.scope(|f| {
        f.perform(WriteAll(&bytes))?;
        f.perform(Flush)
    })
}

#[test]
fn filesystem_roundtrip() -> io::Result<()> {
    let mut std = Std::new();
    let dir = scratch_dir(&std, "fs");
    std.perform(CreateDir::all(&dir))?;

    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    std.perform(WriteFile::new(&a, "hello"))?;
    copy_through_handle::<_, File>(&mut std, &a, &b)?;

    assert_eq!(std.inspect(ReadFile::new(&b))?, b"hello");
    let mut listed = std.inspect(ReadDir::new(&dir))?;
    listed.sort();
    assert_eq!(listed, [a, b]);

    std.perform(RemoveDir::all(&dir))
}

#[test]
fn tcp_loopback() -> io::Result<()> {
    let mut std = Std::new();
    let mut listener: TcpListener =
        std.perform(TcpBind::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))))?;
    let addr = listener.inspect(LocalAddr)?;

    let mut client: TcpStream = std.perform(TcpConnect::new(addr))?;
    let (mut server, _) = listener.perform(Accept::<TcpStream>::new())?;

    client.perform(WriteAll(b"ping"))?;
    let mut buf = [0; 4];
    let n = server.perform(Read(&mut buf))?;
    assert_eq!(&buf[..n], b"ping");
    Ok(())
}

#[test]
fn clocks_and_randomness() {
    let mut std = Std::new();
    let t0 = std.inspect(Now);
    let mut a = [0u8; 13];
    let mut b = [0u8; 13];
    std.perform(FillRandom(&mut a));
    std.perform(FillRandom(&mut b));
    assert_ne!(a, b);
    assert!(std.inspect(Now) >= t0);
}
