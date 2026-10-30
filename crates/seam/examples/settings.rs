//! Where an application keeps its settings, cache and downloads, per
//! platform (`seam::dirs`), and loading its settings from there.
//!
//! The core asks for directories through `Var` (and reads the user dirs
//! file and the settings through the filesystem), so the tests give it a
//! simulated home, on each platform's rules.
//!
//! ```text
//! cargo run -p seam-rs --example settings --features std,test,dirs
//! ```

use std::path::{Path, PathBuf};

use seam::dirs::{Dir, Platform, ProjectDirs, UserDir};
use seam::io;
use seam::ops::env::Var;
use seam::ops::fs::{ReadAt, View, read_to_string};
use seam::prelude::*;

const APP: (&str, &str, &str) = ("org", "Seam Examples", "Settings Demo");

/// Where things go, on `platform`.
#[derive(Debug, PartialEq, Eq)]
struct Places {
    config: Option<PathBuf>,
    cache: Option<PathBuf>,
    downloads: Option<PathBuf>,
}

fn places<C, F>(c: &C, platform: Platform) -> Places
where
    C: for<'a> Inspect<Var<'a>> + for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    let app = ProjectDirs::new(APP.0, APP.1, APP.2);
    Places {
        config: app.dir_on(c, Dir::Config, platform),
        cache: app.dir_on(c, Dir::Cache, platform),
        downloads: seam::dirs::user_dir_on(c, UserDir::Download, platform),
    }
}

/// The settings file's `key = value` lines, or the defaults where there
/// is no file.
fn settings<C, F>(c: &C, config: &Path) -> io::Result<Vec<(String, String)>>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    let text = match read_to_string(c, &config.join("settings.conf")) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => "theme = light\n".to_owned(),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect())
}

/// The edge: the real environment and filesystem.
fn main() -> io::Result<()> {
    let std = seam::stdlib::Std;
    let here = places(&std, Platform::current());
    println!("{here:#?}");
    if let Some(config) = &here.config {
        println!("settings: {:?}", settings(&std, config)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;

    use super::*;

    fn home() -> Sim {
        let mut sim = Sim::new();
        sim.set_var("HOME", "/home/ada");
        sim
    }

    #[test]
    fn each_platform_has_its_own_places() {
        let mut sim = home();
        sim.add_file(
            "/home/ada/.config/user-dirs.dirs",
            "XDG_DOWNLOAD_DIR=\"$HOME/Incoming\"\n",
        );
        assert_eq!(
            places(&sim, Platform::Linux),
            Places {
                config: Some("/home/ada/.config/settings-demo".into()),
                cache: Some("/home/ada/.cache/settings-demo".into()),
                downloads: Some("/home/ada/Incoming".into()),
            }
        );
        let mac = places(&sim, Platform::MacOs);
        assert_eq!(
            mac.config,
            Some("/home/ada/Library/Application Support/org.Seam-Examples.Settings-Demo".into())
        );
        assert_eq!(mac.downloads, Some("/home/ada/Downloads".into()));
    }

    #[test]
    fn settings_load_or_default() {
        let mut sim = home();
        let config = places(&sim, Platform::Linux).config.unwrap();
        assert_eq!(
            settings(&sim, &config).unwrap(),
            [("theme".into(), "light".into())]
        );
        sim.add_file(config.join("settings.conf"), "theme = dark\nfont = mono\n");
        assert_eq!(
            settings(&sim, &config).unwrap(),
            [
                ("theme".into(), "dark".into()),
                ("font".into(), "mono".into())
            ]
        );
    }
}
