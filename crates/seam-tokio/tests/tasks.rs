//! Spawning tasks through seam on a real Tokio runtime.
#![cfg(feature = "full")]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use seam_core::future::SendPerform;
use seam_core::ops::task::{
    Abort, BoxFuture, Detach, IsFinished, Join, JoinError, Spawn, YieldNow,
};
use seam_core::ops::time::Sleep;
use seam_core::{AsyncScoped, Inspect};
use seam_tokio::Tokio;
use seam_tokio::task::JoinHandle;

/// A generic core: runs `job` on each input concurrently. The future type
/// is a parameter, inferred from `job`, so nothing is boxed.
async fn fan_out<C, J, Fut, H>(c: &mut C, inputs: Vec<u64>, job: J) -> Result<Vec<u64>, JoinError>
where
    C: SendPerform<Spawn<Fut, H>> + Send,
    J: Fn(u64) -> Fut + Send,
    Fut: Future<Output = u64> + Send,
    H: SendPerform<Join<u64>> + Send,
{
    let mut handles = Vec::new();
    for input in inputs {
        handles.push(c.perform_send(Spawn::new(job(input))).await);
    }
    let mut out = Vec::new();
    for mut h in handles {
        out.push(h.perform_send(Join::new()).await?);
    }
    Ok(out)
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

/// With `BoxFuture`, one bound covers any task, `async` blocks included.
async fn spawn_counter<C, H>(c: &mut C, tokio: Tokio, ticks: u32) -> H
where
    C: SendPerform<Spawn<BoxFuture<u32>, H>>,
{
    c.perform_send(Spawn::boxed(async move {
        let mut tokio = tokio;
        for _ in 0..ticks {
            tokio.perform_send(YieldNow).await;
        }
        ticks
    }))
    .await
}

#[tokio::test]
async fn boxed_async_blocks() {
    let mut tokio = Tokio::new();
    let worker = tokio.clone();
    let mut h: JoinHandle<u32> = spawn_counter(&mut tokio, worker, 5).await;
    assert_eq!(h.perform_send(Join::new()).await, Ok(5));
    assert!(h.inspect(IsFinished));
}

#[tokio::test]
async fn panics_and_aborts_are_reported() {
    let mut tokio = Tokio::new();
    let mut panicked: JoinHandle<()> = tokio
        .perform_send(Spawn::new(async { panic!("boom") }))
        .await;
    assert_eq!(
        panicked.perform_send(Join::new()).await,
        Err(JoinError::Panicked)
    );

    let mut stuck: JoinHandle<()> = tokio.perform_send(Spawn::new(std::future::pending())).await;
    stuck.perform_send(Abort).await;
    assert_eq!(
        stuck.perform_send(Join::new()).await,
        Err(JoinError::Cancelled)
    );
}

/// A task that sets a flag after 50 ms, unless it is aborted first.
async fn flag_later(tokio: &mut Tokio) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let task = tokio
        .perform_send(Spawn::new(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
        }))
        .await;
    (task, ran)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn dropping_a_handle_aborts_its_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later(&mut tokio).await;
    drop(task);
    settle().await;
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn detached_tasks_outlive_their_handle() {
    let mut tokio = Tokio::new();
    let (mut task, ran) = flag_later(&mut tokio).await;
    task.perform_send(Detach).await;
    drop(task);
    settle().await;
    assert!(ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closing_a_scope_waits_for_the_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later(&mut tokio).await;
    task.scope_async(async |_| Ok::<_, JoinError>(()))
        .await
        .unwrap();
    assert!(ran.load(Ordering::SeqCst), "close joined the task");

    let panicking: JoinHandle<()> = tokio
        .perform_send(Spawn::new(async { panic!("boom") }))
        .await;
    let closed = panicking
        .scope_async(async |_| Ok::<_, JoinError>(()))
        .await;
    assert_eq!(closed, Err(JoinError::Panicked));
}

/// Cancelling the code that holds a task, here with a timeout, aborts it.
#[tokio::test]
async fn cancelling_a_scope_aborts_its_task() {
    let mut tokio = Tokio::new();
    let (task, ran) = flag_later(&mut tokio).await;
    let scoped = task.scope_async(async |_| {
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
