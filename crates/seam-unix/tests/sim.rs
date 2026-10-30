//! The simulated Unix.

use std::path::PathBuf;

use seam_core::io::ErrorKind;
use seam_core::prelude::*;
use seam_test::Sim;
use seam_unix::{
    Chmod, Chown, Hostname, Ids, Kill, Mount, MountFlags, Mounted, ProcessId, ReadLink, SetUmask,
    SimUnix, Symlink, Unmount, UserIds, signals,
};

fn unix() -> SimUnix<Sim> {
    let mut sim = Sim::new();
    sim.add_dir("/data")
        .add_dir("/mnt")
        .add_file("/etc/app.conf", "");
    SimUnix::new(sim)
}

#[test]
fn mounts_as_root_only() {
    let mut u = unix();
    u.perform(Mount::bind("/data", "/mnt").flags(MountFlags::RDONLY))
        .unwrap();
    assert_eq!(
        u.mounts(),
        [Mounted {
            source: Some(PathBuf::from("/data")),
            target: PathBuf::from("/mnt"),
            fstype: None,
            flags: MountFlags::BIND | MountFlags::RDONLY,
        }]
    );
    let missing = u.perform(Mount::bind("/nowhere", "/mnt"));
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);
    u.perform(Mount::new("tmpfs", "/mnt", "tmpfs")).unwrap();
    // Unmounting takes the latest mount at the target.
    u.perform(Unmount::new("/mnt")).unwrap();
    assert_eq!(u.mounts().len(), 1);
    u.perform(Unmount::new("/mnt")).unwrap();
    assert_eq!(
        u.perform(Unmount::new("/mnt")).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    u.set_user(1000, 1000);
    assert_eq!(
        u.perform(Mount::bind("/data", "/mnt")).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
}

#[test]
fn permissions_and_owners() {
    let mut u = unix();
    u.perform(Chmod::new("/etc/app.conf", 0o100640)).unwrap();
    assert_eq!(u.mode("/etc/app.conf"), Some(0o640));
    u.perform(Chown::new("/etc/app.conf", Some(1000), None))
        .unwrap();
    assert_eq!(u.owner("/etc/app.conf"), Some((1000, 0)));

    // The owner may change the mode, but not the owner.
    u.set_user(1000, 1000);
    u.perform(Chmod::new("/etc/app.conf", 0o600)).unwrap();
    assert_eq!(
        u.perform(Chown::new("/etc/app.conf", Some(0), None))
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    );
    u.set_user(2000, 2000);
    assert_eq!(
        u.perform(Chmod::new("/etc/app.conf", 0o777))
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        u.perform(Chmod::new("/missing", 0o777)).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn links_ids_and_signals() {
    let mut u = unix();
    u.perform(Symlink::new("/etc/app.conf", "/etc/current"))
        .unwrap();
    assert_eq!(
        u.inspect(ReadLink::new("/etc/current")).unwrap(),
        PathBuf::from("/etc/app.conf")
    );
    assert_eq!(
        u.perform(Symlink::new("/x", "/etc/current"))
            .unwrap_err()
            .kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        u.inspect(ReadLink::new("/etc/app.conf"))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        u.inspect(ReadLink::new("/nope")).unwrap_err().kind(),
        ErrorKind::NotFound
    );

    u.set_user(1000, 100)
        .set_hostname("box")
        .set_pid(42)
        .add_process(7);
    assert_eq!(
        u.inspect(Ids),
        UserIds {
            uid: 1000,
            gid: 100,
            euid: 1000,
            egid: 100
        }
    );
    assert_eq!(u.inspect(Hostname).unwrap(), "box");
    assert_eq!(u.inspect(ProcessId), 42);

    u.perform(Kill { pid: 7, signal: 0 }).unwrap();
    u.perform(Kill {
        pid: 7,
        signal: signals::SIGTERM,
    })
    .unwrap();
    assert_eq!(u.signals(), [(7, signals::SIGTERM)]);
    // Terminated: it is gone.
    assert_eq!(
        u.perform(Kill { pid: 7, signal: 0 }).unwrap_err().kind(),
        ErrorKind::NotFound
    );

    assert_eq!(u.perform(SetUmask(0o077)), 0o022);
    assert_eq!(u.perform(SetUmask(0o022)), 0o077);
}

#[cfg(feature = "test")]
#[test]
fn world_files() {
    use seam_unix::effects as ux;
    let sim = Sim::new();
    let mut root = seam_core::root! {
        SimUnix<ux::Mount, ux::Unmount, ux::Ids, ux::Hostname, ux::Kill>::new(sim)
    };
    seam_test::load_world::<(ux::Mount,), _>(
        &mut root,
        r#"unix {
            user uid=1000 gid=1000
            hostname "build-box"
            process 77
            mount "/mnt" source="/data" flags="bind,rdonly"
        }"#,
    )
    .unwrap();
    assert_eq!(root.inspect(Hostname).unwrap(), "build-box");
    assert!(!root.inspect(Ids).is_root());
    root.perform(Kill {
        pid: 77,
        signal: signals::SIGHUP,
    })
    .unwrap();
    seam_test::check_world::<(ux::Mount,), _>(
        &root,
        r#"unix { signal 77 1; mount "/mnt" source="/data" flags="bind,rdonly" }"#,
    )
    .unwrap();
    assert!(seam_test::check_world::<(ux::Mount,), _>(&root, r#"unix { signal 77 9 }"#).is_err());

    let text = seam_test::to_world::<(ux::Mount,), _>(&root);
    let mut again = seam_core::root! {SimUnix<ux::Mount, ux::Hostname>::new(Sim::new())};
    seam_test::load_world::<(ux::Mount,), _>(&mut again, &text).unwrap();
    assert_eq!(
        again.part::<ux::Mount>().mounts(),
        root.part::<ux::Mount>().mounts()
    );
}

/// What a user makes through `SimUnix` is theirs, with the umask's mode,
/// so they may change its mode; what they did not make is root's.
#[test]
fn what_a_user_makes_is_theirs() {
    use seam_core::ops::fs::{WriteOnly, create_dir_all, write_file};
    use seam_unix::{SetUmask, effects as ux};

    let mut sim = Sim::new();
    sim.add_dir("/home").add_file("/etc/passwd", "root:x:0:0");
    let mut unix = SimUnix::new(sim.clone());
    unix.set_user(1000, 100);
    let mut root = seam_core::root! {
        Sim<seam_core::effects::Metadata>::clone(&sim),
        SimUnix<seam_core::effects::CreateDir, seam_core::effects::Open<WriteOnly>, ux::Chmod, ux::SetUmask>::clone(&unix),
    };
    root.perform(SetUmask(0o027));
    create_dir_all(&mut root, "/home/ada/notes").unwrap();
    write_file(&mut root, "/home/ada/notes/todo.txt", "milk").unwrap();
    assert_eq!(unix.owner("/home/ada/notes"), Some((1000, 100)));
    assert_eq!(unix.mode("/home/ada/notes"), Some(0o750));
    assert_eq!(unix.mode("/home/ada/notes/todo.txt"), Some(0o640));
    root.perform(Chmod::new("/home/ada/notes/todo.txt", 0o600))
        .unwrap();
    // Rewriting a file keeps its owner and mode.
    write_file(&mut root, "/home/ada/notes/todo.txt", "eggs").unwrap();
    assert_eq!(unix.mode("/home/ada/notes/todo.txt"), Some(0o600));
    // A file the user did not make stays root's.
    let denied = root.perform(Chmod::new("/etc/passwd", 0o666)).unwrap_err();
    assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
}
