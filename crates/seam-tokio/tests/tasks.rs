//! Spawning tasks through seam on a real Tokio runtime.
#![cfg(feature = "full")]

use seam_core::io;
use seam_core::ops::io::{Write, read_to_end_send, write_all_send};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use seam_core::future::SendPerform;
use seam_core::ops::io::Read;
use seam_core::ops::net::{Accept, LocalAddr, Shutdown, TcpBind, TcpConnect};
use seam_core::ops::task::{
    Abort, Detach, IsFinished, Join, JoinError, JoinNext, Spawn, SpawnInto, SpawnSet, SpawnWith,
    TaskBody, YieldNow,
};
use seam_core::ops::time::Sleep;
use seam_core::{AsyncProvide, Inspect};
use seam_tokio::Tokio;
use seam_tokio::net::{TcpListener, TcpStream};
use seam_tokio::task::{JoinHandle, TaskSet};

/// A generic core: runs `job` on each input concurrently. The future type
/// is a parameter, inferred from `job`, so nothing is boxed, and the set's
/// storage is the backend's business.
async fn fan_out<C, S, J, Fut>(c: &mut C, inputs: Vec<u64>, job: J) -> Result<Vec<u64>, JoinError>
where
    C: AsyncProvide<SpawnSet<S>> + Send,
    S: SendPerform<SpawnInto<Fut>> + SendPerform<JoinNext<u64>> + Send,
    J: Fn(u64) -> Fut + Send,
    Fut: Future<Output = u64> + Send,
{
    c.acquire_async(SpawnSet::new(), async |_, set| {
        for input in inputs {
            if set.perform_send(SpawnInto::new(job(input))).await.is_err() {
                unreachable!("Tokio's sets are never full");
            }
        }
        let mut out = Vec::new();
        while let Some(result) = set.perform_send(JoinNext::new()).await {
            out.push(result?);
        }
        out.sort();
        Ok(out)
    })
    .await
}

/// The job gets its own copy of the root capability.
async fn slow_square(mut tokio: Tokio, n: u64) -> u64 {
    tokio.perform_send(Sleep(Duration::from_millis(50))).await;
    n * n
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tasks_run_concurrently() {
    let mut tokio = Tokio::new();
    let start = std::time::Instant::now();
    let inputs = (1..=8).collect();
    let out =
        fan_out::<_, TaskSet<u64>, _, _>(&mut tokio, inputs, |n| slow_square(Tokio::new(), n))
            .await
            .unwrap();
    assert_eq!(out, [1, 4, 9, 16, 25, 36, 49, 64]);
    // Eight 50 ms sleeps, overlapped.
    assert!(
        start.elapsed() < Duration::from_millis(300),
        "took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn boxed_async_blocks() {
    let mut tokio = Tokio::new();
    let mut worker = tokio.clone();
    let task = Spawn::<_, JoinHandle<u32>>::boxed(async move {
        for _ in 0..5 {
            worker.perform_send(YieldNow).await;
        }
        5
    });
    let out = tokio
        .acquire_async(task, async |_, h| {
            let n = h.perform_send(Join::new()).await?;
            assert!(h.inspect(IsFinished));
            Ok::<_, JoinError>(n)
        })
        .await;
    assert_eq!(out, Ok(5));
}

/// A task that sets a flag after 50 ms, unless it is aborted first.
fn flag_later() -> (
    Spawn<impl Future<Output = ()> + Send + 'static, JoinHandle<()>>,
    Arc<AtomicBool>,
) {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let task = Spawn::new(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        flag.store(true, Ordering::SeqCst);
    });
    (task, ran)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn a_successful_scope_waits_for_its_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later();
    tokio
        .acquire_async(task, async |_, _| Ok::<_, JoinError>(()))
        .await
        .unwrap();
    assert!(ran.load(Ordering::SeqCst), "closing joined the task");
}

#[tokio::test]
async fn a_failed_scope_aborts_its_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later();
    let out = tokio
        .acquire_async(task, async |_, _| Err::<(), _>(JoinError::Cancelled))
        .await;
    assert!(out.is_err());
    settle().await;
    assert!(!ran.load(Ordering::SeqCst));
}

/// Cancelling the code that holds a task, here with a timeout, aborts it.
#[tokio::test]
async fn a_cancelled_scope_aborts_its_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later();
    let scoped = tokio.acquire_async(task, async |_, _| {
        std::future::pending::<()>().await;
        Ok::<_, JoinError>(())
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(10), scoped)
            .await
            .is_err()
    );
    settle().await;
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn detached_tasks_outlive_their_scope() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later();
    let out = tokio
        .acquire_async(task, async |_, h| {
            h.perform_send(Detach).await;
            Err::<(), _>(JoinError::Cancelled) // even a failing scope leaves it be
        })
        .await;
    assert!(out.is_err());
    assert!(!ran.load(Ordering::SeqCst), "the scope did not wait");
    settle().await;
    assert!(ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn panics_and_aborts_are_reported() {
    let mut tokio = Tokio::new();
    let panicking = Spawn::<_, JoinHandle<()>>::new(async { panic!("boom") });
    let closed = tokio
        .acquire_async(panicking, async |_, _| Ok::<_, JoinError>(()))
        .await;
    assert_eq!(
        closed,
        Err(JoinError::Panicked),
        "closing reports how the task ended"
    );

    let stuck = Spawn::<_, JoinHandle<()>>::new(std::future::pending());
    let joined = tokio
        .acquire_async(stuck, async |_, h| {
            h.perform_send(Abort).await;
            Ok::<_, JoinError>(h.perform_send(Join::new()).await)
        })
        .await;
    assert_eq!(joined, Ok(Err(JoinError::Cancelled)));
}

/// A task set's scope aborts every task it still holds when it fails, and
/// waits for them all when it succeeds.
#[tokio::test]
async fn task_sets_wait_or_abort_together() {
    let mut tokio = Tokio::new();
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let late = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        flag.store(true, Ordering::SeqCst);
    };
    let out = tokio
        .acquire_async(SpawnSet::<TaskSet<()>>::new(), async |_, set| {
            let _ = set.perform_send(SpawnInto::new(late)).await;
            Err::<(), _>(JoinError::Cancelled)
        })
        .await;
    assert!(out.is_err());
    settle().await;
    assert!(
        !ran.load(Ordering::SeqCst),
        "the failed scope aborted the set"
    );
}

/// Echoes what it reads once, and reports how many bytes. A named body, so
/// nothing is boxed.
struct EchoOnce;

impl<S> TaskBody<S> for EchoOnce
where
    S: for<'a> SendPerform<Read<'a>> + for<'a> SendPerform<Write<'a>> + Send,
{
    type Output = u64;
    type Error = io::Error;

    async fn run(self, s: &mut S) -> io::Result<u64> {
        let mut buf = [0; 64];
        let n = s.perform_send(Read(&mut buf)).await?;
        write_all_send(s, &buf[..n]).await?;
        Ok(n as u64)
    }
}

/// Connects, sends `hello`, and returns the reply. Meanwhile the listener
/// hands the connection to a task running `echo`, and joins it.
async fn round_trip<B>(
    echo: SpawnWith<Accept<TcpStream>, B, JoinHandle<io::Result<u64>>>,
) -> io::Result<(Vec<u8>, u64)>
where
    B: TaskBody<TcpStream, Output = u64, Error = io::Error>,
{
    let mut tokio = Tokio::new();
    tokio
        .acquire_async(
            TcpBind::<TcpListener>::new(([127, 0, 0, 1], 0)),
            async |tokio, listener| {
                let addr = listener.inspect(LocalAddr)?;
                let client =
                    tokio.acquire_async(TcpConnect::<TcpStream>::new(addr), async |_, c| {
                        write_all_send(c, b"hello").await?;
                        c.perform_send(Shutdown(seam_core::ops::net::How::Write))
                            .await?;
                        let mut reply = Vec::new();
                        read_to_end_send(c, &mut reply).await?;
                        io::Result::Ok(reply)
                    });
                let server = listener.acquire_async(echo, async |_, task| {
                    task.perform_send(Join::new())
                        .await
                        .map_err(io::Error::from)
                });
                let (reply, echoed) = tokio::join!(client, server);
                Ok((reply?, echoed??))
            },
        )
        .await
}

/// A connection reaches a task through `SpawnWith`: the listener accepts it
/// and the task owns it. When the task's body succeeds the stream is shut
/// down cleanly, so the client sees end of input.
#[tokio::test(flavor = "multi_thread")]
async fn spawn_with_a_named_body() -> io::Result<()> {
    let (reply, echoed) = round_trip(SpawnWith::new(Accept::new(), EchoOnce)).await?;
    assert_eq!((reply.as_slice(), echoed), (&b"hello"[..], 5));
    Ok(())
}

/// The same with a boxed closure, for when a body type is not worth writing.
#[tokio::test(flavor = "multi_thread")]
async fn spawn_with_a_boxed_closure() -> io::Result<()> {
    let echo = SpawnWith::boxed(Accept::new(), |s: &mut TcpStream| {
        Box::pin(async move {
            let mut buf = [0; 64];
            let n = s.perform_send(Read(&mut buf)).await?;
            write_all_send(s, &buf[..n]).await?;
            Ok(n as u64)
        })
    });
    let (reply, echoed) = round_trip(echo).await?;
    assert_eq!((reply.as_slice(), echoed), (&b"hello"[..], 5));
    Ok(())
}
