//! Spawning tasks through seam on a real Tokio runtime.
#![cfg(feature = "full")]

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use seam_core::future::SendPerform;
use seam_core::ops::io::{Read, ReadToEnd, WriteAll};
use seam_core::ops::net::{Accept, LocalAddr, Shutdown, TcpBind, TcpConnect};
use seam_core::ops::task::{
    Abort, Detach, IsFinished, Join, JoinError, Spawn, SpawnAll, SpawnWith, YieldNow,
};
use seam_core::ops::time::Sleep;
use seam_core::{AsyncAcquire, AsyncProvide, Inspect};
use seam_tokio::Tokio;
use seam_tokio::net::{TcpListener, TcpStream};
use seam_tokio::task::JoinHandle;

/// A generic core: runs `job` on each input concurrently. The future type
/// is a parameter, inferred from `job`, so nothing is boxed.
async fn fan_out<C, J, Fut, H>(c: &mut C, inputs: Vec<u64>, job: J) -> Result<Vec<u64>, JoinError>
where
    C: AsyncProvide<SpawnAll<Fut, H>> + Send,
    J: Fn(u64) -> Fut + Send,
    Fut: Future<Output = u64> + Send,
    H: SendPerform<Join<u64>> + Send,
{
    let tasks = SpawnAll::new(inputs.into_iter().map(job).collect());
    c.acquire_async(tasks, async |_, handles| {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.perform_send(Join::new()).await?);
        }
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
    let out = fan_out(&mut tokio, inputs, |n| slow_square(Tokio::new(), n))
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

/// A connection reaches a task through `SpawnWith`: the listener accepts it
/// and the task owns it. When the task's body succeeds the stream is shut
/// down cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn spawn_with_hands_a_connection_to_a_task() -> io::Result<()> {
    let mut tokio = Tokio::new();
    tokio
        .acquire_async(
            TcpBind::<TcpListener>::new(([127, 0, 0, 1], 0)),
            async |tokio, listener| {
                let addr = listener.inspect(LocalAddr)?;
                let client =
                    tokio.acquire_async(TcpConnect::<TcpStream>::new(addr), async |_, c| {
                        c.perform_send(WriteAll(b"hello")).await?;
                        c.perform_send(Shutdown(std::net::Shutdown::Write)).await?;
                        let mut reply = Vec::new();
                        c.perform_send(ReadToEnd(&mut reply)).await?;
                        io::Result::Ok(reply)
                    });
                let echo = SpawnWith::<_, u64, io::Error, JoinHandle<io::Result<u64>>>::new(
                    Accept::<TcpStream>::new(),
                    |s| {
                        Box::pin(async move {
                            let mut buf = [0; 64];
                            let n = s.perform_send(Read(&mut buf)).await?;
                            s.perform_send(WriteAll(&buf[..n])).await?;
                            Ok(n as u64)
                        })
                    },
                );
                let server = listener.acquire_async(echo, async |_, task| {
                    task.perform_send(Join::new())
                        .await
                        .map_err(io::Error::from)
                });
                let (reply, echoed) = tokio::join!(client, server);
                assert_eq!(
                    reply?, b"hello",
                    "the task's stream was shut down, so the client saw end of input"
                );
                assert_eq!(echoed??, 5);
                Ok(())
            },
        )
        .await
}
