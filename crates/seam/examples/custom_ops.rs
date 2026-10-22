//! Defining your own operations, and building capabilities out of others.
//!
//! seam's built-in operations cover the OS. A program's own effects (a
//! key-value store, a payment API, a message queue) are operations too.
//! This example defines a tiny key-value store, writes a core against it,
//! and gives it two implementations:
//!
//! - `Memory`, a plain in-memory map, for unit tests of the core.
//! - `FileStore<C>`, which keeps the store in a file. It is a capability
//!   built from other capabilities: it needs only `ReadToString` and
//!   `WriteFile` from whatever `C` it wraps, so it runs on the real
//!   filesystem in `main` and on the simulated one in tests.
//!
//! ```text
//! cargo run -p seam --example custom_ops --features std,test,fs,env
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use seam::ops::env::TempDir;
use seam::ops::fs::{ReadToString, WriteFile};
use seam::prelude::*;

/// The store's operations.
mod kv {
    use std::io;

    use seam::{Operation, Query};

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
}
use kv::{Get, Put};

/// The core: counts visits to a page. Knows nothing about where the store
/// lives.
fn visit<K: Inspect<Get> + Perform<Put>>(store: &mut K, page: &str) -> io::Result<u64> {
    let key = format!("visits:{page}");
    let n = match store.inspect(Get(key.clone()))? {
        Some(n) => {
            n.parse::<u64>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                + 1
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
/// capability `C`.
struct FileStore<C> {
    fs: C,
    path: PathBuf,
}

impl<C: Inspect<ReadToString>> FileStore<C> {
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

impl<C: Inspect<ReadToString>> Inspect<Get> for FileStore<C> {
    fn inspect(&self, op: Get) -> io::Result<Option<String>> {
        Ok(self.load()?.remove(&op.0))
    }
}

impl<C: Inspect<ReadToString> + Perform<WriteFile>> Perform<Put> for FileStore<C> {
    fn perform(&mut self, op: Put) -> io::Result<()> {
        let mut map = self.load()?;
        map.insert(op.0, op.1);
        let text: String = map.iter().map(|(k, v)| format!("{k}\t{v}\n")).collect();
        self.fs.perform(WriteFile::new(&self.path, text))
    }
}

fn main() -> io::Result<()> {
    let std = seam::stdlib::Std::new();
    let path = std.inspect(TempDir).join("seam-custom-ops.tsv");
    let mut store = FileStore { fs: std, path };
    let n = visit(&mut store, "home")?;
    // `main` is the edge, so it may print directly.
    println!(
        "home has been visited {n} times (stored in {})",
        store.path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;

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
        let mut store = FileStore {
            fs: Sim::new(),
            path: "/tmp/kv.tsv".into(),
        };
        counts_visits(&mut store);
        assert_eq!(
            store.fs.file("/tmp/kv.tsv").unwrap(),
            b"visits:about\t1\nvisits:home\t2\n"
        );
    }

    #[test]
    fn corrupt_store_is_an_error() {
        let mut sim = Sim::new();
        sim.add_file("/tmp/kv.tsv", "visits:home\tmany\n");
        let mut store = FileStore {
            fs: sim,
            path: "/tmp/kv.tsv".into(),
        };
        assert_eq!(
            visit(&mut store, "home").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
