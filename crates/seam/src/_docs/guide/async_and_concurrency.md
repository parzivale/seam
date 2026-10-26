# Async and concurrency

## Async cores

An async core uses the async forms of the capability traits:
`AsyncInspect` and `AsyncPerform`, with `inspect_async` and
`perform_async`. Every sync capability is also an async one, so the same
core runs on Tokio, and on `Std` or `Sim` through
[`block_on`](crate::future::block_on):

```rust
# #[cfg(all(feature = "test", feature = "time"))] {
use core::time::Duration;
use seam::prelude::*;
use seam::ops::time::{Now, Sleep};

async fn pause<C: AsyncPerform<Sleep> + Inspect<Now>>(c: &mut C) -> Duration {
    let start = c.inspect(Now);
    c.perform_async(Sleep(Duration::from_secs(2))).await;
    c.inspect(Now) - start
}

let mut sim = seam::test::Sim::new();
assert_eq!(seam::future::block_on(pause(&mut sim)), Duration::from_secs(2));
# }
```

On a sync capability every future is ready on its first poll, so
`block_on` never waits.

## `Send` cores, for multithreaded runtimes

Rust cannot be generic over whether a future is `Send`. A core whose
future must be (one spawned on Tokio's multithreaded runtime) uses the
`Send` forms: `SendInspect` and `SendPerform`, with `inspect_send` and
`perform_send`, and `SendProvide` / `SendLend` with `acquire_send` /
`lend_send` for scopes. Helpers come in the same three forms:
`write_all`, `write_all_async`, `write_all_send`.

```rust
# #[cfg(all(feature = "test", feature = "time"))] {
use core::time::Duration;
use seam::prelude::*;
use seam::ops::time::Sleep;

async fn heartbeat<C: SendPerform<Sleep> + Send>(c: &mut C, beats: u32) {
    for _ in 0..beats {
        c.perform_send(Sleep(Duration::from_secs(1))).await;
    }
}

// `Sim` is `Send + Sync`, so a `Send` core runs on it too.
let mut sim = seam::test::Sim::new();
seam::future::block_on(heartbeat(&mut sim, 3));
# }
```

In `main`, run it on Tokio:

```rust,ignore
fn main() {
    seam::tokio::block_on(heartbeat(&mut seam::tokio::Tokio, 3));
}
```

Tokio covers what waits (time, files, the network, stdio, processes,
signals, tasks, channels) but has no environment or randomness; a Tokio
program takes those from `Std` in the same [root](crate::_docs::guide::roots).

A sync operation raised to async still blocks: a `Std` file read inside an
async core blocks the runtime's thread. That is sometimes what a program
wants (a small CLI, a test, an instant operation like `Now`) and is
documented rather than prevented.

## Tasks

Spawning is an acquisition. The scope gets a handle to the task, and
waits for it if it succeeds or aborts it if not. Tasks are `'static`, so
each takes its own clone of the capability (`Std`, `Tokio` and roots of
them are `Copy`; the simulated capabilities are `Clone`).

## Testing concurrent async cores

`seam::test::rt::Runtime` runs async code over a `Sim` on a deterministic,
single-threaded executor. Cores get an `AsyncSim`, a clonable capability
over the same world:

```rust
# #[cfg(all(feature = "test", feature = "rt", feature = "time"))] {
use core::time::Duration;
use seam::prelude::*;
use seam::ops::task::{Join, Spawn};
use seam::ops::time::{Now, Sleep};
use seam::test::Sim;
use seam::test::rt::{JoinHandle, Runtime};

let rt = Runtime::new(Sim::new());
rt.block_on(|mut sim| async move {
    let mut other = sim.clone();
    let slow = async move { other.perform_send(Sleep(Duration::from_secs(60))).await };
    sim.acquire_async(Spawn::<_, JoinHandle<()>>::new(slow), async |sim, task| {
        sim.perform_send(Sleep(Duration::from_secs(1))).await;
        task.perform_send(Join::new()).await
    })
    .await
    .unwrap();
    // A minute of timers takes no real time: the clock jumps when every
    // task is waiting.
    assert_eq!(sim.inspect(Now).since_epoch(), Duration::from_secs(60));
});
# }
```

- **Waiting.** Reads, accepts, receives and `CtrlC` suspend until another
  task makes them possible. The runtime's own sockets are in
  `seam::test::rt::net`.
- **Virtual time.** `Sleep` registers a timer. When no task is ready, the
  clock jumps to the earliest deadline.
- **Seeded scheduling.** `Runtime::with_seed(sim, seed)` picks the next
  ready task from a seed. The same seed gives the same interleaving; loop
  over seeds to explore others, and replay a failure from its seed.
- **Deadlocks.** With no task ready and no timer set, `block_on` panics
  and `try_block_on` returns `Deadlock`, instead of hanging.
- **Between runs.** `rt.with_sim(|sim| ..)` sets the world up before a run
  and inspects it after.

## Threads

On `seam-std`, `Spawn`, `SpawnSet` and `SpawnWith` run closures on OS
threads, with the same scope rules, except that a thread cannot be aborted:
a scope that fails joins it. Channels and cells (`Channel`, `SendMsg`,
`RecvMsg`, `Share`, `Load`, `Update`) come from the backend like every
other effect, so threads share state only through operations.

`seam::test::threads::Threads` tests such cores: each simulated thread is a
real thread, but only one runs at a time, and every operation is a
scheduling point where the seed picks which runnable thread continues.

```rust
# #[cfg(all(feature = "test", feature = "rt", feature = "sync"))] {
use seam::prelude::*;
use seam::ops::sync::{Channel, RecvMsg, SendMsg};
use seam::ops::task::{Join, JoinError, Spawn};
use seam::test::Sim;
use seam::test::threads::sync::{Receiver, Sender};
use seam::test::threads::{JoinHandle, Threads};

for seed in 0..8 {
    let threads = Threads::with_seed(Sim::new(), seed);
    let got = threads.run(|mut sim| {
        let (mut tx, mut rx): (Sender<u32>, Receiver<u32>) = sim.perform(Channel::unbounded());
        let worker = Spawn::<_, JoinHandle<()>>::new(move || {
            tx.perform(SendMsg(7)).unwrap();
        });
        sim.acquire(worker, |_, thread| {
            let v = rx.perform(RecvMsg::new()).unwrap();
            thread.perform(Join::new())?;
            Ok::<_, JoinError>(v)
        })
        .unwrap()
    });
    assert_eq!(got, 7);
}
# }
```

Blocking operations (receives, sends to a full channel, joins, reads,
accepts, `CtrlC`) block the simulated thread until another makes them
possible, `Sleep` uses the same jumping virtual clock, and a deadlock is
reported. An `Update` holds its cell for the whole closure, as a mutex
would.

## Timeouts

Any operation that waits can be given a deadline with `Timeout`. Its
outcome is the operation's own, so a receive that times out reports
`Empty`, a send `Full` with the value handed back, I/O
`io::ErrorKind::TimedOut`, and a join `JoinError::TimedOut` with the task
still running:

```rust,ignore
match rx.perform(Timeout::new(Duration::from_secs(1), RecvMsg::new())) {
    Ok(job) => run(job),
    Err(RecvError::Empty) => check_for_shutdown(),
    Err(RecvError::Closed) => return,
}
```

A timeout of zero is a non-blocking attempt: `try_send` and `try_recv` are
built that way. On the simulated runtime and threads, timeouts run on the
virtual clock, so a test can check exactly when one fired.
