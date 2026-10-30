//! rustix for seam: the Unix effects of `seam-unix`, performed with rustix.
//!
//! [`Rustix`] implements every operation in `seam_unix` on Unix: mounts
//! (Linux), `chmod`, `chown`, symbolic links, IDs, the host name, signals
//! and the umask. List it in a root for the keys it serves, beside `Std`
//! for files: `seam::root![Std<fx::View>, Rustix<unix::Mount, unix::Unmount>]`,
//! and use `seam_unix::SimUnix` in its place in tests.
//!
//! ```no_run
//! use seam_core::prelude::*;
//! use seam_unix::{Ids, Mount, Unmount};
//!
//! let mut unix = seam_rustix::Rustix;
//! if unix.inspect(Ids).is_root() {
//!     unix.perform(Mount::bind("/srv/data", "/mnt/data")).unwrap();
//!     unix.perform(Unmount::new("/mnt/data")).unwrap();
//! }
//! ```
//! # In a program
//!
//! [`Rustix`] is sync: these calls return at once (a mount, at worst, after a
//! moment), so the lifts make it async too, for any executor. In a root, it
//! serves the keys of `seam_unix::effects` it is listed for, beside the
//! handlers for the rest:
//!
//! ```no_run
//! use seam_core::effects as fx;
//! use seam_core::prelude::*;
//! use seam_std::Std;
//! use seam_unix::{Chmod, effects as unix};
//!
//! let mut root = seam_core::root![
//!     Std<fx::Var>,
//!     seam_rustix::Rustix<unix::Chmod, unix::Hostname>,
//! ];
//! root.perform(Chmod::new("/srv/data/report.txt", 0o640))?;
//! # Ok::<(), seam_core::io::Error>(())
//! ```
//!
//! In tests, `seam_unix::SimUnix` takes its place, with the same keys.
//!
//! Through the `seam` crate, this crate is `seam::rustix` (feature `rustix`).
#![forbid(unsafe_code)]

/// Performs `seam-unix`'s operations with rustix.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rustix;

impl seam_core::Capability for Rustix {}

#[cfg(unix)]
mod imp {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::string::String;

    use rustix::fs::Mode;
    use rustix::process::{Gid, Pid, Signal, Uid};
    use seam_core::io;
    use seam_core::{Inspect, Perform};
    use seam_unix::{
        Chmod, Chown, Hostname, Ids, Kill, ProcessId, ReadLink, SetUmask, Symlink, UserIds,
    };

    use super::Rustix;

    /// rustix's error as seam's, by its OS error code.
    pub(crate) fn errno(e: rustix::io::Errno) -> io::Error {
        if e == rustix::io::Errno::SRCH {
            return io::Error::new(io::ErrorKind::NotFound, "no such process");
        }
        std::io::Error::from_raw_os_error(e.raw_os_error()).into()
    }

    fn mode(bits: u32) -> Mode {
        #[allow(clippy::unnecessary_cast)] // u16 on some platforms
        Mode::from_raw_mode(bits as rustix::fs::RawMode)
    }

    impl Perform<Chmod<'_>> for Rustix {
        fn perform(&mut self, op: Chmod<'_>) -> io::Result<()> {
            rustix::fs::chmod(op.path, mode(op.mode)).map_err(errno)
        }
    }

    impl Perform<Chown<'_>> for Rustix {
        fn perform(&mut self, op: Chown<'_>) -> io::Result<()> {
            rustix::fs::chown(
                op.path,
                op.uid.map(Uid::from_raw),
                op.gid.map(Gid::from_raw),
            )
            .map_err(errno)
        }
    }

    impl Perform<Symlink<'_>> for Rustix {
        fn perform(&mut self, op: Symlink<'_>) -> io::Result<()> {
            rustix::fs::symlink(op.original, op.link).map_err(errno)
        }
    }

    impl Inspect<ReadLink<'_>> for Rustix {
        fn inspect(&self, op: ReadLink<'_>) -> io::Result<PathBuf> {
            let target = rustix::fs::readlink(op.0, std::vec::Vec::new()).map_err(errno)?;
            Ok(OsString::from_vec(target.into_bytes()).into())
        }
    }

    impl Inspect<Ids> for Rustix {
        fn inspect(&self, _: Ids) -> UserIds {
            UserIds {
                uid: rustix::process::getuid().as_raw(),
                gid: rustix::process::getgid().as_raw(),
                euid: rustix::process::geteuid().as_raw(),
                egid: rustix::process::getegid().as_raw(),
            }
        }
    }

    impl Inspect<ProcessId> for Rustix {
        fn inspect(&self, _: ProcessId) -> u32 {
            rustix::process::getpid()
                .as_raw_nonzero()
                .get()
                .unsigned_abs()
        }
    }

    impl Inspect<Hostname> for Rustix {
        fn inspect(&self, _: Hostname) -> io::Result<String> {
            Ok(rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .into_owned())
        }
    }

    impl Perform<Kill> for Rustix {
        fn perform(&mut self, op: Kill) -> io::Result<()> {
            let pid = i32::try_from(op.pid)
                .ok()
                .and_then(Pid::from_raw)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a process ID"))?;
            if op.signal == 0 {
                return rustix::process::test_kill_process(pid).map_err(errno);
            }
            let signal = Signal::from_named_raw(op.signal)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a signal"))?;
            rustix::process::kill_process(pid, signal).map_err(errno)
        }
    }

    impl Perform<SetUmask> for Rustix {
        #[allow(clippy::useless_conversion)] // u16 on some platforms
        fn perform(&mut self, op: SetUmask) -> u32 {
            u32::from(rustix::process::umask(mode(op.0)).as_raw_mode())
        }
    }
}

#[cfg(target_os = "linux")]
mod mount {
    use std::ffi::CString;

    use rustix::mount::{MountFlags as Flags, UnmountFlags};
    use seam_core::Perform;
    use seam_core::io;
    use seam_unix::{Mount, MountFlags, Unmount};

    use super::Rustix;
    use super::imp::errno;

    /// The flags beyond `BIND`, `REC` and `REMOUNT`, which pick the call.
    fn flags(f: MountFlags) -> Flags {
        let mut out = Flags::empty();
        for (ours, theirs) in [
            (MountFlags::RDONLY, Flags::RDONLY),
            (MountFlags::NOSUID, Flags::NOSUID),
            (MountFlags::NODEV, Flags::NODEV),
            (MountFlags::NOEXEC, Flags::NOEXEC),
        ] {
            if f.contains(ours) {
                out |= theirs;
            }
        }
        out
    }

    fn c_string(s: &str) -> io::Result<CString> {
        CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL byte"))
    }

    impl Perform<Mount<'_>> for Rustix {
        fn perform(&mut self, op: Mount<'_>) -> io::Result<()> {
            let data = op.data.map(c_string).transpose()?;
            if op.flags.contains(MountFlags::REMOUNT) {
                let bind = if op.flags.contains(MountFlags::BIND) {
                    Flags::BIND
                } else {
                    Flags::empty()
                };
                return rustix::mount::mount_remount(
                    op.target,
                    flags(op.flags) | bind,
                    op.data.unwrap_or(""),
                )
                .map_err(errno);
            }
            if op.flags.contains(MountFlags::BIND) {
                let source = op.source.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "a bind mount needs a source")
                })?;
                if op.flags.contains(MountFlags::REC) {
                    rustix::mount::mount_bind_recursive(source, op.target).map_err(errno)?;
                } else {
                    rustix::mount::mount_bind(source, op.target).map_err(errno)?;
                }
                // A bind mount takes its other flags in a remount.
                if !flags(op.flags).is_empty() {
                    rustix::mount::mount_remount(op.target, flags(op.flags) | Flags::BIND, "")
                        .map_err(errno)?;
                }
                return Ok(());
            }
            let source = op.source.map_or_else(|| std::path::Path::new(""), |s| s);
            rustix::mount::mount(
                source,
                op.target,
                op.fstype.unwrap_or(""),
                flags(op.flags),
                data.as_deref(),
            )
            .map_err(errno)
        }
    }

    impl Perform<Unmount<'_>> for Rustix {
        fn perform(&mut self, op: Unmount<'_>) -> io::Result<()> {
            let flags = if op.lazy {
                UnmountFlags::DETACH
            } else {
                UnmountFlags::empty()
            };
            rustix::mount::unmount(op.target, flags).map_err(errno)
        }
    }
}
