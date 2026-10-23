//! Runs the Tokio backend against the real OS.
#![cfg(feature = "full")]

use seam_core::io;
use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use seam_core::future::{SendInspect, SendPerform};
use seam_core::ops::fs::{
    CreateAtomic, CreateDir, NextEntry, Open, ReadDir, ReadFile, RemoveDir, WriteFile,
};
use seam_core::ops::fs::{OpenOptions, SetLen, Stat, SyncAll, SyncData};
use seam_core::ops::io::{ReadExact, ReadToEnd, WriteAll};
use seam_core::ops::io::{Seek, SeekFrom};
use seam_core::ops::net::{Accept, LocalAddr, ResolveInto, TcpBind, TcpConnect};
use seam_core::ops::time::{Now, Sleep};
use seam_core::{AsyncAcquire, AsyncProvide, Inspect};
use seam_tokio::Tokio;
use seam_tokio::fs::AtomicFile;
use seam_tokio::net::{TcpListener, TcpStream};

/// Edits a file in place through one handle: every file handle operation.
/// Generic, so it runs on any backend.
async fn edit_in_place<C, F>(c: &mut C, path: &std::path::Path) -> io::Result<()>
where
    C: for<'a> AsyncProvide<Open<'a, F>> + for<'a> SendInspect<ReadFile<'a>> + Send,
    F: SendPerform<Seek>
        + for<'a> SendPerform<WriteAll<'a>>
        + for<'a> SendPerform<ReadExact<'a>>
        + SendPerform<SetLen>
        + SendPerform<SyncAll>
        + SendPerform<SyncData>
        + SendInspect<Stat>
        + Send,
{
    let options = OpenOptions {
        read: true,
        write: true,
        create: true,
        truncate: true,
        ..OpenOptions::default()
    };
    c.acquire_async(Open::new(path, options), async |_, f| {
        f.perform_send(WriteAll(b"hello world")).await?;
        assert_eq!(f.perform_send(Seek(SeekFrom::Start(6))).await?, 6);
        let mut word = [0; 5];
        f.perform_send(ReadExact(&mut word)).await?;
        assert_eq!(&word, b"world");
        assert_eq!(f.perform_send(Seek(SeekFrom::End(-5))).await?, 6);
        f.perform_send(WriteAll(b"WORLD")).await?;
        assert_eq!(f.perform_send(Seek(SeekFrom::Current(-11))).await?, 0);
        f.perform_send(SetLen(5)).await?;
        assert_eq!(f.inspect_send(Stat).await?.len, 5);
        // Past the end: the gap fills with zeros.
        assert_eq!(f.perform_send(Seek(SeekFrom::End(2))).await?, 7);
        f.perform_send(WriteAll(b"!")).await?;
        f.perform_send(SyncData).await?;
        f.perform_send(SyncAll).await?;
        let err = f
            .perform_send(Seek(SeekFrom::Current(-100)))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let info = f.inspect_send(Stat).await?;
        assert!(info.is_file());
        assert_eq!(info.len, 8);
        io::Result::Ok(())
    })
    .await?;
    assert_eq!(c.inspect_send(ReadFile::new(path)).await?, b"hello\0\0!");
    // Truncating needs a handle open for writing.
    let err = c
        .acquire_async(Open::read(path), async |_, f| {
            f.perform_send(SetLen(0)).await
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    Ok(())
}

/// Lists `dir` through `ReadDir`, one entry at a time.
async fn list<C, D>(c: &mut C, dir: &Path) -> io::Result<Vec<PathBuf>>
where
    C: for<'a> AsyncProvide<ReadDir<'a, D>> + Send,
    D: SendPerform<NextEntry> + Send,
{
    c.acquire_async(ReadDir::new(dir), async |_, entries| {
        let mut paths = Vec::new();
        while let Some(path) = entries.perform_send(NextEntry).await? {
            paths.push(path);
        }
        Ok(paths)
    })
    .await
}

async fn scratch_dir(tokio: &mut Tokio, name: &str) -> io::Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!("seam-tokio-{name}-{}", std::process::id()));
    let _ = tokio.perform_send(RemoveDir::all(&dir)).await;
    tokio.perform_send(CreateDir::all(&dir)).await?;
    Ok(dir)
}

/// A core written only against capability bounds.
async fn copy_file<C, F>(c: &mut C, from: PathBuf, to: PathBuf) -> io::Result<()>
where
    C: for<'a> AsyncProvide<Open<'a, F>> + Send,
    F: for<'a> SendPerform<ReadToEnd<'a>> + for<'a> SendPerform<WriteAll<'a>> + Send,
{
    c.acquire_async(Open::read(&from), async |c, src| {
        let mut bytes = Vec::new();
        src.perform_send(ReadToEnd(&mut bytes)).await?;
        c.acquire_async(Open::create(&to), async |_, dst| {
            dst.perform_send(WriteAll(&bytes)).await
        })
        .await
    })
    .await
}

/// Generic cores that acquire resources produce `Send` futures, so they can
/// be spawned on a multithreaded runtime.
#[allow(dead_code)]
fn copy_file_is_send<C, F>(c: &mut C)
where
    C: for<'a> AsyncProvide<Open<'a, F>> + Send,
    F: for<'a> SendPerform<ReadToEnd<'a>> + for<'a> SendPerform<WriteAll<'a>> + Send,
{
    fn assert_send<T: Future + Send>(_: T) {}
    assert_send(copy_file(c, PathBuf::new(), PathBuf::new()));
}

#[tokio::test(flavor = "multi_thread")]
async fn spawned_core_on_multithreaded_runtime() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let dir = scratch_dir(&mut tokio, "fs").await?;
    let (a, b) = (dir.join("a"), dir.join("b"));
    tokio.perform_send(WriteFile::new(&a, "hello")).await?;

    let copied = tokio::spawn(async move {
        let mut tokio = Tokio::new();
        copy_file(&mut tokio, a, b.clone()).await?;
        tokio.inspect_send(ReadFile::new(&b)).await
    });
    assert_eq!(copied.await.unwrap()?, b"hello");
    tokio.perform_send(RemoveDir::all(&dir)).await
}

#[tokio::test]
async fn tcp_loopback() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let any_port = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    tokio
        .acquire_async(
            TcpBind::<TcpListener>::new(any_port),
            async |tokio, listener| {
                let port = listener.inspect(LocalAddr)?.port();
                let mut addr = [any_port];
                let found = tokio
                    .inspect_send(ResolveInto::new("127.0.0.1", port, &mut addr))
                    .await?;
                assert_eq!(found, 1);
                let [addr] = addr;
                // Accept and connect concurrently: the body holds both the
                // listener and the root capability.
                let server = listener.acquire_async(Accept::<TcpStream>::new(), async |_, s| {
                    // Waits for the whole buffer, across the client's writes.
                    let mut buf = [0; 4];
                    s.perform_send(ReadExact(&mut buf)).await?;
                    io::Result::Ok(buf.to_vec())
                });
                let client =
                    tokio.acquire_async(TcpConnect::<TcpStream>::new(addr), async |tokio, c| {
                        c.perform_send(WriteAll(b"pi")).await?;
                        tokio.perform_send(Sleep(Duration::from_millis(5))).await;
                        c.perform_send(WriteAll(b"ng")).await
                    });
                let (received, sent) = tokio::join!(server, client);
                sent?;
                assert_eq!(received?, b"ping");
                Ok(())
            },
        )
        .await
}

#[tokio::test]
async fn sync_ops_lift_and_time_advances() {
    let mut tokio = Tokio::new();
    let t0 = tokio.inspect_send(Now).await;
    tokio.perform_send(Sleep(Duration::from_millis(5))).await;
    assert!(tokio.inspect(Now) - t0 >= Duration::from_millis(5));
}

#[test]
fn block_on_runs_a_future() {
    let n = seam_tokio::block_on(async {
        let mut tokio = Tokio::new();
        tokio.perform_send(Sleep(Duration::from_millis(1))).await;
        7
    });
    assert_eq!(n, 7);
}

/// Waits for background cleanup, then returns what is in `dir`.
async fn settled_listing(dir: &Path, expect: &Path) -> Vec<PathBuf> {
    let mut listing = Vec::new();
    for _ in 0..200 {
        listing = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        if listing == [expect] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    listing
}

/// Cancels an atomic write after every possible number of polls, so at
/// every step of creating the temporary file, writing, syncing, renaming and
/// syncing the directory. Every time, the target holds exactly the old or
/// the new contents, and no temporary file is left behind.
#[tokio::test(flavor = "multi_thread")]
async fn atomic_write_is_safe_to_cancel_at_every_poll() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let dir = scratch_dir(&mut tokio, "atomic").await?;
    let path = dir.join("state.json");
    let mut cut_points = 0;

    for cut in 0.. {
        tokio.perform_send(WriteFile::new(&path, "old")).await?;
        let finished = {
            let write =
                tokio.acquire_async(CreateAtomic::<AtomicFile>::new(&path), async |_, f| {
                    f.perform_send(WriteAll(b"new ")).await?;
                    f.perform_send(WriteAll(b"contents")).await
                });
            let mut write = pin!(write);
            let mut polls = 0;
            poll_fn(|cx| {
                if polls == cut {
                    return Poll::Ready(false);
                }
                polls += 1;
                write.as_mut().poll(cx).map(|r| r.map(|()| true).unwrap())
            })
            .await
        }; // dropped here: cancelled, unless it finished

        let contents = std::fs::read(&path)?;
        assert!(
            contents == b"old" || contents == b"new contents",
            "cut {cut}: {contents:?}"
        );
        assert_eq!(
            settled_listing(&dir, &path).await,
            std::slice::from_ref(&path),
            "cut {cut}: leftover file"
        );
        if finished {
            assert_eq!(contents, b"new contents");
            break;
        }
        cut_points += 1;
    }
    assert!(cut_points >= 5, "only {cut_points} points to cancel at");
    tokio.perform_send(RemoveDir::all(&dir)).await
}

#[tokio::test]
async fn atomic_file_listing_is_clean_after_success() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let dir = scratch_dir(&mut tokio, "atomic-ok").await?;
    let path = dir.join("state.json");
    tokio
        .acquire_async(CreateAtomic::<AtomicFile>::new(&path), async |_, f| {
            f.perform_send(WriteAll(b"new")).await
        })
        .await?;
    assert_eq!(tokio.inspect_send(ReadFile::new(&path)).await?, b"new");
    assert_eq!(list(&mut tokio, &dir).await?, std::slice::from_ref(&path));
    tokio.perform_send(RemoveDir::all(&dir)).await
}

#[tokio::test]
async fn file_handles() -> io::Result<()> {
    let mut tokio = Tokio::new();
    let dir = scratch_dir(&mut tokio, "handles").await?;
    edit_in_place(&mut tokio, &dir.join("f")).await?;
    tokio.perform_send(RemoveDir::all(&dir)).await
}
