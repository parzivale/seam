//! Roots built from parts: real backends mixed, simulated parts, and a
//! program's own effects alongside seam's.
#![cfg(all(feature = "std", feature = "tokio", feature = "test", feature = "full"))]

use seam::io;
use seam::ops::fs::{ReadAt, View};
use seam::ops::fs::{read_to_string_send, write_file_send};
use seam::ops::io::write_all;
use seam::ops::random::random_u64;
use std::collections::BTreeMap;
use std::time::Duration;

use seam::effects as fx;
use seam::ops::env::Var;
use seam::ops::fs::CreateAtomic;
use seam::ops::fs::{Open, WriteOnly};
use seam::ops::io::Write;
use seam::ops::sync::{Channel, RecvMsg, SendMsg};
use seam::ops::task::{Join, JoinError, Spawn};
use seam::ops::time::{Now, Sleep};
use seam::prelude::*;
use seam::stdlib::Std;
use seam::test::Sim;
use seam::tokio::Tokio;

// ---- a program's own effects: a key-value store ----

struct Get(String);
impl Operation for Get {
    type Outcome = Option<String>;
}
impl Query for Get {}
impl Effect for Get {
    type Key = Get;
}

struct Put(String, String);
impl Operation for Put {
    type Outcome = ();
}
impl Effect for Put {
    type Key = Put;
}

#[derive(Default, Clone)]
struct Memory(BTreeMap<String, String>);
impl Capability for Memory {}
impl Inspect<Get> for Memory {
    fn inspect(&self, op: Get) -> Option<String> {
        self.0.get(&op.0).cloned()
    }
}
impl Perform<Put> for Memory {
    fn perform(&mut self, op: Put) {
        self.0.insert(op.0, op.1);
    }
}

// ---- cores: bounded exactly as they would be on one backend ----

/// Remembers who visited, using the store, the environment and the clock.
fn visit<C>(c: &mut C) -> String
where
    C: for<'a> Inspect<Var<'a>> + Inspect<Now> + Perform<Sleep> + Inspect<Get> + Perform<Put>,
{
    let user = c
        .inspect(Var::new("SEAM_ROOT_TEST_USER"))
        .map_or_else(|| "nobody".to_owned(), |v| v.to_string_lossy().into_owned());
    let seen = c.inspect(Get(user.clone())).is_some();
    c.perform(Sleep(Duration::from_millis(1)));
    c.perform(Put(user.clone(), "seen".into()));
    if seen {
        format!("welcome back, {user}")
    } else {
        format!("hello, {user}")
    }
}

/// An async core with `Send` bounds, for a multithreaded runtime.
async fn save<C, W, R>(c: &mut C, path: &str) -> io::Result<String>
where
    C: SendPerform<Sleep>
        + for<'a> SendProvide<Open<'a, WriteOnly, W>>
        + for<'a> SendLend<View<'a, R>>
        + Inspect<Now>
        + Send
        + Sync,
    W: for<'a> SendPerform<Write<'a>> + Send,
    R: for<'a> SendInspect<ReadAt<'a>> + Sync,
{
    let before = c.inspect(Now);
    c.perform_send(Sleep(Duration::from_millis(5))).await;
    assert!(c.inspect(Now) > before);
    write_file_send(c, path, "saved").await?;
    read_to_string_send(c, path).await
}

#[test]
fn a_sync_root_with_a_simulated_world_and_an_own_part() {
    seam::root! {
        struct App {
            sim: Sim => [fx::Var, fx::Now, fx::Sleep, fx::CreateAtomic],
            store: Memory => [Get, Put],
        }
    }
    let mut sim = Sim::new();
    sim.set_var("SEAM_ROOT_TEST_USER", "ada");
    let mut root = Root::new(App {
        sim,
        store: Memory::default(),
    });
    assert_eq!(visit(&mut root), "hello, ada");
    assert_eq!(visit(&mut root), "welcome back, ada");
    assert_eq!(root.parts().store.0["ada"], "seen");

    // Acquisitions go through the root too.
    root.acquire(CreateAtomic::new("/tmp/out"), |_, file| {
        write_all(file, b"ok")
    })
    .unwrap();
    assert_eq!(root.parts().sim.file("/tmp/out").unwrap(), b"ok");
}

/// Tokio for what waits, std for what does not: one root, sync and async
/// parts mixed, serving a `Send` async core on a multithreaded runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokio_and_std_in_one_root() {
    seam::root! {
        struct App {
            std: Std => [fx::FillRandom],
            tokio: Tokio => [fx::Now, fx::Sleep, fx::Open<WriteOnly>, fx::View],
        }
    }
    let mut root = Root::new(App {
        std: Std::new(),
        tokio: Tokio::new(),
    });
    let path = std::env::temp_dir().join(format!("seam-root-{}", std::process::id()));
    let path = path.to_str().unwrap().to_owned();
    let read = tokio::spawn(async move {
        let out = save(&mut root, &path).await;
        let _ = random_u64(&mut root); // from the std part
        let _ = std::fs::remove_file(&path);
        out
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(read, "saved");
}

/// On simulated threads, a root of clonable parts is moved into each
/// thread, and spawning goes through it.
#[test]
fn a_root_on_simulated_threads() {
    use seam::test::threads::sync::{Receiver, Sender};
    use seam::test::threads::{JoinHandle, ThreadSim, Threads};

    seam::root! {
        #[derive(Clone)]
        struct App {
            sim: ThreadSim => [fx::Now, fx::Sleep, fx::Spawn, fx::Channel],
            store: Memory => [Get, Put],
        }
    }
    let threads = Threads::new(Sim::new());
    let got = threads.run(|sim| {
        let mut root = Root::new(App {
            sim,
            store: Memory::default(),
        });
        let (mut tx, mut rx): (Sender<u32>, Receiver<u32>) = root.perform(Channel::unbounded());
        let mut worker = root.clone();
        let late = Spawn::<_, JoinHandle<()>>::new(move || {
            worker.perform(Sleep(Duration::from_secs(3)));
            tx.perform(SendMsg(7)).unwrap();
        });
        root.acquire(late, |root, thread| {
            let v = rx.perform(RecvMsg::new()).unwrap();
            assert_eq!(root.inspect(Now).since_epoch(), Duration::from_secs(3));
            thread.perform(Join::new())?;
            Ok::<_, JoinError>(v)
        })
        .unwrap()
    });
    assert_eq!(got, 7);
}

/// A named root, for when its type must be written: here, a field.
#[test]
fn a_named_root() {
    seam::root! {
        #[derive(Default)]
        struct App {
            store: Memory => [Get, Put],
        }
    }
    struct Service {
        root: Root<App>,
    }
    let mut service = Service {
        root: Root::new(App::default()),
    };
    service.root.perform(Put("k".into(), "v".into()));
    assert_eq!(service.root.inspect(Get("k".into())).as_deref(), Some("v"));
}

/// Two roots in one function do not clash.
#[test]
fn roots_side_by_side() {
    let a = seam::root! { store: Memory::default() => [Get, Put] };
    let b = seam::root! { sim: Sim::new() => [fx::Now], store: Memory::default() => [Get] };
    assert_eq!(a.inspect(Get("x".into())), None);
    assert_eq!(b.inspect(Now).since_epoch(), Duration::ZERO);
}

// ---- roots built from handlers, one per effect ----

/// A handler of a program's own: a unit struct serving `Get`.
#[derive(Default)]
struct Fixed;
impl Effect for Fixed {
    type Key = Get;
}
impl Capability for Fixed {}
impl Inspect<Get> for Fixed {
    fn inspect(&self, op: Get) -> Option<String> {
        Some(format!("fixed {}", op.0))
    }
}

/// A tuple-struct handler, serving `Put` into a log.
struct Log(Vec<String>);
impl Effect for Log {
    type Key = Put;
}
impl Capability for Log {}
impl Perform<Put> for Log {
    fn perform(&mut self, op: Put) {
        self.0.push(format!("{}={}", op.0, op.1));
    }
}

/// A struct-literal handler, serving `Now` at a fixed instant.
struct Frozen {
    at: seam::ops::time::Instant,
}
impl Effect for Frozen {
    type Key = fx::Now;
}
impl Capability for Frozen {}
impl Inspect<Now> for Frozen {
    fn inspect(&self, _: Now) -> seam::ops::time::Instant {
        self.at
    }
}

#[test]
fn a_root_of_handlers() {
    let mut root = seam::root![
        Std<fx::Var>, // a backend, restricted to one effect
        Std<fx::Sleep>,
        Fixed,           // a handler of this crate: a unit struct
        Log(Vec::new()), // a tuple struct
        Frozen {
            at: seam::ops::time::Instant::EPOCH
        }, // a struct literal
    ];
    // `visit` asks for Var, Now, Sleep, Get and Put: exactly what is listed.
    assert_eq!(visit(&mut root), "welcome back, nobody");
    assert_eq!(root.part::<Put>().0, ["nobody=seen"]);
    assert_eq!(root.part::<fx::Now>().at, seam::ops::time::Instant::EPOCH);
}

#[test]
fn handlers_and_keyed_parts_mix() {
    let mut sim = Sim::new();
    sim.set_var("SEAM_ROOT_TEST_USER", "ada");
    let mut root = seam::root![
        sim: sim => [fx::Var, fx::Now, fx::Sleep],
        Fixed,
        Memory = Memory::default(),                     // `Handler = value`
    ];
    assert_eq!(root.inspect(Get("k".into())).as_deref(), Some("fixed k"));
    root.perform(Put("k".into(), "v".into()));
    assert_eq!(root.part::<Put>().0["k"], "v");
}

impl Effect for Memory {
    type Key = Put;
}

/// Tokio's handlers in a root serving a `Send` core on a multithreaded
/// runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokio_handlers_in_a_send_root() {
    let mut root = seam::root![
        Tokio<fx::Now>,
        Tokio<fx::Sleep>,
        Tokio<fx::Open<WriteOnly>>,
        Tokio<fx::View>,
        Std<fx::FillRandom>,
    ];
    let path = std::env::temp_dir().join(format!("seam-handlers-{}", std::process::id()));
    let path = path.to_str().unwrap().to_owned();
    let read = tokio::spawn(async move {
        let out = save(&mut root, &path).await;
        let _ = std::fs::remove_file(&path);
        out
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(read, "saved");
}
