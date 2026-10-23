//! Defining your own operations, and building capabilities out of others.
//!
//! seam's built-in operations cover the OS. A program's own effects (a
//! key-value store, a payment API, a message queue) are operations too.
//! This example defines a tiny key-value store, writes a core against it,
//! and gives it two implementations:
//!
//! - `Memory`, a plain in-memory map, for unit tests of the core.
//! - `FileStore<C, F>`, which keeps the store in a file. It is a capability
//!   built from other capabilities: it needs only `ReadToString` and
//!   `CreateAtomic` from whatever `C` it wraps, so it runs on the real
//!   filesystem in `main` and on the simulated one in tests. Every write
//!   goes through an atomic file in a scope, so a crash, an error or a
//!   cancellation mid-write never leaves a half-written store.
//!
//! The store's operations are a kind of their own, `kv::Kv`, so `main` can
//! put the store in a [`Root`](seam::Root) next to `Std`: one capability
//! that serves both the program's own operations and seam's.
//!
//! ```text
//! cargo run -p seam-rs --example custom_ops --features std,test,fs,env
//! ```

use seam::io;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;

use seam::ops::env::TempDir;
use seam::ops::fs::{CreateAtomic, ReadToString};
use seam::ops::io::WriteAll;
use seam::prelude::*;

/// The store's operations.
mod kv {
    use seam::io;

    use seam::{InKind, Operation, Query};

    /// The kind of these operations, so a root can tell which part serves
    /// them.
    pub struct Kv;

    /// Read a key.
    pub struct Get(pub String);

    /// Set a key.
    pub struct Put(pub String, pub String);

    impl Operation for Get {
        type Outcome = io::Result<Option<String>>;
    }
    impl Query for Get {}

    impl Operation for Put {
        type Outcome = io::Result<()>;
    }

    impl InKind for Get {
        type Kind = Kv;
    }
    impl InKind for Put {
        type Kind = Kv;
    }
}
use kv::{Get, Put};

/// The core: counts visits to a page. Knows nothing about where the store
/// lives.
fn visit<K: Inspect<Get> + Perform<Put>>(store: &mut K, page: &str) -> io::Result<u64> {
    let key = format!("visits:{page}");
    let n = match store.inspect(Get(key.clone()))? {
        Some(n) => {
            n.parse::<u64>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "a count is not a number")
            })? + 1
        }
        None => 1,
    };
    store.perform(Put(key, n.to_string()))?;
    Ok(n)
}

/// An in-memory store.
#[derive(Default)]
#[cfg_attr(not(test), allow(dead_code))] // used by the tests
struct Memory(BTreeMap<String, String>);
impl Capability for Memory {}

impl Inspect<Get> for Memory {
    fn inspect(&self, op: Get) -> io::Result<Option<String>> {
        Ok(self.0.get(&op.0).cloned())
    }
}

impl Perform<Put> for Memory {
    fn perform(&mut self, op: Put) -> io::Result<()> {
        self.0.insert(op.0, op.1);
        Ok(())
    }
}

/// A store kept in a file of `key\tvalue` lines, on top of any filesystem
/// capability `C` whose atomic files are of type `F`.
struct FileStore<C, F> {
    fs: C,
    path: PathBuf,
    atomic: PhantomData<fn() -> F>,
}
impl<C, F> Capability for FileStore<C, F> {}

impl<C, F> FileStore<C, F> {
    fn new(fs: C, path: impl Into<PathBuf>) -> Self {
        Self {
            fs,
            path: path.into(),
            atomic: PhantomData,
        }
    }
}

impl<C: Inspect<ReadToString>, F> FileStore<C, F> {
    fn load(&self) -> io::Result<BTreeMap<String, String>> {
        match self.fs.inspect(ReadToString::new(&self.path)) {
            Ok(text) => Ok(text
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e),
        }
    }
}

impl<C: Inspect<ReadToString>, F> Inspect<Get> for FileStore<C, F> {
    fn inspect(&self, op: Get) -> io::Result<Option<String>> {
        Ok(self.load()?.remove(&op.0))
    }
}

impl<C, F> Perform<Put> for FileStore<C, F>
where
    C: Inspect<ReadToString> + Provide<CreateAtomic<F>>,
    F: for<'a> Perform<WriteAll<'a>>,
{
    /// Rewrites the whole file, atomically: the new file is renamed into
    /// place only if the scope succeeds, that is, if every line was written.
    fn perform(&mut self, op: Put) -> io::Result<()> {
        let mut map = self.load()?;
        map.insert(op.0, op.1);
        self.fs.acquire(CreateAtomic::new(&self.path), |_, file| {
            for (k, v) in &map {
                file.perform(WriteAll(format!("{k}\t{v}\n").as_bytes()))?;
            }
            Ok(())
        })
    }
}

fn main() -> io::Result<()> {
    use seam::stdlib::Std;
    use seam::stdlib::fs::AtomicFile;

    let path = Std::new().inspect(TempDir).join("seam-custom-ops.tsv");
    // The program's root: `Std` for the OS, the file store for `Kv`.
    let mut root = seam::root! {
        std: Std::new() => [seam::kinds::Env],
        store: FileStore::<_, AtomicFile>::new(Std::new(), path) => [kv::Kv],
    };
    let n = visit(&mut root, "home")?;
    // `main` is the edge, so it may print directly.
    println!(
        "home has been visited {n} times (stored in {})",
        root.parts().store.path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::ops::fs::ReadDir;
    use seam::test::Sim;
    use seam::test::fs::AtomicFile;

    use super::*;

    /// One behaviour, checked against both implementations.
    fn counts_visits(store: &mut (impl Inspect<Get> + Perform<Put>)) {
        assert_eq!(visit(store, "home").unwrap(), 1);
        assert_eq!(visit(store, "home").unwrap(), 2);
        assert_eq!(visit(store, "about").unwrap(), 1);
    }

    #[test]
    fn in_memory() {
        counts_visits(&mut Memory::default());
    }

    #[test]
    fn in_a_simulated_file() {
        let mut store = FileStore::<_, AtomicFile>::new(Sim::new(), "/tmp/kv.tsv");
        counts_visits(&mut store);
        assert_eq!(
            store.fs.file("/tmp/kv.tsv").unwrap(),
            b"visits:about\t1\nvisits:home\t2\n"
        );
        // The temporary file behind each atomic write is gone.
        assert_eq!(
            store.fs.inspect(ReadDir::new("/tmp")).unwrap(),
            [PathBuf::from("/tmp/kv.tsv")]
        );
    }

    #[test]
    fn corrupt_store_is_an_error() {
        let mut sim = Sim::new();
        sim.add_file("/tmp/kv.tsv", "visits:home\tmany\n");
        let mut store = FileStore::<_, AtomicFile>::new(sim, "/tmp/kv.tsv");
        assert_eq!(
            visit(&mut store, "home").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
