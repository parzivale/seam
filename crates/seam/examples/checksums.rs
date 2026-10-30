//! Checksums of many files at once, on smol (`seam::smol`): a task per
//! file in a task set, results as they finish.
//!
//! The core spawns through `SpawnSet` and reads through `View`, so the
//! program runs on smol's executor and filesystem while the tests run on
//! the deterministic simulated runtime, under several schedules.
//!
//! ```text
//! cargo run -p seam-rs --example checksums --features std,test,smol,fs,rt -- Cargo.toml README.md
//! ```

use std::future::Future;
use std::path::PathBuf;

use seam::io;
use seam::ops::fs::{ReadAt, View, read_file_send};
use seam::ops::task::{JoinNext, SpawnInto, SpawnSet};
use seam::prelude::*;

/// A file's checksum, or why there is none.
type Sum = (PathBuf, io::Result<u64>);

/// FNV-1a, 64-bit: small, and enough to tell files apart.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// One file's job, on its own copy of the capability.
async fn checksum<C, F>(c: C, path: PathBuf) -> Sum
where
    C: for<'a> SendLend<View<'a, F>> + Sync,
    F: for<'a> SendInspect<ReadAt<'a>> + Sync,
{
    let sum = read_file_send(&c, &path).await.map(|bytes| fnv1a(&bytes));
    (path, sum)
}

/// Runs `job` on every path at once; the sums sorted by path. The job's
/// future type is a parameter, so nothing is boxed.
async fn checksums<C, S, J, Fut>(c: &mut C, paths: Vec<PathBuf>, job: J) -> io::Result<Vec<Sum>>
where
    C: AsyncProvide<SpawnSet<S>> + Send,
    S: SendPerform<SpawnInto<Fut>> + SendPerform<JoinNext<Sum>> + Send,
    J: Fn(PathBuf) -> Fut + Send,
    Fut: Future<Output = Sum> + Send,
{
    c.acquire_async(SpawnSet::new(), async |_, set| {
        for path in paths {
            if set.perform_send(SpawnInto::new(job(path))).await.is_err() {
                return Err(io::Error::other("the task set is full"));
            }
        }
        let mut sums = Vec::new();
        while let Some(done) = set.perform_send(JoinNext::new()).await {
            sums.push(done.map_err(io::Error::from)?);
        }
        sums.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(sums)
    })
    .await
}

/// The edge: smol.
fn main() -> io::Result<()> {
    use seam::smol::Smol;
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let sums = seam::smol::block_on(checksums::<_, seam::smol::task::TaskSet<Sum>, _, _>(
        &mut Smol::new(),
        paths,
        |p| checksum(Smol::new(), p),
    ))?;
    for (path, sum) in sums {
        match sum {
            Ok(sum) => println!("{sum:016x}  {}", path.display()),
            Err(e) => println!("{:16}  {} ({e})", "-", path.display()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;
    use seam::test::rt::{AsyncSim, Runtime, TaskSet};

    use super::*;

    #[test]
    fn every_file_under_every_schedule() {
        for seed in 0..8 {
            let mut sim = Sim::new();
            sim.add_file("/a.txt", "alpha").add_file("/b.txt", "beta");
            let rt = Runtime::with_seed(sim, seed);
            let sums = rt.block_on(|mut sim| async move {
                let job = {
                    let sim = sim.clone();
                    move |p| checksum(sim.clone(), p)
                };
                checksums::<AsyncSim, TaskSet<Sum>, _, _>(
                    &mut sim,
                    vec!["/b.txt".into(), "/missing".into(), "/a.txt".into()],
                    job,
                )
                .await
                .unwrap()
            });
            let names: Vec<_> = sums.iter().map(|(p, _)| p.to_str().unwrap()).collect();
            assert_eq!(names, ["/a.txt", "/b.txt", "/missing"], "seed {seed}");
            assert_eq!(*sums[0].1.as_ref().unwrap(), fnv1a(b"alpha"));
            assert_eq!(
                sums[2].1.as_ref().unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
    }
}
