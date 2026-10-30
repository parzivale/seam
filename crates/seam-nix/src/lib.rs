//! nix for seam: the Unix effects of `seam-unix`, performed with nix.
//!
//! [`Nix`] implements every operation in `seam_unix` on Unix: mounts
//! (Linux), `chmod`, `chown`, symbolic links, IDs, the host name, signals
//! and the umask. It serves the same operations as `seam-rustix`'s
//! `Rustix`: pick whichever crate the program already uses, and use
//! `seam_unix::SimUnix` in tests.
//!
//! ```no_run
//! use seam_core::prelude::*;
//! use seam_unix::{Hostname, Kill, signals};
//!
//! let mut unix = seam_nix::Nix;
//! println!("on {}", unix.inspect(Hostname).unwrap());
//! unix.perform(Kill { pid: 4242, signal: signals::SIGTERM }).unwrap();
//! ```
//! # In a program
//!
//! [`Nix`] is sync: these calls return at once (a mount, at worst, after a
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
//!     seam_nix::Nix<unix::Chmod, unix::Hostname>,
//! ];
//! root.perform(Chmod::new("/srv/data/report.txt", 0o640))?;
//! # Ok::<(), seam_core::io::Error>(())
//! ```
//!
//! In tests, `seam_unix::SimUnix` takes its place, with the same keys.
//!
//! Through the `seam` crate, this crate is `seam::nix` (feature `nix`).
#![forbid(unsafe_code)]

/// Performs `seam-unix`'s operations with nix.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nix;

impl seam_core::Capability for Nix {}

#[cfg(unix)]
mod imp {
    use std::path::PathBuf;
    use std::string::String;

    use nix::fcntl::AT_FDCWD;
    use nix::sys::signal::Signal;
    use nix::sys::stat::{FchmodatFlags, Mode};
    use nix::unistd::{Gid, Pid, Uid};
    use seam_core::io;
    use seam_core::{Inspect, Perform};
    use seam_unix::{
        Chmod, Chown, Hostname, Ids, Kill, ProcessId, ReadLink, SetUmask, Symlink, UserIds,
    };

    use super::Nix;

    /// nix's error as seam's, by its OS error code.
    pub(crate) fn errno(e: nix::Error) -> io::Error {
        if e == nix::Error::ESRCH {
            return io::Error::new(io::ErrorKind::NotFound, "no such process");
        }
        std::io::Error::from(e).into()
    }

    fn mode(bits: u32) -> Mode {
        #[allow(clippy::unnecessary_cast)] // u16 on some platforms
        Mode::from_bits_truncate(bits as nix::libc::mode_t)
    }

    impl Perform<Chmod<'_>> for Nix {
        fn perform(&mut self, op: Chmod<'_>) -> io::Result<()> {
            nix::sys::stat::fchmodat(
                AT_FDCWD,
                op.path,
                mode(op.mode),
                FchmodatFlags::FollowSymlink,
            )
            .map_err(errno)
        }
    }

    impl Perform<Chown<'_>> for Nix {
        fn perform(&mut self, op: Chown<'_>) -> io::Result<()> {
            nix::unistd::chown(
                op.path,
                op.uid.map(Uid::from_raw),
                op.gid.map(Gid::from_raw),
            )
            .map_err(errno)
        }
    }

    impl Perform<Symlink<'_>> for Nix {
        fn perform(&mut self, op: Symlink<'_>) -> io::Result<()> {
            nix::unistd::symlinkat(op.original, AT_FDCWD, op.link).map_err(errno)
        }
    }

    impl Inspect<ReadLink<'_>> for Nix {
        fn inspect(&self, op: ReadLink<'_>) -> io::Result<PathBuf> {
            nix::fcntl::readlink(op.0).map(PathBuf::from).map_err(errno)
        }
    }

    impl Inspect<Ids> for Nix {
        fn inspect(&self, _: Ids) -> UserIds {
            UserIds {
                uid: nix::unistd::getuid().as_raw(),
                gid: nix::unistd::getgid().as_raw(),
                euid: nix::unistd::geteuid().as_raw(),
                egid: nix::unistd::getegid().as_raw(),
            }
        }
    }

    impl Inspect<ProcessId> for Nix {
        fn inspect(&self, _: ProcessId) -> u32 {
            nix::unistd::getpid().as_raw().unsigned_abs()
        }
    }

    impl Inspect<Hostname> for Nix {
        fn inspect(&self, _: Hostname) -> io::Result<String> {
            let name = nix::unistd::gethostname().map_err(errno)?;
            Ok(name.to_string_lossy().into_owned())
        }
    }

    impl Perform<Kill> for Nix {
        fn perform(&mut self, op: Kill) -> io::Result<()> {
            let pid = i32::try_from(op.pid)
                .ok()
                .filter(|&p| p > 0)
                .map(Pid::from_raw)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a process ID"))?;
            let signal = match op.signal {
                0 => None,
                n => Some(
                    Signal::try_from(n)
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a signal"))?,
                ),
            };
            nix::sys::signal::kill(pid, signal).map_err(errno)
        }
    }

    impl Perform<SetUmask> for Nix {
        #[allow(clippy::useless_conversion)] // u16 on some platforms
        fn perform(&mut self, op: SetUmask) -> u32 {
            u32::from(nix::sys::stat::umask(mode(op.0)).bits())
        }
    }
}

#[cfg(target_os = "linux")]
mod mount {
    use std::path::Path;

    use nix::mount::{MntFlags, MsFlags};
    use seam_core::Perform;
    use seam_core::io;
    use seam_unix::{Mount, MountFlags, Unmount};

    use super::Nix;
    use super::imp::errno;

    fn flags(f: MountFlags) -> MsFlags {
        let mut out = MsFlags::empty();
        for (ours, theirs) in [
            (MountFlags::BIND, MsFlags::MS_BIND),
            (MountFlags::RDONLY, MsFlags::MS_RDONLY),
            (MountFlags::NOSUID, MsFlags::MS_NOSUID),
            (MountFlags::NODEV, MsFlags::MS_NODEV),
            (MountFlags::NOEXEC, MsFlags::MS_NOEXEC),
            (MountFlags::REC, MsFlags::MS_REC),
            (MountFlags::REMOUNT, MsFlags::MS_REMOUNT),
        ] {
            if f.contains(ours) {
                out |= theirs;
            }
        }
        out
    }

    impl Perform<Mount<'_>> for Nix {
        fn perform(&mut self, op: Mount<'_>) -> io::Result<()> {
            let all = flags(op.flags);
            nix::mount::mount(op.source, op.target, op.fstype, all, op.data).map_err(errno)?;
            // A bind mount takes its other flags in a remount.
            let extra = all - MsFlags::MS_BIND - MsFlags::MS_REC - MsFlags::MS_REMOUNT;
            if op.flags.contains(MountFlags::BIND)
                && !op.flags.contains(MountFlags::REMOUNT)
                && !extra.is_empty()
            {
                nix::mount::mount(
                    None::<&Path>,
                    op.target,
                    None::<&str>,
                    MsFlags::MS_REMOUNT | MsFlags::MS_BIND | extra,
                    None::<&str>,
                )
                .map_err(errno)?;
            }
            Ok(())
        }
    }

    impl Perform<Unmount<'_>> for Nix {
        fn perform(&mut self, op: Unmount<'_>) -> io::Result<()> {
            let flags = if op.lazy {
                MntFlags::MNT_DETACH
            } else {
                MntFlags::empty()
            };
            nix::mount::umount2(op.target, flags).map_err(errno)
        }
    }
}
