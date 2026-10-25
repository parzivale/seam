//! Count the words in files, on a pool of OS threads.
//!
//! ```text
//! word_count <file>...
//! cargo run -p seam-rs --example word_count --features std,test,fs,env,io-std,rt,sync -- src/*.rs
//! ```
//!
//! The core is synchronous: it starts one worker thread per available core
//! (at most one per file), and the workers take paths from a shared queue
//! until it is empty. Each returns its counts, and the main thread prints
//! them sorted, with a total.
//!
//! The same core runs on `seam-std`'s OS threads in `main`, and in the
//! tests on `seam::test::threads`, which runs its threads one at a time in
//! an order chosen from a seed. Sweeping seeds there checks that the
//! output does not depend on how the threads interleave.

use seam::io;
use seam::ops::fs::{ReadAt, View, read_to_string};
use seam::ops::io::Write;
use seam::ops::stdio::{OpenStdout, print};

use seam::ops::env::Args;
use seam::ops::sync::{Share, Update};
use seam::ops::task::{AvailableParallelism, JoinNext, SpawnInto, SpawnSet};
use seam::prelude::*;

/// What each worker returns: the files it counted, and their word counts.
type Counts = Vec<(String, usize)>;

/// A worker. Boxed so that the bounds below can name its type; a closure's
/// type has no name.
type Worker = Box<dyn FnOnce() -> io::Result<Counts> + Send>;

/// Takes paths from `queue` until it is empty, counting each file's words.
fn worker<C, R, Q>(c: C, mut queue: Q) -> io::Result<Counts>
where
    C: for<'a> Lend<View<'a, R>>,
    R: for<'a> Inspect<ReadAt<'a>>,
    Q: for<'a> Perform<Update<'a, Vec<String>, Option<String>>>,
{
    let mut counts = Vec::new();
    // Popping is one `Update`, so no two workers take the same path.
    while let Some(path) = queue.perform(Update::new(&mut |q: &mut Vec<String>| q.pop())) {
        let text = read_to_string(&c, &path)?;
        counts.push((path, text.split_whitespace().count()));
    }
    Ok(counts)
}

/// The program. `Q` is the backend's shared cell, `S` its set of threads,
/// `R` its read-only file and `O` its standard output; all are inferred from
/// the backend.
fn run<C, Q, S, R, O>(c: &mut C) -> io::Result<()>
where
    C: Inspect<Args>
        + Inspect<AvailableParallelism>
        + for<'a> Lend<View<'a, R>>
        + Provide<OpenStdout<O>>
        + Perform<Share<Vec<String>, Q>>
        + Provide<SpawnSet<S>>
        + Clone
        + Send
        + 'static,
    Q: for<'a> Perform<Update<'a, Vec<String>, Option<String>>> + Clone + Send + 'static,
    S: Perform<SpawnInto<Worker>> + Perform<JoinNext<io::Result<Counts>>>,
    R: for<'a> Inspect<ReadAt<'a>>,
    O: for<'a> Perform<Write<'a>>,
{
    let paths: Vec<String> = c
        .inspect(Args)
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: word_count <file>...",
        ));
    }
    let workers = c.inspect(AvailableParallelism).get().min(paths.len());
    let queue = c.perform(Share::new(paths));

    let mut counts = c.acquire(SpawnSet::<S>::new(), |c, set| {
        for _ in 0..workers {
            let work: Worker = Box::new({
                let (c, queue) = (c.clone(), queue.clone());
                move || worker(c, queue)
            });
            let _ = set.perform(SpawnInto::new(work)); // never full
        }
        let mut counts = Counts::new();
        while let Some(result) = set.perform(JoinNext::new()) {
            counts.extend(result??);
        }
        io::Result::Ok(counts)
    })?;

    counts.sort();
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    for (path, n) in counts {
        print(c, format!("{n:>8} {path}\n").as_bytes())?;
    }
    print(c, format!("{total:>8} total\n").as_bytes())
}

fn main() -> io::Result<()> {
    use seam::stdlib::{Std, sync::Shared, task::TaskSet};

    run::<_, Shared<_>, TaskSet<_>, _, _>(&mut Std)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use seam::test::Sim;
    use seam::test::threads::{ThreadSet, Threads, sync::Shared};

    use super::*;

    fn world(cores: usize) -> Sim {
        let mut sim = Sim::from_world(include_str!("word_count.world")).unwrap();
        sim.set_parallelism(NonZeroUsize::new(cores).unwrap());
        sim
    }

    fn output(sim: Sim, seed: u64) -> io::Result<String> {
        let threads = Threads::with_seed(sim, seed);
        threads.run(|mut sim| run::<_, Shared<_>, ThreadSet<_>, _, _>(&mut sim))?;
        Ok(String::from_utf8(threads.with_sim(|s| s.take_stdout())).unwrap())
    }

    /// The same check, written as expectations on the world afterwards.
    #[test]
    fn the_world_afterwards() {
        let threads = Threads::with_seed(world(2), 3);
        threads
            .run(|mut sim| run::<_, Shared<_>, ThreadSet<_>, _, _>(&mut sim))
            .unwrap();
        threads.with_sim(|sim| {
            sim.assert_world(
                r#"
                stdio {
                    stdout """
                               3 /a.txt
                               1 /b.txt
                               2 /c.txt
                               0 /d.txt
                               6 total

                        """
                }
                "#,
            )
        });
    }

    #[test]
    fn counts_every_file_once_whatever_the_interleaving() {
        let expected =
            "       3 /a.txt\n       1 /b.txt\n       2 /c.txt\n       0 /d.txt\n       6 total\n";
        for cores in [1, 2, 8] {
            for seed in 0..16 {
                assert_eq!(
                    output(world(cores), seed).unwrap(),
                    expected,
                    "{cores} cores, seed {seed}"
                );
            }
        }
    }

    #[test]
    fn a_missing_file_fails_the_run() {
        for seed in 0..8 {
            let mut sim = world(2);
            sim.set_args(["word_count", "/a.txt", "/nope.txt"]);
            let err = output(sim, seed).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "seed {seed}");
        }
    }

    #[test]
    fn needs_a_file() {
        let mut sim = world(2);
        sim.set_args(["word_count"]);
        assert_eq!(
            output(sim, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
