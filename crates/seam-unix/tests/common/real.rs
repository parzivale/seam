// The same checks for every real handler of seam-unix's operations
// (seam-rustix and seam-nix include this), against the real system.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use seam_core::io::{self, ErrorKind};
use seam_core::{Inspect, Perform};
use seam_unix::{
    Chmod, Chown, Hostname, Ids, Kill, Mount, ProcessId, ReadLink, SetUmask, Symlink, Unmount,
};

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seam-unix-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn exercise<C>(c: &mut C, name: &str)
where
    C: for<'a> Perform<Chmod<'a>>
        + for<'a> Perform<Chown<'a>>
        + for<'a> Perform<Symlink<'a>>
        + for<'a> Inspect<ReadLink<'a>>
        + Inspect<Ids>
        + Inspect<ProcessId>
        + Inspect<Hostname>
        + Perform<Kill>
        + Perform<SetUmask>
        + for<'a> Perform<Mount<'a>>
        + for<'a> Perform<Unmount<'a>>,
{
    let dir = scratch(name);
    let file = dir.join("file");
    std::fs::write(&file, "x").unwrap();

    c.perform(Chmod::new(&file, 0o640)).unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o640);
    let missing = c.perform(Chmod::new(&dir.join("missing"), 0o600));
    assert_eq!(missing.unwrap_err().kind(), ErrorKind::NotFound);

    // Changing nothing is allowed for anyone.
    let ids = c.inspect(Ids);
    c.perform(Chown::new(&file, None, None)).unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().uid(), ids.euid);

    let link = dir.join("link");
    c.perform(Symlink::new(&file, &link)).unwrap();
    assert_eq!(c.inspect(ReadLink::new(&link)).unwrap(), file);
    assert_eq!(c.perform(Symlink::new(&file, &link)).unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(c.inspect(ReadLink::new(&dir.join("nope"))).unwrap_err().kind(), ErrorKind::NotFound);

    assert_eq!(c.inspect(ProcessId), std::process::id());
    assert!(!c.inspect(Hostname).unwrap().is_empty());

    c.perform(Kill { pid: std::process::id(), signal: 0 }).unwrap();
    let gone = c.perform(Kill { pid: 0x3fff_fff0, signal: 0 });
    assert_eq!(gone.unwrap_err().kind(), ErrorKind::NotFound);

    let before = c.perform(SetUmask(0o077));
    assert_eq!(c.perform(SetUmask(before)), 0o077);

    // A bind mount, where the process may mount (root, in a container
    // that allows it).
    #[cfg(target_os = "linux")]
    {
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("seen"), "through the mount").unwrap();
        match c.perform(Mount::bind(&from, &to)) {
            Ok(()) => {
                let seen = std::fs::read_to_string(to.join("seen")).unwrap();
                c.perform(Unmount::new(&to)).unwrap();
                assert_eq!(seen, "through the mount");
                assert!(!to.join("seen").exists());
            }
            Err(e) => assert_eq!(e.kind(), ErrorKind::PermissionDenied, "{e}"),
        }
        let not_mounted = c.perform(Unmount::new(&to)).unwrap_err().kind();
        assert!(matches!(not_mounted, ErrorKind::InvalidInput | ErrorKind::PermissionDenied));
    }

    let _ = std::fs::remove_dir_all(&dir);
    let _: io::Result<()> = Ok(());
}
