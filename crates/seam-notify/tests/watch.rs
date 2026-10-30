//! Watching, by polling the simulation and with the OS's own events.

use core::time::Duration;
use std::path::PathBuf;

use seam_core::effects as fx;
use seam_core::io::{self, ErrorKind};
use seam_core::ops::fs::{RemoveFile, WriteOnly, write_file};
use seam_core::ops::time::{Now, Timeout};
use seam_core::prelude::*;
use seam_notify::{
    CreateKind, DataChange, EventKind, EventKindMask, ModifyKind, NextEvent, Polling, RemoveKind,
    Watch,
};
use seam_test::Sim;

fn sim() -> Sim {
    let mut sim = Sim::new();
    sim.add_file("/w/a", "1")
        .add_dir("/w/sub")
        .add_file("/other", "");
    sim
}

#[test]
fn polling_reports_creates_modifies_and_removes() -> io::Result<()> {
    let sim = sim();
    let mut root = seam_core::root![
        Sim<fx::View, fx::Open<WriteOnly>, fx::Metadata, fx::RemoveFile>::clone(&sim),
        Polling<seam_notify::effects::Watch>::new(sim.clone()),
    ];
    root.acquire(Watch::recursive("/w"), |root, watcher| {
        write_file(root, "/w/sub/new", "x")?;
        let created = watcher.perform(NextEvent)?;
        assert_eq!(created.kind, EventKind::Create(CreateKind::File));
        assert_eq!(created.paths, vec![PathBuf::from("/w/sub/new")]);
        write_file(root, "/w/a", "longer")?;
        root.perform(RemoveFile::new("/w/sub/new"))?;
        // One rescan sees both: the data change, then the removal.
        let kinds = [
            watcher.perform(NextEvent)?.kind,
            watcher.perform(NextEvent)?.kind,
        ];
        assert_eq!(
            kinds,
            [
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                EventKind::Remove(RemoveKind::File)
            ]
        );
        // Outside the watched tree: nothing.
        write_file(root, "/other", "changed")?;
        let quiet = watcher.perform(Timeout::new(Duration::from_secs(5), NextEvent));
        assert_eq!(quiet.unwrap_err().kind(), ErrorKind::TimedOut);
        Ok(())
    })
}

#[test]
fn a_timeout_waits_on_the_virtual_clock() -> io::Result<()> {
    let sim = sim();
    let start = sim.inspect(Now);
    let mut polling = Polling::every(sim.clone(), Duration::from_millis(250));
    polling.acquire(Watch::flat("/w"), |_, watcher| {
        let quiet = watcher.perform(Timeout::new(Duration::from_secs(10), NextEvent));
        assert_eq!(quiet.unwrap_err().kind(), ErrorKind::TimedOut);
        io::Result::Ok(())
    })?;
    assert_eq!(
        sim.inspect(Now).duration_since(start),
        Duration::from_secs(10)
    );
    Ok(())
}

#[test]
fn a_flat_watch_ignores_deeper_changes() -> io::Result<()> {
    let mut sim = sim();
    let mut polling = Polling::new(sim.clone());
    polling.acquire(Watch::flat("/w"), |_, watcher| {
        sim.add_file("/w/sub/deep", "");
        sim.add_file("/w/top", "");
        let event = watcher.perform(NextEvent)?;
        assert_eq!(event.paths, [PathBuf::from("/w/top")]);
        Ok(())
    })
}

#[test]
fn watching_a_missing_path_fails() {
    let mut polling = Polling::new(sim());
    let missing = polling.acquire(Watch::recursive("/nowhere"), |_, _| io::Result::Ok(()));
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
}

/// The real thing: inotify (or the platform's own) on a temporary
/// directory.
#[cfg(feature = "notify")]
#[test]
fn the_os_reports_a_new_file() -> io::Result<()> {
    use seam_notify::Notify;

    let dir = std::env::temp_dir().join(format!("seam-notify-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(io::Error::from)?;
    let result = Notify::new().acquire(Watch::recursive(&dir), |_, watcher| {
        std::fs::write(dir.join("hello"), "hi").map_err(io::Error::from)?;
        loop {
            let event = watcher.perform(Timeout::new(Duration::from_secs(10), NextEvent))?;
            if event.kind.is_create() {
                return io::Result::Ok(event.paths);
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result?.iter().any(|p| p.ends_with("hello")));
    Ok(())
}

/// The async form waits without blocking its executor: on one thread, the
/// task writing the file runs while the watch waits.
#[cfg(feature = "notify")]
#[test]
fn async_waits_leave_the_executor_free() -> io::Result<()> {
    use seam_notify::Notify;

    let dir = std::env::temp_dir().join(format!("seam-notify-async-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(io::Error::from)?;
    let ex = smol::LocalExecutor::new();
    let result = smol::block_on(ex.run(async {
        let writer = ex.spawn({
            let dir = dir.clone();
            async move {
                smol::Timer::after(Duration::from_millis(50)).await;
                std::fs::write(dir.join("hello"), "hi").unwrap();
            }
        });
        let seen = Notify::new()
            .acquire_async(Watch::recursive(&dir), async |_, watcher| {
                loop {
                    let event = watcher
                        .perform_async(Timeout::new(Duration::from_secs(10), NextEvent))
                        .await?;
                    if event.kind.is_create() {
                        return io::Result::Ok(event.paths);
                    }
                }
            })
            .await;
        writer.await;
        // And a timeout with nothing happening times out.
        let quiet = Notify::new()
            .acquire_async(Watch::recursive(&dir), async |_, watcher| {
                watcher
                    .perform_async(Timeout::new(Duration::from_millis(100), NextEvent))
                    .await
                    .map(drop)
            })
            .await;
        assert_eq!(quiet.unwrap_err().kind(), io::ErrorKind::TimedOut);
        seen
    }));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(result?.iter().any(|p| p.ends_with("hello")));
    Ok(())
}

#[test]
fn a_watch_reports_only_its_kinds() -> io::Result<()> {
    let sim = sim();
    let mut root = seam_core::root![
        Sim<fx::View, fx::Open<WriteOnly>, fx::Metadata, fx::RemoveFile>::clone(&sim),
        Polling<seam_notify::effects::Watch>::new(sim.clone()),
    ];
    root.acquire(
        Watch::recursive("/w").kinds(EventKindMask::REMOVE),
        |root, watcher| {
            write_file(root, "/w/sub/new", "x")?;
            root.perform(RemoveFile::new("/w/a"))?;
            // The creation is not reported; the removal is.
            let event = watcher.perform(NextEvent)?;
            assert_eq!(event.kind, EventKind::Remove(RemoveKind::File));
            assert_eq!(event.paths, vec![PathBuf::from("/w/a")]);
            Ok(())
        },
    )
}

/// A rewrite that keeps the size, at the same (virtual) time: unseen by
/// size and time, seen once contents are compared.
#[test]
fn comparing_contents_sees_a_same_size_rewrite() -> io::Result<()> {
    fn rewrite<C, W>(root: &mut C) -> io::Result<Option<EventKind>>
    where
        C: for<'a> Provide<Watch<'a, W>>
            + for<'a> Provide<seam_core::ops::fs::Open<'a, WriteOnly, seam_test::fs::File<WriteOnly>>>,
        W: Perform<Timeout<NextEvent>>,
    {
        root.acquire(Watch::recursive("/w"), |root, watcher| {
            write_file(root, "/w/a", "2")?;
            match watcher.perform(Timeout::new(Duration::from_secs(5), NextEvent)) {
                Ok(event) => Ok(Some(event.kind)),
                Err(e) if e.kind() == ErrorKind::TimedOut => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    let sim = sim();
    let mut by_metadata = seam_core::root! {
        Sim<fx::Open<WriteOnly>>::clone(&sim),
        Polling<seam_notify::effects::Watch>::new(sim.clone()),
    };
    assert_eq!(rewrite(&mut by_metadata)?, None);

    let mut by_contents = seam_core::root! {
        Sim<fx::Open<WriteOnly>>::clone(&sim),
        Polling<seam_notify::effects::Watch>::comparing_contents(sim.clone()),
    };
    assert_eq!(
        rewrite(&mut by_contents)?,
        Some(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
    );
    Ok(())
}
