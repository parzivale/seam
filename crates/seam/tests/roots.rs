//! Roots built from parts: real backends mixed, simulated parts, and a
//! program's own kind of operation alongside seam's.
#![cfg(all(feature = "std", feature = "tokio", feature = "test", feature = "full"))]

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use seam::kinds;
use seam::ops::env::Var;
use seam::ops::fs::{CreateAtomic, ReadToString, WriteFile};
use seam::ops::io::WriteAll;
use seam::ops::random::RandomU64;
use seam::ops::sync::{Channel, RecvMsg, SendMsg};
use seam::ops::task::{Join, JoinError, Spawn};
use seam::ops::time::{Now, Sleep};
use seam::prelude::*;
use seam::stdlib::Std;
use seam::test::Sim;
use seam::tokio::Tokio;

// ---- a program's own kind: a key-value store ----

/// The kind: a marker type.
struct Store;

struct Get(String);
impl Operation for Get {
    type Outcome = Option<String>;
}
impl Query for Get {}
impl InKind for Get {
    type Kind = Store;
}

struct Put(String, String);
impl Operation for Put {
    type Outcome = ();
}
impl InKind for Put {
    type Kind = Store;
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
    C: Inspect<Var> + Inspect<Now> + Perform<Sleep> + Inspect<Get> + Perform<Put>,
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
async fn save<C>(c: &mut C, path: &str) -> io::Result<String>
where
    C: SendPerform<Sleep>
        + SendPerform<WriteFile>
        + SendInspect<ReadToString>
        + Inspect<Now>
        + Send,
{
    let before = c.inspect(Now);
    c.perform_send(Sleep(Duration::from_millis(5))).await;
    assert!(c.inspect(Now) > before);
    c.perform_send(WriteFile::new(path, "saved")).await?;
    c.inspect_send(ReadToString::new(path)).await
}

#[test]
fn a_sync_root_with_a_simulated_world_and_an_own_part() {
    seam::root! {
        struct App {
            sim: Sim => [kinds::Env, kinds::Time, kinds::Fs],
            store: Memory => [Store],
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
        file.perform(WriteAll(b"ok"))
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
            std: Std => [kinds::Env, kinds::Random],
            tokio: Tokio => [kinds::Time, kinds::Fs],
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
        let _ = root.perform(RandomU64); // from the std part
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
            sim: ThreadSim => [kinds::Time, kinds::Task, kinds::Sync],
            store: Memory => [Store],
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
            store: Memory => [Store],
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
    let a = seam::root! { store: Memory::default() => [Store] };
    let b = seam::root! { sim: Sim::new() => [kinds::Time], store: Memory::default() => [Store] };
    assert_eq!(a.inspect(Get("x".into())), None);
    assert_eq!(b.inspect(Now).since_epoch(), Duration::ZERO);
}
