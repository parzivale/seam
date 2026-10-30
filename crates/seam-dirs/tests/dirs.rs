//! Each platform's rules, on a simulated environment.

use std::path::{Path, PathBuf};

use seam_dirs::{Dir, Platform, ProjectDirs, dir_on};
use seam_test::Sim;

fn at(sim: &Sim, dir: Dir, platform: Platform) -> Option<PathBuf> {
    dir_on(sim, dir, platform)
}

#[test]
fn xdg_with_and_without_variables() {
    let mut sim = Sim::new();
    assert_eq!(at(&sim, Dir::Config, Platform::Linux), None);
    sim.set_var("HOME", "/home/ada");
    let p = |s: &str| Some(PathBuf::from(s));
    assert_eq!(at(&sim, Dir::Home, Platform::Linux), p("/home/ada"));
    assert_eq!(
        at(&sim, Dir::Config, Platform::Linux),
        p("/home/ada/.config")
    );
    assert_eq!(
        at(&sim, Dir::Data, Platform::Linux),
        p("/home/ada/.local/share")
    );
    assert_eq!(
        at(&sim, Dir::State, Platform::Linux),
        p("/home/ada/.local/state")
    );
    assert_eq!(
        at(&sim, Dir::Executable, Platform::Linux),
        p("/home/ada/.local/bin")
    );
    assert_eq!(at(&sim, Dir::Runtime, Platform::Linux), None);

    sim.set_var("XDG_CACHE_HOME", "/var/cache/ada");
    sim.set_var("XDG_DATA_HOME", "relative/ignored");
    sim.set_var("XDG_RUNTIME_DIR", "/run/user/1000");
    assert_eq!(at(&sim, Dir::Cache, Platform::Linux), p("/var/cache/ada"));
    assert_eq!(
        at(&sim, Dir::Data, Platform::Linux),
        p("/home/ada/.local/share")
    );
    assert_eq!(at(&sim, Dir::Runtime, Platform::Linux), p("/run/user/1000"));
}

#[test]
fn macos_and_windows() {
    let mut sim = Sim::new();
    sim.set_var("HOME", "/Users/ada");
    let p = |s: &str| Some(PathBuf::from(s));
    assert_eq!(
        at(&sim, Dir::Cache, Platform::MacOs),
        p("/Users/ada/Library/Caches")
    );
    assert_eq!(
        at(&sim, Dir::Preference, Platform::MacOs),
        p("/Users/ada/Library/Preferences")
    );
    assert_eq!(at(&sim, Dir::State, Platform::MacOs), None);

    #[cfg(windows)]
    {
        sim.set_var("USERPROFILE", r"C:\Users\ada");
        sim.set_var("APPDATA", r"C:\Users\ada\AppData\Roaming");
        sim.set_var("LOCALAPPDATA", r"C:\Users\ada\AppData\Local");
        assert_eq!(
            at(&sim, Dir::Config, Platform::Windows),
            p(r"C:\Users\ada\AppData\Roaming")
        );
        assert_eq!(
            at(&sim, Dir::Cache, Platform::Windows),
            p(r"C:\Users\ada\AppData\Local")
        );
    }
    // Without Windows' variables, nothing.
    assert_eq!(at(&Sim::new(), Dir::Config, Platform::Windows), None);
}

#[test]
fn project_dirs() {
    let mut sim = Sim::new();
    sim.set_var("HOME", "/home/ada");
    let app = ProjectDirs::new("org", "Example Corp", "Frob Nicator");
    assert_eq!(
        app.dir_on(&sim, Dir::Cache, Platform::Linux).unwrap(),
        Path::new("/home/ada/.cache/frob-nicator")
    );
    assert_eq!(
        app.dir_on(&sim, Dir::Data, Platform::MacOs).unwrap(),
        Path::new("/home/ada/Library/Application Support/org.Example-Corp.Frob-Nicator")
    );
    assert_eq!(app.dir_on(&sim, Dir::Home, Platform::Linux), None);
    let async_dir =
        seam_core::future::block_on(app.dir_on_async(&sim, Dir::Config, Platform::Linux));
    assert_eq!(
        async_dir.unwrap(),
        Path::new("/home/ada/.config/frob-nicator")
    );
}

#[test]
fn user_dirs_from_xdg_and_by_convention() {
    use seam_dirs::{Platform, UserDir, user_dir_on};
    use std::path::Path;

    let mut sim = seam_test::Sim::new();
    sim.set_var("HOME", "/home/ada");
    sim.add_dir("/home/ada/.config");
    sim.add_file(
        "/home/ada/.config/user-dirs.dirs",
        "# written by xdg-user-dirs-update\n\
         XDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\n\
         XDG_DOWNLOAD_DIR=\"/data/downloads\"\n\
         XDG_TEMPLATES_DIR=\"$HOME/\"\n",
    );
    let linux = |d| user_dir_on(&sim, d, Platform::Linux);
    assert_eq!(
        linux(UserDir::Desktop).as_deref(),
        Some(Path::new("/home/ada/Schreibtisch"))
    );
    assert_eq!(
        linux(UserDir::Download).as_deref(),
        Some(Path::new("/data/downloads"))
    );
    // Set to the home directory: turned off.
    assert_eq!(linux(UserDir::Template), None);
    // Not in the file.
    assert_eq!(linux(UserDir::Video), None);
    // Fonts are under the data directory.
    assert_eq!(
        linux(UserDir::Font).as_deref(),
        Some(Path::new("/home/ada/.local/share/fonts"))
    );
    // macOS, by convention.
    assert_eq!(
        user_dir_on(&sim, UserDir::Video, Platform::MacOs).as_deref(),
        Some(Path::new("/home/ada/Movies"))
    );
    assert_eq!(user_dir_on(&sim, UserDir::Template, Platform::MacOs), None);
    // Windows: under the profile, and %PUBLIC%.
    sim.set_var("USERPROFILE", "/Users/ada");
    sim.set_var("PUBLIC", "/Users/Public");
    assert_eq!(
        user_dir_on(&sim, UserDir::Picture, Platform::Windows).as_deref(),
        Some(Path::new("/Users/ada/Pictures"))
    );
    assert_eq!(
        user_dir_on(&sim, UserDir::Public, Platform::Windows).as_deref(),
        Some(Path::new("/Users/Public"))
    );
    // No file at all: none of the XDG ones.
    let mut bare = seam_test::Sim::new();
    bare.set_var("HOME", "/home/bob");
    assert_eq!(user_dir_on(&bare, UserDir::Desktop, Platform::Linux), None);
}
