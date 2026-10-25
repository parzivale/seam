//! Snapshot a directory, keeping the newest few snapshots.
//!
//! ```text
//! backup <source> <destination> [keep]
//! cargo run -p seam-rs --example backup --features std,test,fs,env,time,io-std -- src /tmp/backups 3
//! ```
//!
//! Each run copies `source` into `destination/<unix seconds>/`, writes a
//! `MANIFEST` listing what it copied, and deletes the oldest snapshots
//! beyond `keep` (default 5). Snapshots left without a manifest by a run
//! that died partway are removed.
//!
//! The core needs many capabilities, so it names them once with a bundling
//! trait, `BackupEnv`, instead of repeating a long `where` clause on every
//! function. The manifest is written through `CreateAtomic` in a scope, which
//! is what makes "has a manifest" mean "is complete".

use seam::io;
use seam::ops::fs::create_dir_all;
use seam::ops::io::{Write, write_all};
use seam::ops::stdio::{OpenStdout, print};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use seam::ops::env::Args;
use seam::ops::fs::{CopyFile, CreateAtomic, CreateDir, Metadata, NextEntry, ReadDir, RemoveDir};
use seam::ops::time::WallClock;
use seam::prelude::*;

/// Marks a snapshot as complete. Written last, atomically.
const MANIFEST: &str = "MANIFEST";

/// Everything the backup tool may do. Reading this trait is an audit of the
/// program: it can read its arguments and the clock, list, stat, create,
/// copy and delete files, write files atomically, and print. It cannot touch
/// the network, run commands, or read environment variables.
///
/// `F` is the backend's atomic file type, `D` its directory listing type and
/// `O` its standard output type; all are inferred from the backend.
trait BackupEnv<F: AtomicWrite, D: Listing, O: Output>:
    Inspect<Args>
    + Inspect<WallClock>
    + for<'a> Provide<ReadDir<'a, D>>
    + for<'a> Inspect<Metadata<'a>>
    + for<'a> Perform<CreateDir<'a>>
    + for<'a> Perform<CopyFile<'a>>
    + for<'a> Perform<RemoveDir<'a>>
    + for<'a> Provide<CreateAtomic<'a, F>>
    + Provide<OpenStdout<O>>
{
}

impl<T, F: AtomicWrite, D: Listing, O: Output> BackupEnv<F, D, O> for T where
    T: Inspect<Args>
        + Inspect<WallClock>
        + for<'a> Provide<ReadDir<'a, D>>
        + for<'a> Inspect<Metadata<'a>>
        + for<'a> Perform<CreateDir<'a>>
        + for<'a> Perform<CopyFile<'a>>
        + for<'a> Perform<RemoveDir<'a>>
        + for<'a> Provide<CreateAtomic<'a, F>>
        + Provide<OpenStdout<O>>
{
}

/// What the backup tool does with an atomic file: write to it.
trait AtomicWrite: for<'a> Perform<Write<'a>> {}
impl<F: for<'a> Perform<Write<'a>>> AtomicWrite for F {}

/// What the backup tool does with standard output: write to it.
trait Output: for<'a> Perform<Write<'a>> {}
impl<O: for<'a> Perform<Write<'a>>> Output for O {}

/// What the backup tool does with a directory listing: step through it.
trait Listing: Perform<NextEntry> {}
impl<D: Perform<NextEntry>> Listing for D {}

/// Parses the arguments and runs one backup.
fn run<F: AtomicWrite, D: Listing, O: Output>(env: &mut impl BackupEnv<F, D, O>) -> io::Result<()> {
    let args = env.inspect(Args);
    let usage = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: backup <source> <destination> [keep]",
        )
    };
    let [_, source, dest, rest @ ..] = args.as_slice() else {
        return Err(usage());
    };
    let keep = match rest {
        [] => 5,
        [n] => n
            .to_str()
            .and_then(|n| n.parse().ok())
            .filter(|&n| n > 0)
            .ok_or_else(usage)?,
        _ => return Err(usage()),
    };
    let snapshot = backup(env, Path::new(source), Path::new(dest), keep)?;
    say(env, &format!("snapshot {}\n", snapshot.display()))
}

/// Copies `source` into a new snapshot under `dest`, then prunes old ones.
///
/// A snapshot counts only once its manifest exists. The manifest is written
/// last, through an atomic file acquired for a scope, so it exists completely or not
/// at all: a run that crashes, fails or is killed partway leaves a snapshot
/// with no manifest, which the next run removes.
fn backup<F: AtomicWrite, D: Listing, O: Output>(
    env: &mut impl BackupEnv<F, D, O>,
    source: &Path,
    dest: &Path,
    keep: usize,
) -> io::Result<PathBuf> {
    if !env.inspect(Metadata::new(source))?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "source is not a directory",
        ));
    }
    let now = env
        .inspect(WallClock)
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("the clock is set before 1970"))?;
    // Zero-padded, so that sorting names sorts snapshots by time.
    let snapshot = dest.join(format!("{:020}", now.as_secs()));
    create_dir_all(env, &snapshot)?;
    let mut copied = Vec::new();
    copy_tree(env, source, &snapshot, Path::new(""), &mut copied)?;

    env.acquire(
        CreateAtomic::new(&snapshot.join(MANIFEST)),
        |_, manifest| {
            for (path, bytes) in &copied {
                let line = format!("{bytes}\t{}\n", path.display());
                write_all(manifest, line.as_bytes())?;
            }
            io::Result::Ok(())
        },
    )?;

    let mut complete = env.acquire(ReadDir::new(dest), |env, snapshots| {
        let mut complete = Vec::new();
        while let Some(dir) = snapshots.perform(NextEntry)? {
            if env.inspect(Metadata::new(&dir.join(MANIFEST))).is_ok() {
                complete.push(dir);
            } else {
                env.perform(RemoveDir::all(&dir))?;
                say(env, &format!("removed incomplete {}\n", dir.display()))?;
            }
        }
        io::Result::Ok(complete)
    })?;
    complete.sort();
    let excess = complete.len().saturating_sub(keep);
    for old in &complete[..excess] {
        env.perform(RemoveDir::all(old))?;
        say(env, &format!("pruned {}\n", old.display()))?;
    }
    Ok(snapshot)
}

/// Copies the tree under `from` to `to`, recording each file's path
/// (relative to the snapshot) and size.
fn copy_tree<F: AtomicWrite, D: Listing, O: Output>(
    env: &mut impl BackupEnv<F, D, O>,
    from: &Path,
    to: &Path,
    rel: &Path,
    copied: &mut Vec<(PathBuf, u64)>,
) -> io::Result<()> {
    // One entry at a time: the listing holds no more than the next path.
    env.acquire(ReadDir::new(from), |env, entries| {
        while let Some(entry) = entries.perform(NextEntry)? {
            let name = entry.file_name().expect("directory entries have names");
            let target = to.join(name);
            if env.inspect(Metadata::new(&entry))?.is_dir() {
                env.perform(CreateDir::new(&target))?;
                copy_tree(env, &entry, &target, &rel.join(name), copied)?;
            } else {
                let bytes = env.perform(CopyFile::new(&entry, &target))?;
                copied.push((rel.join(name), bytes));
            }
        }
        Ok(())
    })
}

fn say<F: AtomicWrite, D: Listing, O: Output>(
    env: &mut impl BackupEnv<F, D, O>,
    text: &str,
) -> io::Result<()> {
    print(env, text.as_bytes())
}

fn main() -> io::Result<()> {
    run(&mut seam::stdlib::Std)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use seam::test::Sim;

    use super::*;

    fn world() -> Sim {
        let mut sim = Sim::new();
        sim.add_file("/home/notes/todo.txt", "buy milk")
            .add_file("/home/notes/ideas/seam.md", "sans-effect")
            .set_args(["backup", "/home/notes", "/backups", "2"])
            .set_wall_clock(UNIX_EPOCH + Duration::from_secs(1_000));
        sim
    }

    #[test]
    fn copies_the_whole_tree_and_writes_a_manifest() {
        let mut sim = world();
        run(&mut sim).unwrap();
        let snap = "/backups/00000000000000001000";
        assert_eq!(sim.file(format!("{snap}/todo.txt")).unwrap(), b"buy milk");
        assert_eq!(
            sim.file(format!("{snap}/ideas/seam.md")).unwrap(),
            b"sans-effect"
        );
        assert_eq!(
            String::from_utf8(sim.file(format!("{snap}/MANIFEST")).unwrap()).unwrap(),
            "11\tideas/seam.md\n8\ttodo.txt\n"
        );
        assert_eq!(sim.stdout(), format!("snapshot {snap}\n").as_bytes());
    }

    /// A run that died before writing its manifest left a partial snapshot
    /// behind. The next run removes it, and it never counted toward `keep`.
    #[test]
    fn removes_snapshots_from_crashed_runs() {
        let mut sim = world();
        sim.add_file("/backups/00000000000000000500/todo.txt", "half a cop");
        run(&mut sim).unwrap();
        assert!(!sim.exists("/backups/00000000000000000500"));
        assert!(sim.exists("/backups/00000000000000001000/MANIFEST"));
        assert!(
            String::from_utf8_lossy(&sim.stdout())
                .contains("removed incomplete /backups/00000000000000000500")
        );
    }

    #[test]
    fn keeps_only_the_newest() {
        let mut sim = world();
        for _ in 0..3 {
            run(&mut sim).unwrap();
            sim.advance(Duration::from_secs(3600));
        }
        let snaps = sim
            .acquire(ReadDir::new("/backups"), |_, d| {
                std::iter::from_fn(|| d.perform(NextEntry).transpose())
                    .collect::<io::Result<Vec<_>>>()
            })
            .unwrap();
        assert_eq!(
            snaps,
            [
                PathBuf::from("/backups/00000000000000004600"),
                PathBuf::from("/backups/00000000000000008200")
            ]
        );
        assert!(
            String::from_utf8_lossy(&sim.stdout()).contains("pruned /backups/00000000000000001000")
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        let mut sim = world();
        sim.set_args(["backup", "/home/notes"]);
        assert_eq!(
            run(&mut sim).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        sim.set_args(["backup", "/home/notes/todo.txt", "/backups"]);
        assert_eq!(
            run(&mut sim).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert!(!sim.exists("/backups"), "nothing was written");
    }
}
