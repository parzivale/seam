//! Sets up a directory a team shares (`seam::unix`): made group-writable
//! with the set-group-ID bit, handed to the team's group when we can, and
//! linked from a well-known path.
//!
//! The core asks for `Chmod`, `Chown`, `Symlink` and the user's `Ids`, so
//! the program performs them with rustix while the tests use `SimUnix`,
//! which checks permissions as Unix does: only root changes owners.
//!
//! ```text
//! cargo run -p seam-rs --example shared_dir --features std,test,rustix -- /tmp/team
//! ```

use std::path::{Path, PathBuf};

use seam::io;
use seam::ops::fs::{CreateDir, Metadata, create_dir_all};
use seam::prelude::*;
use seam::unix::{Chmod, Chown, Hostname, Ids, ReadLink, Symlink};

/// rwxrwsr-x: the group writes, and new files inherit the group.
const MODE: u32 = 0o2775;

/// What [`share`] did.
#[derive(Debug, PartialEq, Eq)]
struct Shared {
    dir: PathBuf,
    /// Whether the group was handed over (only root can).
    regrouped: bool,
    /// Where the link points.
    link: PathBuf,
    host: String,
}

/// Makes `dir`, shares it with group `gid`, and links `link` to it (or
/// leaves a link that is already there).
fn share<C>(c: &mut C, dir: &Path, gid: u32, link: &Path) -> io::Result<Shared>
where
    C: for<'a> Perform<CreateDir<'a>>
        + for<'a> Inspect<Metadata<'a>>
        + for<'a> Perform<Chmod<'a>>
        + for<'a> Perform<Chown<'a>>
        + for<'a> Perform<Symlink<'a>>
        + for<'a> Inspect<ReadLink<'a>>
        + Inspect<Ids>
        + Inspect<Hostname>,
{
    create_dir_all(c, dir)?;
    c.perform(Chmod::new(dir, MODE))?;
    let regrouped = c.inspect(Ids).is_root();
    if regrouped {
        c.perform(Chown::new(dir, None, Some(gid)))?;
    }
    match c.perform(Symlink::new(dir, link)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Ok(Shared {
        dir: dir.to_owned(),
        regrouped,
        link: c.inspect(ReadLink::new(link))?,
        host: c.inspect(Hostname)?,
    })
}

/// The edge: std's filesystem, rustix's Unix calls.
fn main() -> io::Result<()> {
    use seam::effects as fx;
    use seam::unix::effects as ux;
    let mut root = seam::root! {
        seam::stdlib::Std<fx::CreateDir, fx::Metadata, fx::Args>,
        seam::rustix::Rustix<ux::Chmod, ux::Chown, ux::Symlink, ux::ReadLink, ux::Ids, ux::Hostname>,
    };
    let args = root.inspect(seam::ops::env::Args);
    let dir = PathBuf::from(args.get(1).map_or("/tmp/seam-shared".into(), |a| a.clone()));
    let link = dir.with_extension("latest");
    let shared = share(&mut root, &dir, 100, &link)?;
    println!("{shared:#?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::effects as fx;
    use seam::test::Sim;
    use seam::unix::SimUnix;
    use seam::unix::effects as ux;

    use super::*;

    /// A machine with `/srv`, as user `uid`. What the core creates goes
    /// through `SimUnix`, so it belongs to the user who made it.
    fn machine(uid: u32) -> (SimUnix<Sim>, impl FnMut(&Path, &Path) -> io::Result<Shared>) {
        let mut sim = Sim::new();
        sim.add_dir("/srv");
        let mut unix = SimUnix::new(sim.clone());
        unix.set_user(uid, uid).set_hostname("build-box");
        let mut root = seam::root! {
            Sim<fx::Metadata>::clone(&sim),
            SimUnix<fx::CreateDir, ux::Chmod, ux::Chown, ux::Symlink, ux::ReadLink, ux::Ids, ux::Hostname>::clone(&unix),
        };
        (unix, move |dir, link| share(&mut root, dir, 100, link))
    }

    #[test]
    fn as_root_the_group_is_handed_over() {
        let (unix, mut share) = machine(0);
        let shared = share(Path::new("/srv/team"), Path::new("/srv/latest")).unwrap();
        assert_eq!(
            shared,
            Shared {
                dir: "/srv/team".into(),
                regrouped: true,
                link: "/srv/team".into(),
                host: "build-box".into(),
            }
        );
        assert_eq!(unix.mode("/srv/team"), Some(MODE));
        assert_eq!(unix.owner("/srv/team"), Some((0, 100)));
        // Run again: the link is already there, and that is fine.
        assert!(share(Path::new("/srv/team"), Path::new("/srv/latest")).is_ok());
    }

    #[test]
    fn as_a_user_the_group_stays() {
        let (unix, mut share) = machine(1000);
        let shared = share(Path::new("/srv/team"), Path::new("/srv/latest")).unwrap();
        assert!(!shared.regrouped);
        // The user made it, so it is theirs, and they set its mode.
        assert_eq!(unix.owner("/srv/team"), Some((1000, 1000)));
        assert_eq!(unix.mode("/srv/team"), Some(MODE));
    }
}
