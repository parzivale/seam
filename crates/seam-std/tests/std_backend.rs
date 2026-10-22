//! Runs the std backend against the real OS.
#![cfg(feature = "full")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use seam_core::ops::fs::{
    CreateAtomic, CreateDir, Open, ReadDir, ReadFile, RemoveDir, SweepAtomic, WriteFile,
};
use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, TcpBind, TcpConnect};
use seam_core::ops::random::FillRandom;
use seam_core::ops::time::Now;
use seam_core::{Acquire, Inspect, Perform, Provide};
use seam_std::Std;
use seam_std::fs::AtomicFile;

fn scratch_dir(std: &mut Std, name: &str) -> io::Result<PathBuf> {
    let mut dir = std.inspect(seam_core::ops::env::TempDir);
    dir.push(format!("seam-std-{name}-{}", std::process::id()));
    let _ = std.perform(RemoveDir::all(&dir));
    std.perform(CreateDir::all(&dir))?;
    Ok(dir)
}

/// A core written only against capability bounds: it can open files, and
/// read and write them, and nothing else.
fn copy_through_handle<C, F>(c: &mut C, from: &Path, to: &Path) -> io::Result<()>
where
    C: Provide<Open<F>>,
    F: for<'a> Perform<ReadToEnd<'a>> + for<'a> Perform<WriteAll<'a>>,
{
    c.acquire(Open::read(from), |c, src| {
        let mut bytes = Vec::new();
        src.perform(ReadToEnd(&mut bytes))?;
        c.acquire(Open::create(to), |_, dst| dst.perform(WriteAll(&bytes)))
    })
}

#[test]
fn filesystem_roundtrip() -> io::Result<()> {
    let mut std = Std::new();
    let dir = scratch_dir(&mut std, "fs")?;
    let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
    std.perform(WriteFile::new(&a, "hello"))?;
    copy_through_handle(&mut std, &a, &b)?;

    assert_eq!(std.inspect(ReadFile::new(&b))?, b"hello");
    let mut listed = std.inspect(ReadDir::new(&dir))?;
    listed.sort();
    assert_eq!(listed, [a, b]);
    std.perform(RemoveDir::all(&dir))
}

#[test]
fn tcp_loopback() -> io::Result<()> {
    let mut std = Std::new();
    let any_port = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    std.acquire(TcpBind::new(any_port), |std, listener| {
        let addr = listener.inspect(LocalAddr)?;
        std.acquire(TcpConnect::new(addr), |_, client| {
            client.perform(WriteAll(b"ping"))?;
            listener.acquire(Accept::new(), |_, server| {
                let mut buf = [0; 4];
                let n = server.perform(Read(&mut buf))?;
                assert_eq!(&buf[..n], b"ping");
                Ok(())
            })
        })
    })
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

mod atomic {
    use super::*;

    fn replace(std: &mut Std, path: &Path, fail: bool) -> io::Result<()> {
        std.acquire(CreateAtomic::<AtomicFile>::new(path), |_, f| {
            f.perform(WriteAll(b"new"))?;
            if fail {
                return Err(io::Error::other("failed halfway"));
            }
            Ok(())
        })
    }

    #[test]
    fn commits_on_ok_and_rolls_back_otherwise() -> io::Result<()> {
        let mut std = Std::new();
        let dir = scratch_dir(&mut std, "atomic")?;
        let path = dir.join("settings.conf");
        std.perform(WriteFile::new(&path, "old"))?;

        assert!(replace(&mut std, &path, true).is_err());
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"old");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut std = Std::new();
            std.acquire(
                CreateAtomic::<AtomicFile>::new(&path),
                |_, f| -> io::Result<()> {
                    f.perform(WriteAll(b"new"))?;
                    panic!("crash halfway")
                },
            )
        }));
        assert!(panicked.is_err());
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"old");
        assert_eq!(
            std.inspect(ReadDir::new(&dir))?,
            std::slice::from_ref(&path),
            "no temporary files left"
        );

        replace(&mut std, &path, false)?;
        assert_eq!(std.inspect(ReadFile::new(&path))?, b"new");
        assert_eq!(
            std.inspect(ReadDir::new(&dir))?,
            std::slice::from_ref(&path)
        );
        std.perform(RemoveDir::all(&dir))
    }

    /// A process killed mid-write leaves its temporary file. Sweeping
    /// removes ones old enough to be abandoned, and nothing else.
    #[test]
    fn sweeping_removes_leftovers() -> io::Result<()> {
        let mut std = Std::new();
        let dir = scratch_dir(&mut std, "sweep")?;
        let leftover = dir.join(".settings.conf.seam-0123456789abcdef.tmp");
        let unrelated = dir.join("notes.tmp");
        std.perform(WriteFile::new(&leftover, "half"))?;
        std.perform(WriteFile::new(&unrelated, "keep"))?;

        assert_eq!(
            std.perform(SweepAtomic::new(&dir, Duration::from_secs(3600)))?,
            0,
            "too recent"
        );
        assert_eq!(std.perform(SweepAtomic::new(&dir, Duration::ZERO))?, 1);
        assert_eq!(std.inspect(ReadDir::new(&dir))?, [unrelated]);
        std.perform(RemoveDir::all(&dir))
    }
}
