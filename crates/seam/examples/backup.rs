//! Snapshot a directory, keeping the newest few snapshots.
//!
//! ```text
//! backup <source> <destination> [keep]
//! cargo run -p seam --example backup --features std,test,fs,env,time,io-std -- src /tmp/backups 3
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

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use seam::ops::env::Args;
use seam::ops::fs::{CopyFile, CreateAtomic, CreateDir, Metadata, ReadDir, RemoveDir};
use seam::ops::io::WriteAll;
use seam::ops::stdio::Print;
use seam::ops::time::WallClock;
use seam::prelude::*;

/// Marks a snapshot as complete. Written last, atomically.
const MANIFEST: &str = "MANIFEST";

/// Everything the backup tool may do. Reading this trait is an audit of the
/// program: it can read its arguments and the clock, list, stat, create,
/// copy and delete files, write files atomically, and print. It cannot touch
/// the network, run commands, or read environment variables.
///
/// `F` is the backend's atomic file type; it is inferred from the backend.
trait BackupEnv<F: AtomicWrite>:
    Inspect<Args>
    + Inspect<WallClock>
    + Inspect<ReadDir>
    + Inspect<Metadata>
    + Perform<CreateDir>
    + Perform<CopyFile>
    + Perform<RemoveDir>
    + Provide<CreateAtomic<F>>
    + for<'a> Perform<Print<'a>>
{
}

impl<T, F: AtomicWrite> BackupEnv<F> for T where
    T: Inspect<Args>
        + Inspect<WallClock>
        + Inspect<ReadDir>
        + Inspect<Metadata>
        + Perform<CreateDir>
        + Perform<CopyFile>
        + Perform<RemoveDir>
        + Provide<CreateAtomic<F>>
        + for<'a> Perform<Print<'a>>
{
}

/// What the backup tool does with an atomic file: write to it.
trait AtomicWrite: for<'a> Perform<WriteAll<'a>> {}
impl<F: for<'a> Perform<WriteAll<'a>>> AtomicWrite for F {}

/// Parses the arguments and runs one backup.
fn run<F: AtomicWrite>(env: &mut impl BackupEnv<F>) -> io::Result<()> {
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
fn backup<F: AtomicWrite>(
    env: &mut impl BackupEnv<F>,
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
        .map_err(io::Error::other)?;
    // Zero-padded, so that sorting names sorts snapshots by time.
    let snapshot = dest.join(format!("{:020}", now.as_secs()));
    env.perform(CreateDir::all(&snapshot))?;
    let mut copied = Vec::new();
    copy_tree(env, source, &snapshot, Path::new(""), &mut copied)?;

    env.acquire(CreateAtomic::new(snapshot.join(MANIFEST)), |_, manifest| {
        for (path, bytes) in &copied {
            let line = format!("{bytes}\t{}\n", path.display());
            manifest.perform(WriteAll(line.as_bytes()))?;
        }
        io::Result::Ok(())
    })?;

    let mut complete = Vec::new();
    for dir in env.inspect(ReadDir::new(dest))? {
        if env.inspect(Metadata::new(dir.join(MANIFEST))).is_ok() {
            complete.push(dir);
        } else {
            env.perform(RemoveDir::all(&dir))?;
            say(env, &format!("removed incomplete {}\n", dir.display()))?;
        }
    }
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
fn copy_tree<F: AtomicWrite>(
    env: &mut impl BackupEnv<F>,
    from: &Path,
    to: &Path,
    rel: &Path,
    copied: &mut Vec<(PathBuf, u64)>,
) -> io::Result<()> {
    for entry in env.inspect(ReadDir::new(from))? {
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
}

fn say<F: AtomicWrite>(env: &mut impl BackupEnv<F>, text: &str) -> io::Result<()> {
    env.perform(Print(text.as_bytes()))
}

fn main() -> io::Result<()> {
    run(&mut seam::stdlib::Std::new())
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
            String::from_utf8_lossy(sim.stdout())
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
        let snaps = sim.inspect(ReadDir::new("/backups")).unwrap();
        assert_eq!(
            snaps,
            [
                PathBuf::from("/backups/00000000000000004600"),
                PathBuf::from("/backups/00000000000000008200")
            ]
        );
        assert!(
            String::from_utf8_lossy(sim.stdout()).contains("pruned /backups/00000000000000001000")
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
