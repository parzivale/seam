//! Snapshot a directory, keeping the newest few snapshots.
//!
//! ```text
//! backup <source> <destination> [keep]
//! cargo run -p seam --example backup --features std,test,fs,env,time,io-std -- src /tmp/backups 3
//! ```
//!
//! Each run copies `source` into `destination/<unix seconds>/` and deletes
//! the oldest snapshots beyond `keep` (default 5).
//!
//! The core needs many capabilities, so it names them once with a bundling
//! trait, [`BackupEnv`], instead of repeating a long `where` clause on every
//! function.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use seam::ops::env::Args;
use seam::ops::fs::{CopyFile, CreateDir, Metadata, ReadDir, RemoveDir};
use seam::ops::stdio::Print;
use seam::ops::time::WallClock;
use seam::prelude::*;

/// Everything the backup tool may do. Reading this trait is an audit of the
/// program: it can read its arguments and the clock, list, stat, create,
/// copy and delete files, and print. It cannot touch the network, run
/// commands, or read environment variables.
trait BackupEnv:
    Inspect<Args>
    + Inspect<WallClock>
    + Inspect<ReadDir>
    + Inspect<Metadata>
    + Perform<CreateDir>
    + Perform<CopyFile>
    + Perform<RemoveDir>
    + for<'a> Perform<Print<'a>>
{
}

impl<T> BackupEnv for T where
    T: Inspect<Args>
        + Inspect<WallClock>
        + Inspect<ReadDir>
        + Inspect<Metadata>
        + Perform<CreateDir>
        + Perform<CopyFile>
        + Perform<RemoveDir>
        + for<'a> Perform<Print<'a>>
{
}

/// Parses the arguments and runs one backup.
fn run(env: &mut impl BackupEnv) -> io::Result<()> {
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
fn backup(
    env: &mut impl BackupEnv,
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
    copy_tree(env, source, &snapshot)?;

    let mut snapshots = env.inspect(ReadDir::new(dest))?;
    snapshots.sort();
    let excess = snapshots.len().saturating_sub(keep);
    for old in &snapshots[..excess] {
        env.perform(RemoveDir::all(old))?;
        say(env, &format!("pruned {}\n", old.display()))?;
    }
    Ok(snapshot)
}

fn copy_tree(env: &mut impl BackupEnv, from: &Path, to: &Path) -> io::Result<()> {
    for entry in env.inspect(ReadDir::new(from))? {
        let target = to.join(entry.file_name().expect("directory entries have names"));
        if env.inspect(Metadata::new(&entry))?.is_dir() {
            env.perform(CreateDir::new(&target))?;
            copy_tree(env, &entry, &target)?;
        } else {
            env.perform(CopyFile::new(&entry, &target))?;
        }
    }
    Ok(())
}

fn say(env: &mut impl BackupEnv, text: &str) -> io::Result<()> {
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
    fn copies_the_whole_tree() {
        let mut sim = world();
        run(&mut sim).unwrap();
        let snap = "/backups/00000000000000001000";
        assert_eq!(sim.file(format!("{snap}/todo.txt")).unwrap(), b"buy milk");
        assert_eq!(
            sim.file(format!("{snap}/ideas/seam.md")).unwrap(),
            b"sans-effect"
        );
        assert_eq!(sim.stdout(), format!("snapshot {snap}\n").as_bytes());
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
