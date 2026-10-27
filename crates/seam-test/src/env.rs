use seam_core::io;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::format;
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

use seam_core::ops::env::{Args, CurrentDir, RemoveVar, SetVar, TempDir, Var};

use seam_core::{Inspect, Perform};

use crate::Sim;
use crate::sim::{Inner, States, World};
use crate::world_file::{self, Failure, Node};

/// The simulated environment: variables, arguments, and the current and
/// temporary directories.
///
/// Its block is `env { var NAME VALUE; unset NAME; args A..; cwd P; temp P }`.
/// Relative paths in filesystem operations resolve against `cwd`.
#[derive(Debug, Clone)]
pub struct Env {
    vars: BTreeMap<OsString, OsString>,
    args: Vec<OsString>,
    temp: PathBuf,
    cwd: PathBuf,
}

impl Default for Env {
    fn default() -> Self {
        Self {
            vars: BTreeMap::new(),
            args: Vec::from([OsString::from("sim")]),
            temp: PathBuf::from("/tmp"),
            cwd: PathBuf::from("/"),
        }
    }
}

impl Env {
    /// The current directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// A variable's value.
    pub fn var(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        self.vars.get(name.as_ref()).map(OsString::as_os_str)
    }
}

impl Inner {
    /// The current directory, that relative paths resolve against.
    #[cfg(feature = "fs")]
    pub(crate) fn cwd(&mut self) -> PathBuf {
        self.state::<Env>().cwd.clone()
    }
}

impl Sim {
    /// Sets an environment variable.
    pub fn set_var(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.lock()
            .state::<Env>()
            .vars
            .insert(name.into(), value.into());
        self
    }

    /// Removes an environment variable.
    pub fn remove_var(&mut self, name: impl Into<OsString>) -> &mut Self {
        self.lock().state::<Env>().vars.remove(&name.into());
        self
    }

    /// Sets the command-line arguments, including the program name. The
    /// default is `["sim"]`.
    pub fn set_args<I: IntoIterator<Item = A>, A: Into<OsString>>(&mut self, args: I) -> &mut Self {
        self.lock().state::<Env>().args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the directory `TempDir` reports. The default is `/tmp`.
    pub fn set_temp_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.lock().state::<Env>().temp = path.into();
        self
    }

    /// Sets the current directory. Relative paths in filesystem operations
    /// are resolved against it.
    pub fn set_current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        let mut states = self.lock();
        let env = states.state::<Env>();
        env.cwd = env.cwd.join(path);
        drop(states);
        self
    }
}

fn env_node(node: &Node) -> world_file::Result<()> {
    node.no_children()?;
    node.props_only(&[])
}

impl World for Env {
    const BLOCKS: &'static [&'static str] = &["env"];

    fn load(&mut self, block: &Node) -> world_file::Result<()> {
        for node in block.block()? {
            env_node(node)?;
            match node.name() {
                "var" => {
                    node.arity(2, Some(2), "a name and a value")?;
                    let name = node.args()[0].string("the name")?;
                    let value = node.args()[1].string("the value")?;
                    self.vars.insert(name.into(), value.into());
                }
                "unset" => {
                    node.arity(1, Some(1), "a name")?;
                    self.vars
                        .remove(OsStr::new(node.args()[0].string("the name")?));
                }
                "args" => {
                    self.args = node
                        .args()
                        .iter()
                        .map(|a| a.string("an argument").map(OsString::from))
                        .collect::<world_file::Result<Vec<_>>>()?;
                }
                "cwd" => {
                    node.arity(1, Some(1), "a path")?;
                    self.cwd = self.cwd.join(node.args()[0].string("the path")?);
                }
                "temp" => {
                    node.arity(1, Some(1), "a path")?;
                    self.temp = node.args()[0].string("the path")?.into();
                }
                _ => {
                    return Err(node.unknown("`env`", &["var", "unset", "args", "cwd", "temp"]));
                }
            }
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("env");
        for (name, value) in &self.vars {
            block = block.with_child(
                Node::new("var")
                    .with_arg(name.to_string_lossy().into_owned())
                    .with_arg(value.to_string_lossy().into_owned()),
            );
        }
        let mut args = Node::new("args");
        for arg in &self.args {
            args = args.with_arg(arg.to_string_lossy().into_owned());
        }
        block = block
            .with_child(args)
            .with_child(Node::new("cwd").with_arg(self.cwd.to_string_lossy().into_owned()))
            .with_child(Node::new("temp").with_arg(self.temp.to_string_lossy().into_owned()));
        std::vec![block]
    }

    /// `var NAME VALUE` expects the variable to hold exactly that, and
    /// `unset NAME` expects it not to be set; `args`, `cwd` and `temp` must
    /// be as listed.
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> world_file::Result<()> {
        for node in block.block()? {
            env_node(node)?;
            let (name, want) = match node.name() {
                "var" => {
                    node.arity(2, Some(2), "a name and a value")?;
                    let name = node.args()[0].string("the name")?;
                    (name, Some(node.args()[1].string("the value")?))
                }
                "unset" => {
                    node.arity(1, Some(1), "a name")?;
                    (node.args()[0].string("the name")?, None)
                }
                "args" | "cwd" | "temp" => {
                    let wrapped = Node::new("env").with_child(node.clone());
                    crate::world_file::check_subset(&self.to_world(), &wrapped, failures);
                    continue;
                }
                _ => {
                    return Err(node.unknown(
                        "`env` expectations",
                        &["var", "unset", "args", "cwd", "temp"],
                    ));
                }
            };
            let got = self.vars.get(OsStr::new(name)).map(OsString::as_os_str);
            let want = want.map(OsStr::new);
            if got != want {
                let show = |v: Option<&OsStr>| match v {
                    Some(v) => format!("{:?}", v.to_string_lossy()),
                    None => String::from("unset"),
                };
                failures.push(node.failure(format!(
                    "env: expected {name} {}, found {}",
                    show(want),
                    show(got)
                )));
            }
        }
        Ok(())
    }
}

impl Inspect<Var<'_>> for Sim {
    fn inspect(&self, op: Var<'_>) -> Option<OsString> {
        let states = &mut States::new(&self.shared);
        states.state::<Env>().vars.get(op.name).cloned()
    }
}

impl Perform<SetVar<'_>> for Sim {
    fn perform(&mut self, op: SetVar<'_>) -> io::Result<()> {
        let states = &mut States::new(&self.shared);
        op.check()?;
        states
            .state::<Env>()
            .vars
            .insert(OsString::from(op.name), OsString::from(op.value));
        Ok(())
    }
}

impl Perform<RemoveVar<'_>> for Sim {
    fn perform(&mut self, op: RemoveVar<'_>) -> io::Result<()> {
        let states = &mut States::new(&self.shared);
        op.check()?;
        states.state::<Env>().vars.remove(op.name);
        Ok(())
    }
}

impl Inspect<Args> for Sim {
    fn inspect(&self, _: Args) -> Vec<OsString> {
        let states = &mut States::new(&self.shared);
        states.state::<Env>().args.clone()
    }
}

impl Inspect<CurrentDir> for Sim {
    fn inspect(&self, _: CurrentDir) -> io::Result<PathBuf> {
        let states = &mut States::new(&self.shared);
        Ok(states.state::<Env>().cwd.clone())
    }
}

impl Inspect<TempDir> for Sim {
    fn inspect(&self, _: TempDir) -> PathBuf {
        let states = &mut States::new(&self.shared);
        states.state::<Env>().temp.clone()
    }
}
