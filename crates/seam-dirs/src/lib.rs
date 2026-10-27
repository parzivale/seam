//! Platform directories for seam: where a program keeps its configuration,
//! data and caches.
//!
//! The `dirs` and `directories` crates find these by reading the
//! environment and asking the platform, which are effects. This crate
//! applies the same rules to the environment as seam reads it (`Inspect<Var>`),
//! so a core that needs its config directory says so in its bounds, and a
//! test sets `HOME` or `XDG_CONFIG_HOME` on a `Sim` to choose it.
//!
//! | [`Dir`] | Linux (XDG) | macOS | Windows |
//! |---|---|---|---|
//! | `Home` | `$HOME` | `$HOME` | `%USERPROFILE%` |
//! | `Config` | `$XDG_CONFIG_HOME` or `~/.config` | `~/Library/Application Support` | `%APPDATA%` |
//! | `ConfigLocal` | as `Config` | as `Config` | `%LOCALAPPDATA%` |
//! | `Data` | `$XDG_DATA_HOME` or `~/.local/share` | `~/Library/Application Support` | `%APPDATA%` |
//! | `DataLocal` | as `Data` | as `Data` | `%LOCALAPPDATA%` |
//! | `Cache` | `$XDG_CACHE_HOME` or `~/.cache` | `~/Library/Caches` | `%LOCALAPPDATA%` |
//! | `State` | `$XDG_STATE_HOME` or `~/.local/state` | none | none |
//! | `Runtime` | `$XDG_RUNTIME_DIR` | none | none |
//! | `Executable` | `$XDG_BIN_HOME` or `~/.local/bin` | none | none |
//! | `Preference` | as `Config` | `~/Library/Preferences` | `%APPDATA%` |
//!
//! As the XDG spec says, an `XDG_*` variable that is empty or not an
//! absolute path is ignored. Where `dirs` asks the platform rather than the
//! environment (the password database when `HOME` is unset, Windows' known
//! folders), this crate reads only the environment, and answers `None`.
//!
//! [`ProjectDirs`] gives a program its own directories inside those, named
//! as `directories` names them.
//!
//! ```
//! use seam_dirs::{Dir, ProjectDirs, Platform, dir_on};
//! use seam_test::Sim;
//!
//! let mut sim = Sim::new();
//! sim.set_var("HOME", "/home/ada");
//! assert_eq!(dir_on(&sim, Dir::Cache, Platform::Linux).unwrap(), std::path::Path::new("/home/ada/.cache"));
//!
//! sim.set_var("XDG_CONFIG_HOME", "/etc/xdg-ada");
//! let app = ProjectDirs::new("org", "Example Corp", "Frobnicator");
//! assert_eq!(
//!     app.dir_on(&sim, Dir::Config, Platform::Linux).unwrap(),
//!     std::path::Path::new("/etc/xdg-ada/frobnicator"),
//! );
//! assert_eq!(
//!     app.dir_on(&sim, Dir::Config, Platform::MacOs).unwrap(),
//!     std::path::Path::new("/home/ada/Library/Application Support/org.Example-Corp.Frobnicator"),
//! );
//! ```
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::string::String;

use seam_core::ops::env::Var;

/// A kind of directory. See the [crate docs](crate) for where each is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// The user's home directory.
    Home,
    /// Configuration, roaming where the platform roams.
    Config,
    /// Configuration that stays on this machine.
    ConfigLocal,
    /// Data, roaming where the platform roams.
    Data,
    /// Data that stays on this machine.
    DataLocal,
    /// Caches: data that can be thrown away.
    Cache,
    /// State that persists but is not worth keeping in data: logs,
    /// history.
    State,
    /// Sockets, pipes and other files that last as long as the login.
    Runtime,
    /// The user's own executables.
    Executable,
    /// Preferences.
    Preference,
}

/// Whose rules to follow. [`Platform::current`] is the one the program
/// was built for; tests can pick another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// The XDG base directory spec: Linux, the BSDs and other Unixes.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
}

impl Platform {
    /// The platform this program was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The variables a lookup may read, in the order [`resolve`] wants them.
fn wanted(dir: Dir, platform: Platform) -> [Option<&'static str>; 2] {
    use Dir::*;
    match platform {
        Platform::Linux => [
            Some("HOME"),
            match dir {
                Config | ConfigLocal | Preference => Some("XDG_CONFIG_HOME"),
                Data | DataLocal => Some("XDG_DATA_HOME"),
                Cache => Some("XDG_CACHE_HOME"),
                State => Some("XDG_STATE_HOME"),
                Runtime => Some("XDG_RUNTIME_DIR"),
                Executable => Some("XDG_BIN_HOME"),
                Home => None,
            },
        ],
        Platform::MacOs => [Some("HOME"), None],
        Platform::Windows => [
            Some("USERPROFILE"),
            match dir {
                Config | Data | Preference => Some("APPDATA"),
                ConfigLocal | DataLocal | Cache => Some("LOCALAPPDATA"),
                _ => None,
            },
        ],
    }
}

/// An absolute, non-empty path from a variable.
fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The rules, given the variables [`wanted`] named.
fn resolve(dir: Dir, platform: Platform, [home, var]: [Option<OsString>; 2]) -> Option<PathBuf> {
    use Dir::*;
    let home = absolute(home);
    let var = absolute(var);
    let under = |rel: &str| home.as_ref().map(|h| h.join(rel));
    match (platform, dir) {
        (_, Home) => home,
        (Platform::Linux, Runtime) => var,
        (Platform::Linux, Config | ConfigLocal | Preference) => var.or_else(|| under(".config")),
        (Platform::Linux, Data | DataLocal) => var.or_else(|| under(".local/share")),
        (Platform::Linux, Cache) => var.or_else(|| under(".cache")),
        (Platform::Linux, State) => var.or_else(|| under(".local/state")),
        (Platform::Linux, Executable) => var.or_else(|| under(".local/bin")),
        (Platform::MacOs, Config | ConfigLocal | Data | DataLocal) => {
            under("Library/Application Support")
        }
        (Platform::MacOs, Cache) => under("Library/Caches"),
        (Platform::MacOs, Preference) => under("Library/Preferences"),
        (Platform::MacOs, State | Runtime | Executable) => None,
        (Platform::Windows, _) => var,
    }
}

/// Writes each lookup in its sync, async and `Send` forms, from one body.
macro_rules! three_ways {
    ($(
        $(#[$m:meta])*
        pub fn $name:ident, $aname:ident, $sname:ident
        ($($args:tt)*) -> $ret:ty
        $body:block
    )*) => {$(
        $(#[$m])*
        pub fn $name<C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::Inspect<Var<'a>> + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::Inspect::inspect($c, $q) };
            }
            $body
        }

        #[doc = concat!("`", stringify!($name), "`, for async capabilities.")]
        pub async fn $aname<C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::AsyncInspect<Var<'a>> + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::AsyncInspect::inspect_async($c, $q).await };
            }
            $body
        }

        #[doc = concat!(
            "`", stringify!($name), "`, for async capabilities, with a `Send` future: ",
            "for multithreaded runtimes."
        )]
        pub async fn $sname<C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::SendInspect<Var<'a>> + Sync + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! ask {
                ($c:expr, $q:expr) => { seam_core::future::SendInspect::inspect_send($c, $q).await };
            }
            $body
        }
    )*};
}

three_ways! {
    /// Where `dir` is on `platform`, from the environment `c` reads; `None`
    /// if the platform has no such directory, or the variables it is found
    /// from are unset.
    pub fn dir_on, dir_on_async, dir_on_send
    (c: &C, dir: Dir, platform: Platform) -> Option<PathBuf>
    {
        let mut values: [Option<OsString>; 2] = [None, None];
        for (slot, name) in values.iter_mut().zip(wanted(dir, platform)) {
            if let Some(name) = name {
                *slot = ask!(c, Var::new(name));
            }
        }
        resolve(dir, platform, values)
    }
}

/// Where `dir` is on this platform ([`Platform::current`]). See
/// [`dir_on`].
pub fn dir<C>(c: &C, dir: Dir) -> Option<PathBuf>
where
    C: for<'a> seam_core::Inspect<Var<'a>> + ?Sized,
{
    dir_on(c, dir, Platform::current())
}

/// [`dir`], for async capabilities.
pub async fn dir_async<C>(c: &C, dir: Dir) -> Option<PathBuf>
where
    C: for<'a> seam_core::future::AsyncInspect<Var<'a>> + ?Sized,
{
    dir_on_async(c, dir, Platform::current()).await
}

/// [`dir`], with a `Send` future.
pub async fn dir_send<C>(c: &C, dir: Dir) -> Option<PathBuf>
where
    C: for<'a> seam_core::future::SendInspect<Var<'a>> + Sync + ?Sized,
{
    dir_on_send(c, dir, Platform::current()).await
}

/// A program's own directories, inside the user's, named as the
/// `directories` crate names them: on Linux the application's name in
/// lowercase with spaces turned to `-` (`frobnicator`); on macOS a reverse
/// domain name (`org.Example-Corp.Frobnicator`); on Windows
/// `Organization\Application`, with config and data in subdirectories
/// `config` and `data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDirs {
    qualifier: String,
    organization: String,
    application: String,
}

impl ProjectDirs {
    /// The project `application`, by `organization`, whose domain's top
    /// level is `qualifier` (`"org"`, `"com"`, or empty).
    pub fn new(qualifier: &str, organization: &str, application: &str) -> Self {
        Self {
            qualifier: qualifier.into(),
            organization: organization.into(),
            application: application.into(),
        }
    }

    /// The project's own path inside `base`, for `dir`, on `platform`.
    fn inside(&self, base: PathBuf, dir: Dir, platform: Platform) -> PathBuf {
        let dash = |s: &str| s.replace(' ', "-");
        match platform {
            Platform::Linux => base.join(dash(&self.application).to_lowercase()),
            Platform::MacOs => {
                let mut name = String::new();
                for part in [&self.qualifier, &self.organization, &self.application] {
                    if !part.is_empty() {
                        if !name.is_empty() {
                            name.push('.');
                        }
                        name.push_str(&dash(part));
                    }
                }
                base.join(name)
            }
            Platform::Windows => {
                let project = base.join(&self.organization).join(&self.application);
                match dir {
                    Dir::Config | Dir::ConfigLocal | Dir::Preference => project.join("config"),
                    Dir::Data | Dir::DataLocal => project.join("data"),
                    Dir::Cache => project.join("cache"),
                    _ => project,
                }
            }
        }
    }

    /// The project's directory of kind `dir` on `platform`; `None` where
    /// the user's is (and always for `Home`, which a project has none of).
    pub fn dir_on<C>(&self, c: &C, dir: Dir, platform: Platform) -> Option<PathBuf>
    where
        C: for<'a> seam_core::Inspect<Var<'a>> + ?Sized,
    {
        if dir == Dir::Home {
            return None;
        }
        Some(self.inside(dir_on(c, dir, platform)?, dir, platform))
    }

    /// [`dir_on`](Self::dir_on), for this platform.
    pub fn dir<C>(&self, c: &C, dir: Dir) -> Option<PathBuf>
    where
        C: for<'a> seam_core::Inspect<Var<'a>> + ?Sized,
    {
        self.dir_on(c, dir, Platform::current())
    }

    /// [`dir_on`](Self::dir_on), for async capabilities.
    pub async fn dir_on_async<C>(&self, c: &C, dir: Dir, platform: Platform) -> Option<PathBuf>
    where
        C: for<'a> seam_core::future::AsyncInspect<Var<'a>> + ?Sized,
    {
        if dir == Dir::Home {
            return None;
        }
        Some(self.inside(dir_on_async(c, dir, platform).await?, dir, platform))
    }
}
