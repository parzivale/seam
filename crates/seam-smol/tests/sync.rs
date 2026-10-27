//! Channels and shared cells on smol.
#![cfg(feature = "full")]

use std::time::Duration;

use seam_core::future::{SendInspect, SendPerform};
use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendMsg, Share, Update};
use seam_core::ops::task::{JoinError, JoinNext, SpawnInto, SpawnSet};
use seam_core::{AsyncProvide, Perform};
use seam_smol::Smol;
use seam_smol::sync::{Receiver, Sender, Shared};
use seam_smol::task::TaskSet;

/// Tokio's `timeout`, on smol's timer.
#[allow(dead_code)]
async fn timeout<T>(
    d: core::time::Duration,
    f: impl core::future::Future<Output = T>,
) -> Result<T, ()> {
    smol::future::or(async { Ok(f.await) }, async {
        smol::Timer::after(d).await;
        Err(())
    })
    .await
}

/// Producers in tasks, a consumer in the test; a bounded channel makes the
/// producers wait for the consumer.
#[test]
fn producers_and_a_consumer() {
    seam_smol::block_on(async move {
        let mut smol = Smol::new();
        let (tx, mut rx): (Sender<u64>, Receiver<u64>) = smol.perform(Channel::bounded(2));
        let sum = smol
            .acquire_async(SpawnSet::<TaskSet<()>>::new(), async |_, set| {
                for t in 0..4u64 {
                    let mut tx = tx.clone();
                    let producer = async move {
                        for i in 0..100 {
                            tx.perform_send(SendMsg(t * 1000 + i)).await.unwrap();
                        }
                    };
                    let _ = set.perform_send(SpawnInto::new(producer)).await;
                }
                drop(tx);
                let mut sum = 0;
                loop {
                    match rx.perform_send(RecvMsg::new()).await {
                        Ok(n) => sum += n,
                        Err(RecvError::Closed) => break,
                        Err(e) => panic!("{e}"),
                    }
                }
                Ok::<_, JoinError>(sum)
            })
            .await
            .unwrap();
        assert_eq!(sum, (0..4).map(|t| t * 1000 * 100 + 4950).sum::<u64>());
    })
}

/// A receive cancelled by a timeout takes no message: the next receive
/// still gets it.
#[test]
fn a_cancelled_receive_loses_nothing() {
    seam_smol::block_on(async move {
        let mut smol = Smol::new();
        let (mut tx, mut rx): (Sender<&str>, Receiver<&str>) = smol.perform(Channel::unbounded());
        let timed_out = timeout(Duration::from_millis(10), rx.perform_send(RecvMsg::new())).await;
        assert!(timed_out.is_err());
        tx.perform_send(SendMsg("still here")).await.unwrap();
        assert_eq!(rx.perform_send(RecvMsg::new()).await, Ok("still here"));
    })
}

#[test]
fn cells_across_tasks() {
    seam_smol::block_on(async move {
        let mut smol = Smol::new();
        let counter: Shared<u64> = smol.perform(Share::new(0));
        smol.acquire_async(SpawnSet::<TaskSet<()>>::new(), async |_, set| {
            for _ in 0..8 {
                let mut c = counter.clone();
                let task = async move {
                    for _ in 0..1000 {
                        c.perform_send(Update::new(&mut |n: &mut u64| *n += 1))
                            .await;
                        smol::future::yield_now().await;
                    }
                };
                let _ = set.perform_send(SpawnInto::new(task)).await;
            }
            while set.perform_send(JoinNext::<()>::new()).await.is_some() {}
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();
        assert_eq!(counter.inspect_send(Load::new()).await, 8000);
    })
}
