//! Channels and shared cells, in the plain simulation and on the runtime.
#![cfg(feature = "full")]

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use seam_core::future::{SendInspect, SendPerform};
use seam_core::ops::fs::read_to_string_send;
use seam_core::ops::fs::write_file_send;
use seam_core::ops::sync::try_recv_send;
use seam_core::ops::sync::{store, store_send, try_recv, try_send};

use seam_core::ops::sync::{Channel, Load, RecvError, RecvMsg, SendError, SendMsg, Share, Update};
use seam_core::ops::task::{JoinError, JoinNext, SpawnInto, SpawnSet, YieldNow};
use seam_core::ops::time::Sleep;
use seam_core::{AsyncAcquire, Inspect, Perform};
use seam_test::Sim;
use seam_test::rt::{Deadlock, Runtime, TaskSet};
use seam_test::sync::{Receiver, Sender, Shared};

mod plain {
    use super::*;

    /// Without a runtime nothing waits: a full channel and an empty one say
    /// so instead.
    #[test]
    fn full_empty_and_closed() {
        let mut sim = Sim::new();
        let (mut tx, mut rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::bounded(1));
        assert_eq!(rx.perform(RecvMsg::new()), Err(RecvError::Empty));
        tx.perform(SendMsg(1)).unwrap();
        assert_eq!(try_send(&mut tx, 2), Err(SendError::Full(2)));
        assert_eq!(tx.perform(SendMsg(2)), Err(SendError::Full(2)));
        assert_eq!(try_recv(&mut rx), Ok(1));

        let mut tx2 = tx.clone();
        drop(tx);
        tx2.perform(SendMsg(3)).unwrap();
        drop(tx2);
        assert_eq!(
            rx.perform(RecvMsg::new()),
            Ok(3),
            "queued messages survive the senders"
        );
        assert_eq!(rx.perform(RecvMsg::new()), Err(RecvError::Closed));

        let (mut tx, rx): (Sender<u8>, Receiver<u8>) = sim.perform(Channel::unbounded());
        drop(rx);
        assert_eq!(tx.perform(SendMsg(9)), Err(SendError::Closed(9)));
    }

    #[test]
    fn cells() {
        let mut sim = Sim::new();
        let mut a: Shared<Vec<u8>> = sim.perform(Share::new(Vec::new()));
        let mut b = a.clone();
        a.perform(Update::new(&mut |v: &mut Vec<u8>| v.push(1)));
        b.perform(Update::new(&mut |v: &mut Vec<u8>| v.push(2)));
        assert_eq!(
            a.inspect(Load::new()),
            [1, 2],
            "both clones see the same value"
        );
        store(&mut b, vec![7]);
        assert_eq!(a.inspect(Load::new()), [7]);
    }
}

/// A producer sends `n` numbers on a bounded channel; a consumer adds them
/// up. Both wait on each other, in whatever order the seed picks.
fn pipeline(seed: u64, capacity: usize, n: u64) -> u64 {
    let rt = Runtime::with_seed(Sim::new(), seed);
    rt.block_on(|mut sim| async move {
        let (tx, mut rx): (
            seam_test::rt::sync::Sender<u64>,
            seam_test::rt::sync::Receiver<u64>,
        ) = sim.perform(Channel::bounded(capacity));
        sim.acquire_async(SpawnSet::<TaskSet<()>>::new(), async |_, set| {
            let mut tx = tx;
            let producer = async move {
                for i in 1..=n {
                    tx.perform_send(SendMsg(i)).await.unwrap();
                }
                // `tx` is dropped here, which closes the channel.
            };
            let _ = set.perform_send(SpawnInto::new(producer)).await;
            let mut sum = 0;
            loop {
                match rx.perform_send(RecvMsg::new()).await {
                    Ok(i) => sum += i,
                    Err(RecvError::Closed) => break,
                    Err(e) => panic!("{e}"),
                }
            }
            Ok::<_, JoinError>(sum)
        })
        .await
        .unwrap()
    })
}

#[test]
fn sends_and_receives_wait_for_each_other() {
    for seed in 0..16 {
        assert_eq!(pipeline(seed, 1, 50), 1275, "seed {seed}");
        assert_eq!(pipeline(seed, 4, 50), 1275, "seed {seed}");
    }
}

/// A receiver waiting on a channel whose sender is alive but idle can never
/// be woken: the runtime reports it instead of hanging.
#[test]
fn waiting_on_an_idle_channel_is_a_deadlock() {
    let rt = Runtime::new(Sim::new());
    let result = rt.try_block_on(|mut sim| async move {
        let (_tx, mut rx): (
            seam_test::rt::sync::Sender<u8>,
            seam_test::rt::sync::Receiver<u8>,
        ) = sim.perform(Channel::unbounded());
        rx.perform_send(RecvMsg::new()).await
    });
    assert_eq!(result, Err(Deadlock { tasks: 0 }));
}

/// Polls `fut` up to `polls` times, without waiting in between, and drops
/// it if it has not finished by then. Returns its output if it finished.
async fn cancel_after<T>(polls: usize, fut: impl Future<Output = T>) -> Option<T> {
    let mut fut = pin!(fut);
    let mut count = 0;
    poll_fn(|cx| {
        if let Poll::Ready(v) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(v));
        }
        count += 1;
        if count >= polls {
            return Poll::Ready(None);
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await
}

/// Cancelling a receive or a send after any number of polls never loses
/// or duplicates a message: every message sent is received exactly once.
/// The channel fills up and drains unevenly, so sends are cancelled while
/// waiting for room and receives while waiting for a message.
#[test]
fn operations_are_atomic_under_cancellation() {
    for cut in 1..4 {
        let rt = Runtime::new(Sim::new());
        let (sent, got, cancelled) = rt.block_on(|mut sim| async move {
            let (mut tx, mut rx): (
                seam_test::rt::sync::Sender<u32>,
                seam_test::rt::sync::Receiver<u32>,
            ) = sim.perform(Channel::bounded(1));
            let (mut sent, mut got, mut cancelled) = (Vec::new(), Vec::new(), 0);
            for i in 0..30 {
                match cancel_after(cut, tx.perform_send(SendMsg(i))).await {
                    Some(Ok(())) => sent.push(i),
                    Some(Err(e)) => panic!("{e}"),
                    None => cancelled += 1, // waited for room, then cancelled
                }
                // Drain in bursts: sometimes the queue is full, sometimes empty.
                let receives = if i % 3 == 0 { 2 } else { 0 };
                for _ in 0..receives {
                    match cancel_after(cut, rx.perform_send(RecvMsg::new())).await {
                        Some(Ok(v)) => got.push(v),
                        Some(Err(e)) => panic!("{e}"),
                        None => cancelled += 1, // waited for a message, then cancelled
                    }
                }
            }
            drop(tx);
            while let Ok(v) = try_recv_send(&mut rx).await {
                got.push(v);
            }
            (sent, got, cancelled)
        });
        assert!(
            cancelled > 0,
            "cut {cut}: some operations were cancelled while waiting"
        );
        assert_eq!(
            sent, got,
            "cut {cut}: every message sent arrives exactly once, in order"
        );
    }
}

/// The lost-update race from the runtime tests, with the read-modify-write
/// done as one `Update`. No schedule loses a count.
#[test]
fn update_closes_the_race() {
    fn run_with(seed: u64, atomic: bool) -> u64 {
        let rt = Runtime::with_seed(Sim::new(), seed);
        rt.block_on(|mut sim| async move {
            let counter: Shared<u64> = sim.perform(Share::new(0));
            sim.acquire_async(SpawnSet::<TaskSet<()>>::new(), async |sim, set| {
                for _ in 0..3 {
                    let mut counter = counter.clone();
                    let mut sim = sim.clone();
                    let task = async move {
                        if atomic {
                            counter
                                .perform_send(Update::new(&mut |n: &mut u64| *n += 1))
                                .await;
                        } else {
                            let n = counter.inspect_send(Load::new()).await;
                            sim.perform_send(YieldNow).await; // the window
                            store_send(&mut counter, n + 1).await;
                        }
                    };
                    let _ = set.perform_send(SpawnInto::new(task)).await;
                }
                while set.perform_send(JoinNext::<()>::new()).await.is_some() {}
                Ok::<_, JoinError>(())
            })
            .await
            .unwrap();
            counter.inspect(Load::new())
        })
    }

    assert!(
        (0..32).any(|seed| run_with(seed, false) < 3),
        "the racy version loses counts"
    );
    for seed in 0..32 {
        assert_eq!(run_with(seed, true), 3, "seed {seed}: Update never does");
    }
}

/// Channels compose with the rest of the world: workers pull jobs from a
/// shared queue and write results to files.
#[test]
fn a_worker_pool() {
    let rt = Runtime::with_seed(Sim::new(), 3);
    rt.block_on(|mut sim| async move {
        let (jobs, rx): (
            seam_test::rt::sync::Sender<u32>,
            seam_test::rt::sync::Receiver<u32>,
        ) = sim.perform(Channel::bounded(2));
        // Workers take turns holding the receiver: a non-waiting receive is a
        // zero timeout, which the runtime's receivers perform asynchronously,
        // so it cannot run inside a (synchronous) `Update`.
        let queue: Shared<Option<seam_test::rt::sync::Receiver<u32>>> =
            sim.perform(Share::new(Some(rx)));
        sim.acquire_async(SpawnSet::<TaskSet<()>>::new(), async |sim, workers| {
            for _ in 0..3 {
                let mut sim = sim.clone();
                let mut queue = queue.clone();
                let worker = async move {
                    loop {
                        let taken = queue
                            .perform_send(Update::new(&mut |q: &mut Option<
                                seam_test::rt::sync::Receiver<u32>,
                            >| q.take()))
                            .await;
                        let Some(mut rx) = taken else {
                            sim.perform_send(Sleep(Duration::from_millis(1))).await;
                            continue;
                        };
                        let job = try_recv_send(&mut rx).await;
                        store_send(&mut queue, Some(rx)).await;
                        match job {
                            Ok(n) => {
                                sim.perform_send(Sleep(Duration::from_millis(u64::from(n))))
                                    .await;
                                write_file_send(
                                    &mut sim,
                                    &format!("/tmp/{n}"),
                                    &(n * n).to_string(),
                                )
                                .await
                                .unwrap();
                            }
                            Err(RecvError::Empty) => {
                                sim.perform_send(Sleep(Duration::from_millis(1))).await
                            }
                            Err(RecvError::Closed) => return,
                        }
                    }
                };
                let _ = workers.perform_send(SpawnInto::new(worker)).await;
            }
            let mut jobs = Some(jobs);
            for n in 1..=6 {
                jobs.as_mut()
                    .unwrap()
                    .perform_send(SendMsg(n))
                    .await
                    .unwrap();
            }
            drop(jobs.take()); // closing the queue lets the workers finish
            Ok::<_, JoinError>(())
        })
        .await
        .unwrap();
        for n in 1..=6u32 {
            let out = read_to_string_send(&mut sim, &format!("/tmp/{n}"))
                .await
                .unwrap();
            assert_eq!(out, (n * n).to_string());
        }
    });
}
