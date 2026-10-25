//! [`Sim`]'s world-file blocks, written as any other crate's simulated part
//! would write its own: an impl of [`World`] over the public API of
//! [`Node`], [`Value`] and [`Sim`], and nothing else. The integration test
//! `sim_world_public` compiles this same file outside the crate, so a
//! private item used here fails the build.

#[cfg(any(feature = "env", feature = "process"))]
use std::ffi::OsString;
use std::format;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

use seam_test::world_file::{Failure, Node, Result, Value, World, WorldError};

use super::Sim;

/// The blocks `Sim` reads.
const TOP: &[&str] = &[
    "seed",
    "parallelism",
    "clock",
    "env",
    "fs",
    "net",
    "process",
    "stdio",
    "signal",
];

#[allow(dead_code)]
fn needs_feature(node: &Node, feature: &str) -> WorldError {
    node.error(format!(
        "`{}` needs seam-test's `{feature}` feature",
        node.name()
    ))
}

/// Bytes written as hex digits, ignoring spaces and `_`.
#[allow(dead_code)]
fn hex(v: &Value) -> Result<Vec<u8>> {
    let digits: Vec<char> = v
        .string("`hex`")?
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    if digits.len() % 2 != 0 {
        return Err(v.error("`hex` needs an even number of digits"));
    }
    digits
        .chunks(2)
        .map(|pair| {
            let s: String = pair.iter().collect();
            u8::from_str_radix(&s, 16).map_err(|_| v.error(format!("`{s}` is not a hex byte")))
        })
        .collect()
}

/// The contents of a `file`, `stdin`, `stdout` or `stderr` node: the
/// argument at `at`, or a `hex` property.
#[allow(dead_code)]
fn contents(node: &Node, at: usize, usage: &str) -> Result<Vec<u8>> {
    node.props_only(&["hex"])?;
    node.no_children()?;
    match (node.args().get(at), node.prop("hex")) {
        (Some(_), Some(h)) => Err(h.error("give the contents or `hex`, not both")),
        (Some(arg), None) => {
            node.arity(at + 1, Some(at + 1), usage)?;
            Ok(arg.string("the contents")?.as_bytes().to_vec())
        }
        (None, Some(h)) => {
            node.arity(at, Some(at), usage)?;
            hex(h)
        }
        (None, None) => Err(node.error(format!("`{}` takes {usage}", node.name()))),
    }
}

/// A path in an `fs` block: absolute at the top, relative inside a `dir`.
#[allow(dead_code)]
fn path_in(node: &Node, parent: Option<&Path>) -> Result<PathBuf> {
    let arg = node
        .args()
        .first()
        .ok_or_else(|| node.error(format!("`{}` takes a path first", node.name())))?;
    let path = Path::new(arg.string("a path")?);
    match parent {
        None => Ok(path.to_path_buf()),
        Some(_) if path.is_absolute() => Err(arg.error("inside a `dir`, paths are relative to it")),
        Some(parent) => Ok(parent.join(path)),
    }
}

/// Bytes for a message: quoted text if they are UTF-8, hex otherwise.
#[allow(dead_code)]
fn show(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(s) => format!("{s:?}"),
        Err(_) => {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            format!("hex {}", hex.join(""))
        }
    }
}

impl World for Sim {
    const BLOCKS: &'static [&'static str] = TOP;

    fn load(&mut self, block: &Node) -> Result<()> {
        self.load_top(block)
    }

    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> Result<()> {
        self.check_top(block, failures)
    }
}

impl Sim {
    /// The simulation, for seam's generic functions. Outside the crate,
    /// the test's stand-in reaches it by `Deref`.
    #[cfg(feature = "fs")]
    fn sim(&mut self) -> &mut seam_test::Sim {
        self
    }

    fn load_top(&mut self, node: &Node) -> Result<()> {
        match node.name() {
            "seed" => {
                #[cfg(feature = "random")]
                {
                    node.no_children()?;
                    node.props_only(&[])?;
                    node.arity(1, Some(1), "a seed")?;
                    let seed = node.args()[0].uint::<u64>("the seed")?;
                    self.set_seed(seed);
                    Ok(())
                }
                #[cfg(not(feature = "random"))]
                Err(needs_feature(node, "random"))
            }
            "parallelism" => {
                #[cfg(feature = "rt")]
                {
                    node.no_children()?;
                    node.props_only(&[])?;
                    node.arity(1, Some(1), "a number of threads")?;
                    let n = node.args()[0].uint::<usize>("parallelism")?;
                    let n = core::num::NonZeroUsize::new(n)
                        .ok_or_else(|| node.args()[0].error("parallelism must be at least 1"))?;
                    self.set_parallelism(n);
                    Ok(())
                }
                #[cfg(not(feature = "rt"))]
                Err(needs_feature(node, "rt"))
            }
            "clock" => {
                #[cfg(feature = "time")]
                {
                    node.no_children()?;
                    node.arity(0, Some(0), "only `now=` and `wall=`")?;
                    node.props_only(&["now", "wall"])?;
                    if let Some(now) = node.prop("now") {
                        let secs = now.uint::<u64>("`now`")?;
                        self.advance(core::time::Duration::from_secs(secs));
                    }
                    if let Some(wall) = node.prop("wall") {
                        let secs = wall.uint::<u64>("`wall`")?;
                        self.set_wall_clock(
                            std::time::UNIX_EPOCH + core::time::Duration::from_secs(secs),
                        );
                    }
                    Ok(())
                }
                #[cfg(not(feature = "time"))]
                Err(needs_feature(node, "time"))
            }
            "env" => {
                #[cfg(feature = "env")]
                {
                    for child in node.block()? {
                        self.load_env(child)?;
                    }
                    Ok(())
                }
                #[cfg(not(feature = "env"))]
                Err(needs_feature(node, "env"))
            }
            "fs" => {
                #[cfg(feature = "fs")]
                {
                    for child in node.block()? {
                        self.load_fs(child, None)?;
                    }
                    Ok(())
                }
                #[cfg(not(feature = "fs"))]
                Err(needs_feature(node, "fs"))
            }
            "net" => {
                #[cfg(feature = "net")]
                {
                    for child in node.block()? {
                        if child.name() != "host" {
                            return Err(child.unknown("`net`", &["host"]));
                        }
                        child.no_children()?;
                        child.props_only(&[])?;
                        child.arity(2, Some(2), "a host name and an IP address")?;
                        let name = child.args()[0].string("the host name")?;
                        let ip = child.args()[1].string("the IP address")?;
                        let ip: core::net::IpAddr = ip.parse().map_err(|_| {
                            child.args()[1].error(format!("`{ip}` is not an IP address"))
                        })?;
                        self.add_host(name, ip);
                    }
                    Ok(())
                }
                #[cfg(not(feature = "net"))]
                Err(needs_feature(node, "net"))
            }
            "process" => {
                #[cfg(feature = "process")]
                {
                    for child in node.block()? {
                        self.load_command(child)?;
                    }
                    Ok(())
                }
                #[cfg(not(feature = "process"))]
                Err(needs_feature(node, "process"))
            }
            "stdio" => {
                #[cfg(feature = "io-std")]
                {
                    for child in node.block()? {
                        if child.name() != "stdin" {
                            return Err(child.unknown("`stdio`", &["stdin"]));
                        }
                        let bytes = contents(child, 0, "the input, or `hex=`")?;
                        self.feed_stdin(bytes);
                    }
                    Ok(())
                }
                #[cfg(not(feature = "io-std"))]
                Err(needs_feature(node, "io-std"))
            }
            "signal" => {
                #[cfg(feature = "signal")]
                {
                    for child in node.block()? {
                        if child.name() != "ctrl-c" {
                            return Err(child.unknown("`signal`", &["ctrl-c"]));
                        }
                        child.no_children()?;
                        child.props_only(&[])?;
                        child.arity(0, Some(1), "an optional count")?;
                        let n = match child.args().first() {
                            Some(n) => n.uint::<u32>("the count")?,
                            None => 1,
                        };
                        for _ in 0..n {
                            self.raise_ctrl_c();
                        }
                    }
                    Ok(())
                }
                #[cfg(not(feature = "signal"))]
                Err(needs_feature(node, "signal"))
            }
            _ => Err(node.unknown("the world", TOP)),
        }
    }

    #[cfg(feature = "env")]
    fn load_env(&mut self, node: &Node) -> Result<()> {
        node.no_children()?;
        node.props_only(&[])?;
        match node.name() {
            "var" => {
                node.arity(2, Some(2), "a name and a value")?;
                let name = node.args()[0].string("the name")?;
                let value = node.args()[1].string("the value")?;
                self.set_var(name, value);
            }
            "unset" => {
                node.arity(1, Some(1), "a name")?;
                self.remove_var(node.args()[0].string("the name")?);
            }
            "args" => {
                let args = node
                    .args()
                    .iter()
                    .map(|a| a.string("an argument").map(OsString::from))
                    .collect::<Result<Vec<_>>>()?;
                self.set_args(args);
            }
            "cwd" => {
                node.arity(1, Some(1), "a path")?;
                self.set_current_dir(node.args()[0].string("the path")?);
            }
            "temp" => {
                node.arity(1, Some(1), "a path")?;
                self.set_temp_dir(node.args()[0].string("the path")?);
            }
            _ => {
                return Err(node.unknown("`env`", &["var", "unset", "args", "cwd", "temp"]));
            }
        }
        Ok(())
    }

    #[cfg(feature = "fs")]
    fn load_fs(&mut self, node: &Node, parent: Option<&Path>) -> Result<()> {
        use seam_core::ops::fs::{create_dir_all, write_file};

        let path = match node.name() {
            "dir" | "file" => path_in(node, parent)?,
            _ => return Err(node.unknown("`fs`", &["dir", "file"])),
        };
        let failed = |e: seam_core::io::Error| {
            node.error(format!("cannot create `{}`: {e}", path.display()))
        };
        if node.name() == "dir" {
            node.props_only(&[])?;
            node.arity(1, Some(1), "a path, then a block of its contents")?;
            create_dir_all(self.sim(), &path).map_err(failed)?;
            for child in node.children() {
                self.load_fs(child, Some(&path))?;
            }
        } else {
            let bytes = contents(node, 1, "a path and its contents, or a path and `hex=`")?;
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                create_dir_all(self.sim(), dir).map_err(failed)?;
            }
            write_file(self.sim(), &path, &bytes).map_err(failed)?;
        }
        Ok(())
    }

    #[cfg(feature = "process")]
    fn load_command(&mut self, node: &Node) -> Result<()> {
        if node.name() != "command" {
            return Err(node.unknown("`process`", &["command"]));
        }
        node.no_children()?;
        node.props_only(&["exit", "stdout", "stderr"])?;
        node.arity(1, None, "a program, then any arguments it must be run with")?;
        let program = OsString::from(node.args()[0].string("the program")?);
        let args = node.args()[1..]
            .iter()
            .map(|a| a.string("an argument").map(OsString::from))
            .collect::<Result<Vec<_>>>()?;
        let code = match node.prop("exit") {
            Some(v) => Some(v.uint::<i32>("`exit`")?),
            None => Some(0),
        };
        let bytes = |key: &str| -> Result<Vec<u8>> {
            Ok(match node.prop(key) {
                Some(v) => v.string(key)?.as_bytes().to_vec(),
                None => Vec::new(),
            })
        };
        let output = seam_core::ops::process::Output {
            code,
            stdout: bytes("stdout")?,
            stderr: bytes("stderr")?,
        };
        self.script_command(program, (!args.is_empty()).then_some(args), output);
        Ok(())
    }

    #[allow(unused_variables, clippy::ptr_arg)] // `failures` is used by whichever blocks are on
    fn check_top(&self, node: &Node, failures: &mut Vec<Failure>) -> Result<()> {
        #[allow(unused_variables)]
        let fail = |failures: &mut Vec<Failure>, line: usize, message: String| {
            failures.push(Failure { line, message });
        };
        match node.name() {
            "clock" => {
                #[cfg(feature = "time")]
                {
                    use seam_core::Inspect;
                    use seam_core::ops::time::{Now, WallClock};

                    node.no_children()?;
                    node.arity(0, Some(0), "only `now=` and `wall=`")?;
                    node.props_only(&["now", "wall"])?;
                    if let Some(now) = node.prop("now") {
                        let want = core::time::Duration::from_secs(now.uint::<u64>("`now`")?);
                        let got = self.inspect(Now).since_epoch();
                        if got != want {
                            fail(
                                failures,
                                now.line(),
                                format!("clock: expected {want:?} since the start, found {got:?}"),
                            );
                        }
                    }
                    if let Some(wall) = node.prop("wall") {
                        let want = std::time::UNIX_EPOCH
                            + core::time::Duration::from_secs(wall.uint::<u64>("`wall`")?);
                        let got = self.inspect(WallClock);
                        if got != want {
                            let secs = |t: std::time::SystemTime| {
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_secs())
                            };
                            fail(
                                failures,
                                wall.line(),
                                format!(
                                    "wall clock: expected {} seconds after the epoch, found {}",
                                    secs(want),
                                    secs(got)
                                ),
                            );
                        }
                    }
                    Ok(())
                }
                #[cfg(not(feature = "time"))]
                Err(needs_feature(node, "time"))
            }
            "fs" => {
                #[cfg(feature = "fs")]
                {
                    for child in node.block()? {
                        self.check_fs(child, None, failures)?;
                    }
                    Ok(())
                }
                #[cfg(not(feature = "fs"))]
                Err(needs_feature(node, "fs"))
            }
            "stdio" => {
                #[cfg(feature = "io-std")]
                {
                    for child in node.block()? {
                        let got = match child.name() {
                            "stdout" => self.stdout(),
                            "stderr" => self.stderr(),
                            _ => {
                                return Err(child.unknown("`stdio` expectations",
                                    &["stdout", "stderr"],
                                ));
                            }
                        };
                        let want = contents(child, 0, "the expected output, or `hex=`")?;
                        if got != want {
                            fail(
                                failures,
                                child.line(),
                                format!(
                                    "{}: expected {}, found {}",
                                    child.name(),
                                    show(&want),
                                    show(&got)
                                ),
                            );
                        }
                    }
                    Ok(())
                }
                #[cfg(not(feature = "io-std"))]
                Err(needs_feature(node, "io-std"))
            }
            "process" => {
                #[cfg(feature = "process")]
                {
                    let mut want = Vec::new();
                    for child in node.block()? {
                        if child.name() != "ran" {
                            return Err(child.unknown("`process` expectations", &["ran"]));
                        }
                        child.no_children()?;
                        child.props_only(&["stdin"])?;
                        child.arity(1, None, "a program, then its arguments")?;
                        let words = child
                            .args()
                            .iter()
                            .map(|a| a.string("an argument").map(OsString::from))
                            .collect::<Result<Vec<_>>>()?;
                        let stdin = match child.prop("stdin") {
                            Some(v) => Some(v.string("`stdin`")?.as_bytes().to_vec()),
                            None => None,
                        };
                        want.push((child.line(), words, stdin));
                    }
                    let log = self.invocations();
                    let got: Vec<Vec<OsString>> = log
                        .iter()
                        .map(|i| {
                            core::iter::once(i.program.clone())
                                .chain(i.args.iter().cloned())
                                .collect()
                        })
                        .collect();
                    let line = |words: &[OsString]| {
                        words
                            .iter()
                            .map(|w| w.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    for (n, (pos, words, stdin)) in want.iter().enumerate() {
                        match got.get(n) {
                            Some(g) if g == words => {
                                let given = log[n].stdin();
                                if let Some(stdin) = stdin.as_ref().filter(|s| *s != given) {
                                    fail(
                                        failures,
                                        *pos,
                                        format!(
                                            "command {}: expected stdin {}, but it was given {}",
                                            n + 1,
                                            show(stdin),
                                            show(given)
                                        ),
                                    );
                                }
                            }
                            Some(g) => fail(
                                failures,
                                *pos,
                                format!(
                                    "command {}: expected `{}`, but it ran `{}`",
                                    n + 1,
                                    line(words),
                                    line(g)
                                ),
                            ),
                            None => fail(
                                failures,
                                *pos,
                                format!(
                                    "command {}: expected `{}`, but only {} ran",
                                    n + 1,
                                    line(words),
                                    got.len()
                                ),
                            ),
                        }
                    }
                    if got.len() > want.len() {
                        let extra: Vec<String> = got[want.len()..]
                            .iter()
                            .map(|g| format!("`{}`", line(g)))
                            .collect();
                        fail(
                            failures,
                            node.line(),
                            format!("more commands ran than expected: {}", extra.join(", ")),
                        );
                    }
                    Ok(())
                }
                #[cfg(not(feature = "process"))]
                Err(needs_feature(node, "process"))
            }
            "env" => {
                #[cfg(feature = "env")]
                {
                    use seam_core::Inspect;
                    use seam_core::ops::env::Var;

                    for child in node.block()? {
                        child.no_children()?;
                        child.props_only(&[])?;
                        let (name, want) = match child.name() {
                            "var" => {
                                child.arity(2, Some(2), "a name and a value")?;
                                let name = child.args()[0].string("the name")?;
                                (name, Some(child.args()[1].string("the value")?))
                            }
                            "unset" => {
                                child.arity(1, Some(1), "a name")?;
                                (child.args()[0].string("the name")?, None)
                            }
                            _ => {
                                return Err(child.unknown("`env` expectations",
                                    &["var", "unset"],
                                ));
                            }
                        };
                        let got = self.inspect(Var::new(name));
                        if got.as_deref() != want.map(std::ffi::OsStr::new) {
                            let show_var = |v: Option<&std::ffi::OsStr>| match v {
                                Some(v) => format!("{:?}", v.to_string_lossy()),
                                None => String::from("unset"),
                            };
                            fail(
                                failures,
                                child.line(),
                                format!(
                                    "env: expected {name} {}, found {}",
                                    show_var(want.map(std::ffi::OsStr::new)),
                                    show_var(got.as_deref())
                                ),
                            );
                        }
                    }
                    Ok(())
                }
                #[cfg(not(feature = "env"))]
                Err(needs_feature(node, "env"))
            }
            "seed" | "parallelism" | "net" | "signal" => Err(node.error(format!(
                    "`{}` does not change during a run, so it has no expectations; expectations cover `clock`, `env`, `fs`, `stdio` and `process`",
                    node.name()
                ),
            )),
            _ => Err(node.unknown("expectations",
                &["clock", "env", "fs", "stdio", "process"],
            )),
        }
    }

    #[cfg(feature = "fs")]
    fn check_fs(
        &self,
        node: &Node,
        parent: Option<&Path>,
        failures: &mut Vec<Failure>,
    ) -> Result<()> {
        use seam_core::Inspect;
        use seam_core::ops::fs::{FileKind, Metadata};

        let path = match node.name() {
            "dir" | "file" | "absent" => path_in(node, parent)?,
            _ => {
                return Err(node.unknown("`fs` expectations", &["dir", "file", "absent"]));
            }
        };
        let mut fail = |message: String| {
            failures.push(Failure {
                line: node.line(),
                message: format!("`{}`: {message}", path.display()),
            });
        };
        let kind = self.inspect(Metadata::new(&path)).ok().map(|m| m.kind);
        match node.name() {
            "absent" => {
                node.no_children()?;
                node.props_only(&[])?;
                node.arity(1, Some(1), "a path")?;
                if kind.is_some() {
                    fail(String::from("expected nothing there, but it exists"));
                }
            }
            "dir" => {
                node.props_only(&[])?;
                node.arity(1, Some(1), "a path, then a block of what it holds")?;
                match kind {
                    Some(FileKind::Dir) => {
                        for child in node.children() {
                            self.check_fs(child, Some(&path), failures)?;
                        }
                    }
                    Some(_) => fail(String::from("expected a directory, but it is a file")),
                    None => fail(String::from("expected a directory, but nothing is there")),
                }
            }
            _ => {
                let want = contents(node, 1, "a path and its expected contents, or `hex=`")?;
                match (kind, self.file(&path)) {
                    (_, Some(got)) if got == want => {}
                    (_, Some(got)) => {
                        fail(format!("expected {}, found {}", show(&want), show(&got)))
                    }
                    (Some(FileKind::Dir), None) => {
                        fail(String::from("expected a file, but it is a directory"))
                    }
                    _ => fail(String::from("expected a file, but nothing is there")),
                }
            }
        }
        Ok(())
    }
}
