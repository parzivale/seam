//! What a step changes in a directory (`seam::notify`): watch it, run the
//! step, then report every change until the directory has been quiet for
//! a while.
//!
//! The core watches through `Watch` and waits with `Timeout<NextEvent>`,
//! so the program uses the OS's watcher while the tests use `Polling` over
//! a simulation, where the quiet period passes in virtual time, at once.
//!
//! ```text
//! cargo run -p seam-rs --example changes --features std,test,notify
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use seam::io;
use seam::notify::{EventKind, NextEvent, Watch};
use seam::ops::time::Timeout;
use seam::prelude::*;

/// Watches `dir`, runs `step`, then collects each change until none comes
/// for `quiet`: as kind and path.
fn changes<C, W>(
    c: &mut C,
    dir: &Path,
    quiet: Duration,
    step: impl FnOnce(&mut C) -> io::Result<()>,
) -> io::Result<Vec<(EventKind, PathBuf)>>
where
    C: for<'a> Provide<Watch<'a, W>>,
    W: Perform<Timeout<NextEvent>>,
{
    c.acquire(Watch::recursive(dir), |c, watcher| {
        step(c)?;
        let mut seen = Vec::new();
        loop {
            match watcher.perform(Timeout::new(quiet, NextEvent)) {
                Ok(event) => seen.extend(event.paths.into_iter().map(|p| (event.kind, p))),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => return Ok(seen),
                Err(e) => return Err(e),
            }
        }
    })
}

/// The edge: the OS's watcher, and std's files for the step.
fn main() -> io::Result<()> {
    use seam::effects as fx;
    use seam::ops::fs::{WriteOnly, write_file};
    let dir = std::env::temp_dir().join(format!("seam-changes-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut root = seam::root![
        seam::stdlib::Std<fx::Open<WriteOnly>>,
        seam::notify::Notify<seam::notify::effects::Watch>::new(),
    ];
    let seen = changes(&mut root, &dir, Duration::from_millis(500), |root| {
        write_file(root, &dir.join("report.txt"), "done\n")?;
        write_file(root, &dir.join("report.txt"), "done, again\n")
    })?;
    for (kind, path) in seen {
        println!("{kind:?}  {}", path.display());
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::effects as fx;
    use seam::notify::{CreateKind, ModifyKind, Polling};
    use seam::ops::fs::{WriteOnly, write_file};
    use seam::test::Sim;

    use super::*;

    #[test]
    fn a_step_that_writes_two_files() {
        let mut sim = Sim::new();
        sim.add_dir("/site").add_file("/site/index.html", "old");
        let mut root = seam::root![
            Sim<fx::View, fx::Open<WriteOnly>, fx::Metadata>::clone(&sim),
            Polling<seam::notify::effects::Watch>::new(sim.clone()),
        ];
        let seen = changes(
            &mut root,
            Path::new("/site"),
            Duration::from_secs(5),
            |root| {
                // `Polling`, like notify's own poll watcher, sees a change
                // by size or modification time, and the virtual clock has
                // not moved: so this rewrite changes the size.
                write_file(root, "/site/index.html", "new page")?;
                write_file(root, "/site/about.html", "hello")
            },
        )
        .unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen.contains(&(
            EventKind::Create(CreateKind::File),
            "/site/about.html".into()
        )));
        assert!(
            seen.iter()
                .any(|(k, p)| matches!(k, EventKind::Modify(ModifyKind::Data(_)))
                    && p == Path::new("/site/index.html")),
            "{seen:?}"
        );
    }

    #[test]
    fn a_step_that_changes_nothing() {
        let mut sim = Sim::new();
        sim.add_dir("/site");
        let mut root = seam::root![
            Sim<fx::View, fx::Metadata>::clone(&sim),
            Polling<seam::notify::effects::Watch>::new(sim.clone()),
        ];
        let seen = changes(
            &mut root,
            Path::new("/site"),
            Duration::from_secs(5),
            |_| Ok(()),
        )
        .unwrap();
        assert!(seen.is_empty());
    }
}
