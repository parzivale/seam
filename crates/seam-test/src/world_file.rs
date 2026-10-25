//! World files: a [`Sim`]'s world described in [KDL](https://kdl.dev).
//!
//! A world file sets up the world a core starts in, in one block per area
//! (the clock, the environment, the filesystem, ...):
//!
//! ```kdl
//! seed 7
//! clock now=0 wall=1767225600
//!
//! env {
//!     var USER ada
//!     args word_count "/a.txt"
//!     cwd "/home/ada"
//! }
//!
//! fs {
//!     dir "/home/ada" {
//!         file notes.txt "buy milk"
//!         dir projects {
//!             file seam.md """
//!                 sans-effect
//!                 """
//!         }
//!     }
//!     file "/data/blob.bin" hex="00ff10ab"
//! }
//!
//! net { host example.com "93.184.216.34" }
//! process { command git exit=0 stdout="clean\n" }
//! stdio { stdin "y\n" }
//! signal { ctrl-c }
//! ```
//!
//! Paths are quoted: KDL does not allow `/` in a bare word. Load a world
//! with [`Sim::from_world`], or onto an existing world with
//! [`Sim::load_world`]. Directives apply in order, top to bottom, as the
//! matching setup methods would; something set twice keeps its last value.
//! Directories nest, and a nested path is relative to its directory.
//!
//! # Directives
//!
//! | Block | Directive | Does |
//! |---|---|---|
//! | | `seed N` | `Sim::set_seed` |
//! | | `parallelism N` | `Sim::set_parallelism` |
//! | | `clock now=S wall=S` | advances the clock by `now` seconds, then sets the wall clock to `wall` seconds after the Unix epoch |
//! | `env` | `var NAME VALUE`, `unset NAME` | `Sim::set_var`, `Sim::remove_var` |
//! | `env` | `args A...`, `cwd P`, `temp P` | `Sim::set_args`, `Sim::set_current_dir`, `Sim::set_temp_dir` |
//! | `fs` | `dir P { ... }` | creates a directory and its contents |
//! | `fs` | `file P CONTENTS`, `file P hex="..."` | creates a file, and its parents |
//! | `net` | `host NAME IP` | `Sim::add_host` |
//! | `process` | `command PROGRAM [ARGS...] exit=N stdout=".." stderr=".."` | a fixed response; with arguments, only for exactly those |
//! | `stdio` | `stdin CONTENTS` | `Sim::feed_stdin` |
//! | `signal` | `ctrl-c [N]` | `Sim::raise_ctrl_c`, `N` times |
//!
//! Each block needs the seam-test feature of the same name (`io-std` for
//! `stdio`); a directive whose feature is off is an error saying so.
//! Multiline strings follow KDL: the closing `"""`'s indentation is removed
//! from every line, and the string does not end in a newline unless a
//! blank line comes before the `"""`.
//!
//! # Expectations
//!
//! The same format describes what the world should look like after a run,
//! for `Sim::check_world` and `Sim::assert_world`. Only what is listed
//! is checked:
//!
//! ```kdl
//! clock now=3
//! env { var MODE done; unset LOCK_HELD }
//! fs {
//!     file "/tmp/out/report.txt" "total 6\n"
//!     dir "/tmp/out" { file count.txt "1" }
//!     absent "/tmp/scratch"
//! }
//! stdio { stdout "done\n" }
//! process { ran git status; ran make test }
//! ```
//!
//! `var` expects a variable to hold exactly that value, and `unset` expects
//! it not to be set: a core can change them with `SetVar` and `RemoveVar`.
//! `stdout` and `stderr` compare exact bytes, and the `ran` lines must be
//! every command the core ran or spawned, in order. `ran cat stdin="hi"`
//! also checks what a spawned child was given on its standard input.

#[cfg(any(feature = "env", feature = "process"))]
use std::ffi::OsString;
use std::fmt;
use std::format;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::string::String;
use std::vec::Vec;

use crate::Sim;
use crate::kdl::{self, Kind, Pos};
pub use crate::kdl::{Node, Value};

/// A world file that cannot be loaded: it is not valid KDL, or it says
/// something the world does not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldError {
    /// The line the problem is on, from 1.
    pub line: usize,
    /// The column, from 1.
    pub column: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for WorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl core::error::Error for WorldError {}

/// A world that does not match its expectations, or expectations that
/// cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldMismatch {
    /// Each expectation that failed, in the order they are listed.
    pub failures: Vec<Failure>,
}

/// One failed expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The expectation's line, from 1.
    pub line: usize,
    /// What differs.
    pub message: String,
}

impl fmt::Display for WorldMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the world does not match its expectations:")?;
        for failure in &self.failures {
            write!(f, "\n  line {}: {}", failure.line, failure.message)?;
        }
        Ok(())
    }
}

impl core::error::Error for WorldMismatch {}

/// A result whose error is a [`WorldError`].
pub type Result<T> = core::result::Result<T, WorldError>;

fn error(pos: Pos, message: impl Into<String>) -> WorldError {
    WorldError {
        line: pos.line,
        column: pos.column,
        message: message.into(),
    }
}

fn parse(text: &str) -> Result<Vec<Node>> {
    kdl::parse(text).map_err(|e| error(e.pos, e.message))
}

/// Names at the top of a world file.
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

/// The edit distance between two names, for suggestions.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn suggest(name: &str, options: &[&str]) -> String {
    match options.iter().min_by_key(|o| distance(name, o)) {
        Some(best) if distance(name, best) <= 2 => format!(" (did you mean `{best}`?)"),
        _ => format!(
            " (expected one of: {})",
            options
                .iter()
                .map(|o| format!("`{o}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn unknown(node: &Node, within: &str, options: &[&str]) -> WorldError {
    error(
        node.pos,
        format!(
            "unknown `{}` in {within}{}",
            node.name,
            suggest(&node.name, options)
        ),
    )
}

#[allow(dead_code)]
fn needs_feature(node: &Node, feature: &str) -> WorldError {
    error(
        node.pos,
        format!("`{}` needs seam-test's `{feature}` feature", node.name),
    )
}

/// Reading a node, for a [`World`] part's own blocks. The checks return a
/// [`WorldError`] that points at the offending line and column.
impl Node {
    /// Its name: `file` in `file "/a" "x"`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its line, from 1.
    pub fn line(&self) -> usize {
        self.pos.line
    }

    /// Its column, from 1.
    pub fn column(&self) -> usize {
        self.pos.column
    }

    /// Its arguments, in order.
    pub fn args(&self) -> &[Value] {
        &self.args
    }

    /// Its block: the nodes in braces after it.
    pub fn children(&self) -> &[Node] {
        &self.children
    }

    /// An error about this node.
    pub fn error(&self, message: impl Into<String>) -> WorldError {
        error(self.pos, message)
    }

    /// A failed expectation at this node.
    pub fn failure(&self, message: impl Into<String>) -> Failure {
        Failure {
            line: self.pos.line,
            message: message.into(),
        }
    }

    /// An error for a node that is not one of `options`, inside `within`,
    /// suggesting the closest.
    pub fn unknown(&self, within: &str, options: &[&str]) -> WorldError {
        unknown(self, within, options)
    }

    /// Checks that it has no block.
    pub fn no_children(&self) -> Result<()> {
        match self.children.first() {
            Some(child) => Err(error(child.pos, format!("`{}` takes no block", self.name))),
            None => Ok(()),
        }
    }

    /// Its block, checking that it has no arguments or properties.
    pub fn block(&self) -> Result<&[Node]> {
        if let Some(arg) = self.args.first() {
            return Err(error(
                arg.pos,
                format!("`{}` takes a block, not arguments", self.name),
            ));
        }
        self.props_only(&[])?;
        Ok(&self.children)
    }

    /// Checks that every property is one of `allowed`.
    pub fn props_only(&self, allowed: &[&str]) -> Result<()> {
        for (key, value) in &self.props {
            if !allowed.contains(&key.as_str()) {
                let hint = if allowed.is_empty() {
                    String::from(" (it takes none)")
                } else {
                    suggest(key, allowed)
                };
                return Err(error(
                    value.pos,
                    format!("unknown property `{key}` on `{}`{hint}", self.name),
                ));
            }
        }
        Ok(())
    }

    /// A property's value; the last one, if it repeats.
    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.props
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Checks the number of arguments, with `usage` saying what they are.
    pub fn arity(&self, min: usize, max: Option<usize>, usage: &str) -> Result<()> {
        let n = self.args.len();
        if n < min || max.is_some_and(|max| n > max) {
            let pos = self.args.get(max.unwrap_or(n)).map_or(self.pos, |a| a.pos);
            return Err(error(pos, format!("`{}` takes {usage}", self.name)));
        }
        Ok(())
    }
}

/// Reading a value. `what` names it in the error, as in "the seed must be
/// an integer".
impl Value {
    /// Its line, from 1.
    pub fn line(&self) -> usize {
        self.pos.line
    }

    /// Its column, from 1.
    pub fn column(&self) -> usize {
        self.pos.column
    }

    /// An error about this value.
    pub fn error(&self, message: impl Into<String>) -> WorldError {
        error(self.pos, message)
    }

    /// The string it is.
    pub fn string(&self, what: &str) -> Result<&str> {
        match &self.kind {
            Kind::Str(s) => Ok(s),
            k => Err(error(
                self.pos,
                format!("{what} must be a string, not {}", k.describe()),
            )),
        }
    }

    /// The integer it is.
    pub fn int(&self, what: &str) -> Result<i128> {
        match &self.kind {
            Kind::Int(n) => Ok(*n),
            k => Err(error(
                self.pos,
                format!("{what} must be an integer, not {}", k.describe()),
            )),
        }
    }

    /// The integer it is, which must fit in `T`.
    pub fn uint<T: TryFrom<i128>>(&self, what: &str) -> Result<T> {
        let n = self.int(what)?;
        T::try_from(n).map_err(|_| error(self.pos, format!("{what} is out of range: {n}")))
    }

    /// The boolean it is: `#true` or `#false`.
    pub fn bool(&self, what: &str) -> Result<bool> {
        match &self.kind {
            Kind::Bool(b) => Ok(*b),
            k => Err(error(
                self.pos,
                format!("{what} must be `#true` or `#false`, not {}", k.describe()),
            )),
        }
    }
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
        return Err(error(v.pos, "`hex` needs an even number of digits"));
    }
    digits
        .chunks(2)
        .map(|pair| {
            let s: String = pair.iter().collect();
            u8::from_str_radix(&s, 16).map_err(|_| error(v.pos, format!("`{s}` is not a hex byte")))
        })
        .collect()
}

/// The contents of a `file`, `stdin`, `stdout` or `stderr` node: the
/// argument at `at`, or a `hex` property.
#[allow(dead_code)]
fn contents(node: &Node, at: usize, usage: &str) -> Result<Vec<u8>> {
    node.props_only(&["hex"])?;
    node.no_children()?;
    match (node.args.get(at), node.prop("hex")) {
        (Some(_), Some(h)) => Err(error(h.pos, "give the contents or `hex`, not both")),
        (Some(arg), None) => {
            node.arity(at + 1, Some(at + 1), usage)?;
            Ok(arg.string("the contents")?.as_bytes().to_vec())
        }
        (None, Some(h)) => {
            node.arity(at, Some(at), usage)?;
            hex(h)
        }
        (None, None) => Err(error(node.pos, format!("`{}` takes {usage}", node.name))),
    }
}

/// A path in an `fs` block: absolute at the top, relative inside a `dir`.
#[allow(dead_code)]
fn path_in(node: &Node, parent: Option<&Path>) -> Result<PathBuf> {
    let arg = node
        .args
        .first()
        .ok_or_else(|| error(node.pos, format!("`{}` takes a path first", node.name)))?;
    let path = Path::new(arg.string("a path")?);
    match parent {
        None => Ok(path.to_path_buf()),
        Some(_) if path.is_absolute() => {
            Err(error(arg.pos, "inside a `dir`, paths are relative to it"))
        }
        Some(parent) => Ok(parent.join(path)),
    }
}

impl Sim {
    /// A fresh world, set up by a world file. See [the format](crate::world_file).
    ///
    /// ```
    /// # #[cfg(all(feature = "fs", feature = "env"))] {
    /// use seam_test::Sim;
    ///
    /// let sim = Sim::from_world(r#"
    ///     env { var USER ada }
    ///     fs {
    ///         dir "/home/ada" { file notes.txt "buy milk" }
    ///     }
    /// "#).unwrap();
    /// assert_eq!(sim.file("/home/ada/notes.txt").unwrap(), b"buy milk");
    /// # }
    /// ```
    pub fn from_world(text: &str) -> core::result::Result<Self, WorldError> {
        let mut sim = Self::new();
        sim.load_world(text)?;
        Ok(sim)
    }

    /// Sets up this world further from a world file, after what is already
    /// set: a base world, then a test's own changes. On an error, the
    /// directives before it have been applied.
    pub fn load_world(&mut self, text: &str) -> core::result::Result<&mut Self, WorldError> {
        for node in &parse(text)? {
            self.load_top(node)?;
        }
        Ok(self)
    }

    /// Checks this world against expectations written as a world file.
    /// Every failing expectation is reported, with its line. See
    /// [the format](crate::world_file#expectations).
    pub fn check_world(&self, text: &str) -> core::result::Result<(), WorldMismatch> {
        let invalid = |e: WorldError| WorldMismatch {
            failures: std::vec![Failure {
                line: e.line,
                message: format!("invalid expectations (column {}): {}", e.column, e.message),
            }],
        };
        let nodes = parse(text).map_err(invalid)?;
        let mut failures = Vec::new();
        for node in &nodes {
            self.check_top(node, &mut failures).map_err(invalid)?;
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(WorldMismatch { failures })
        }
    }

    /// Like [`check_world`](Self::check_world), but panics with the
    /// failures, for use in tests.
    ///
    /// # Panics
    ///
    /// If the world does not match, or the expectations cannot be read.
    #[track_caller]
    pub fn assert_world(&self, text: &str) {
        if let Err(mismatch) = self.check_world(text) {
            panic!("{mismatch}");
        }
    }

    fn load_top(&mut self, node: &Node) -> Result<()> {
        match node.name.as_str() {
            "seed" => {
                #[cfg(feature = "random")]
                {
                    node.no_children()?;
                    node.props_only(&[])?;
                    node.arity(1, Some(1), "a seed")?;
                    let seed = node.args[0].uint::<u64>("the seed")?;
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
                    let n = node.args[0].uint::<usize>("parallelism")?;
                    let n = core::num::NonZeroUsize::new(n)
                        .ok_or_else(|| error(node.args[0].pos, "parallelism must be at least 1"))?;
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
                        if child.name != "host" {
                            return Err(unknown(child, "`net`", &["host"]));
                        }
                        child.no_children()?;
                        child.props_only(&[])?;
                        child.arity(2, Some(2), "a host name and an IP address")?;
                        let name = child.args[0].string("the host name")?;
                        let ip = child.args[1].string("the IP address")?;
                        let ip: core::net::IpAddr = ip.parse().map_err(|_| {
                            error(child.args[1].pos, format!("`{ip}` is not an IP address"))
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
                        if child.name != "stdin" {
                            return Err(unknown(child, "`stdio`", &["stdin"]));
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
                        if child.name != "ctrl-c" {
                            return Err(unknown(child, "`signal`", &["ctrl-c"]));
                        }
                        child.no_children()?;
                        child.props_only(&[])?;
                        child.arity(0, Some(1), "an optional count")?;
                        let n = match child.args.first() {
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
            _ => Err(unknown(node, "the world", TOP)),
        }
    }

    #[cfg(feature = "env")]
    fn load_env(&mut self, node: &Node) -> Result<()> {
        node.no_children()?;
        node.props_only(&[])?;
        match node.name.as_str() {
            "var" => {
                node.arity(2, Some(2), "a name and a value")?;
                let name = node.args[0].string("the name")?;
                let value = node.args[1].string("the value")?;
                self.set_var(name, value);
            }
            "unset" => {
                node.arity(1, Some(1), "a name")?;
                self.remove_var(node.args[0].string("the name")?);
            }
            "args" => {
                let args = node
                    .args
                    .iter()
                    .map(|a| a.string("an argument").map(OsString::from))
                    .collect::<Result<Vec<_>>>()?;
                self.set_args(args);
            }
            "cwd" => {
                node.arity(1, Some(1), "a path")?;
                self.set_current_dir(node.args[0].string("the path")?);
            }
            "temp" => {
                node.arity(1, Some(1), "a path")?;
                self.set_temp_dir(node.args[0].string("the path")?);
            }
            _ => {
                return Err(unknown(
                    node,
                    "`env`",
                    &["var", "unset", "args", "cwd", "temp"],
                ));
            }
        }
        Ok(())
    }

    #[cfg(feature = "fs")]
    fn load_fs(&mut self, node: &Node, parent: Option<&Path>) -> Result<()> {
        use seam_core::ops::fs::{create_dir_all, write_file};

        let path = match node.name.as_str() {
            "dir" | "file" => path_in(node, parent)?,
            _ => return Err(unknown(node, "`fs`", &["dir", "file"])),
        };
        let failed = |e: seam_core::io::Error| {
            error(node.pos, format!("cannot create `{}`: {e}", path.display()))
        };
        if node.name == "dir" {
            node.props_only(&[])?;
            node.arity(1, Some(1), "a path, then a block of its contents")?;
            create_dir_all(self, &path).map_err(failed)?;
            for child in &node.children {
                self.load_fs(child, Some(&path))?;
            }
        } else {
            let bytes = contents(node, 1, "a path and its contents, or a path and `hex=`")?;
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                create_dir_all(self, dir).map_err(failed)?;
            }
            write_file(self, &path, &bytes).map_err(failed)?;
        }
        Ok(())
    }

    #[cfg(feature = "process")]
    fn load_command(&mut self, node: &Node) -> Result<()> {
        if node.name != "command" {
            return Err(unknown(node, "`process`", &["command"]));
        }
        node.no_children()?;
        node.props_only(&["exit", "stdout", "stderr"])?;
        node.arity(1, None, "a program, then any arguments it must be run with")?;
        let program = OsString::from(node.args[0].string("the program")?);
        let args = node.args[1..]
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
        let fail = |failures: &mut Vec<Failure>, pos: Pos, message: String| {
            failures.push(Failure {
                line: pos.line,
                message,
            });
        };
        match node.name.as_str() {
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
                                now.pos,
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
                                wall.pos,
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
                        let got = match child.name.as_str() {
                            "stdout" => self.stdout(),
                            "stderr" => self.stderr(),
                            _ => {
                                return Err(unknown(
                                    child,
                                    "`stdio` expectations",
                                    &["stdout", "stderr"],
                                ));
                            }
                        };
                        let want = contents(child, 0, "the expected output, or `hex=`")?;
                        if got != want {
                            fail(
                                failures,
                                child.pos,
                                format!(
                                    "{}: expected {}, found {}",
                                    child.name,
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
                        if child.name != "ran" {
                            return Err(unknown(child, "`process` expectations", &["ran"]));
                        }
                        child.no_children()?;
                        child.props_only(&["stdin"])?;
                        child.arity(1, None, "a program, then its arguments")?;
                        let words = child
                            .args
                            .iter()
                            .map(|a| a.string("an argument").map(OsString::from))
                            .collect::<Result<Vec<_>>>()?;
                        let stdin = match child.prop("stdin") {
                            Some(v) => Some(v.string("`stdin`")?.as_bytes().to_vec()),
                            None => None,
                        };
                        want.push((child.pos, words, stdin));
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
                            node.pos,
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
                        let (name, want) = match child.name.as_str() {
                            "var" => {
                                child.arity(2, Some(2), "a name and a value")?;
                                let name = child.args[0].string("the name")?;
                                (name, Some(child.args[1].string("the value")?))
                            }
                            "unset" => {
                                child.arity(1, Some(1), "a name")?;
                                (child.args[0].string("the name")?, None)
                            }
                            _ => {
                                return Err(unknown(
                                    child,
                                    "`env` expectations",
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
                                child.pos,
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
            "seed" | "parallelism" | "net" | "signal" => Err(error(
                node.pos,
                format!(
                    "`{}` does not change during a run, so it has no expectations; expectations cover `clock`, `env`, `fs`, `stdio` and `process`",
                    node.name
                ),
            )),
            _ => Err(unknown(
                node,
                "expectations",
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

        let path = match node.name.as_str() {
            "dir" | "file" | "absent" => path_in(node, parent)?,
            _ => {
                return Err(unknown(
                    node,
                    "`fs` expectations",
                    &["dir", "file", "absent"],
                ));
            }
        };
        let mut fail = |message: String| {
            failures.push(Failure {
                line: node.pos.line,
                message: format!("`{}`: {message}", path.display()),
            });
        };
        let kind = self.inspect(Metadata::new(&path)).ok().map(|m| m.kind);
        match node.name.as_str() {
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
                        for child in &node.children {
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

/// A simulated part's share of a world file: the top-level blocks it reads,
/// set up from them, and checked against them after a run.
///
/// [`Sim`] reads the blocks above. A part of a program's own, or of another
/// crate's, reads its own, so one world file sets up and checks a whole
/// root with [`load_world`] and [`check_world`]:
///
/// ```
/// use seam_test::world_file::{self, Failure, Node, World, WorldError};
///
/// /// A fake mount table.
/// #[derive(Default)]
/// struct Mounts(Vec<(String, String)>);
///
/// impl World for Mounts {
///     const BLOCKS: &'static [&'static str] = &["mounts"];
///
///     /// `mounts { bind "/srv/a" "/mnt/a" }` mounts one, before the run.
///     fn load(&mut self, block: &Node) -> Result<(), WorldError> {
///         for node in block.block()? {
///             match node.name() {
///                 "bind" => {
///                     node.arity(2, Some(2), "a source and a target")?;
///                     let (from, to) = (&node.args()[0], &node.args()[1]);
///                     self.0.push((from.string("the source")?.into(), to.string("the target")?.into()));
///                 }
///                 _ => return Err(node.unknown("`mounts`", &["bind"])),
///             }
///         }
///         Ok(())
///     }
///
///     /// The same, after: each `bind` must be in place.
///     fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> Result<(), WorldError> {
///         for node in block.block()? {
///             node.arity(2, Some(2), "a source and a target")?;
///             let from = node.args()[0].string("the source")?;
///             let to = node.args()[1].string("the target")?;
///             if !self.0.iter().any(|(f, t)| f == from && t == to) {
///                 failures.push(node.failure(format!("{to} is not bound to {from}")));
///             }
///         }
///         Ok(())
///     }
/// }
/// # let mut m = Mounts::default();
/// # m.load(&world_file::parse_one(r#"mounts { bind "/a" "/b" }"#).unwrap()).unwrap();
/// # assert_eq!(m.0.len(), 1);
/// ```
pub trait World {
    /// The top-level blocks it reads, by name.
    const BLOCKS: &'static [&'static str];

    /// Sets it up from one of its blocks.
    fn load(&mut self, block: &Node) -> Result<()>;

    /// Checks it against one of its blocks, adding a [`Failure`] for each
    /// expectation that does not hold. An `Err` is for expectations that
    /// cannot be read.
    fn check(&self, block: &Node, failures: &mut Vec<Failure>) -> Result<()>;
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

/// Parses a world file holding one node, for testing a [`World`] part on
/// its own.
pub fn parse_one(text: &str) -> Result<Node> {
    let mut nodes = parse(text)?;
    match nodes.len() {
        1 => Ok(nodes.remove(0)),
        n => Err(WorldError {
            line: 1,
            column: 1,
            message: format!("expected one node, found {n}"),
        }),
    }
}

/// The effects whose parts take part in [`load_world`] and
/// [`check_world`]: a tuple of up to 32 keys, each served by a [`World`]
/// part of the root.
pub trait WorldKeys<A> {
    /// The blocks the part of the key at `i` reads, or `None` past the end.
    #[doc(hidden)]
    fn blocks(i: usize) -> Option<&'static [&'static str]>;
    /// Where the part of the key at `i` is, to use a part reached through
    /// several keys once.
    #[doc(hidden)]
    fn place(parts: &A, i: usize) -> (usize, core::any::TypeId);
    #[doc(hidden)]
    fn load(parts: &mut A, i: usize, block: &Node) -> Result<()>;
    #[doc(hidden)]
    fn check(parts: &A, i: usize, block: &Node, failures: &mut Vec<Failure>) -> Result<()>;
}

macro_rules! world_keys {
    ([$($k:ident $i:tt)*]) => {};
    ([$($k:ident $i:tt)*] $next:ident $ni:tt $($rest:tt)*) => {
        impl<A, $($k,)* $next> WorldKeys<A> for ($($k,)* $next,)
        where
            $($k: seam_core::Pick<A>, $k::Part: World + 'static,)*
            $next: seam_core::Pick<A>,
            $next::Part: World + 'static,
        {
            fn blocks(i: usize) -> Option<&'static [&'static str]> {
                match i {
                    $($i => Some(<$k::Part as World>::BLOCKS),)*
                    $ni => Some(<$next::Part as World>::BLOCKS),
                    _ => None,
                }
            }

            fn place(parts: &A, i: usize) -> (usize, core::any::TypeId) {
                match i {
                    $($i => (
                        core::ptr::from_ref($k::pick(parts)).addr(),
                        core::any::TypeId::of::<$k::Part>(),
                    ),)*
                    _ => (
                        core::ptr::from_ref($next::pick(parts)).addr(),
                        core::any::TypeId::of::<$next::Part>(),
                    ),
                }
            }

            fn load(parts: &mut A, i: usize, block: &Node) -> Result<()> {
                match i {
                    $($i => World::load($k::pick_mut(parts), block),)*
                    _ => World::load($next::pick_mut(parts), block),
                }
            }

            fn check(parts: &A, i: usize, block: &Node, failures: &mut Vec<Failure>) -> Result<()> {
                match i {
                    $($i => World::check($k::pick(parts), block, failures),)*
                    _ => World::check($next::pick(parts), block, failures),
                }
            }
        }

        world_keys!([$($k $i)* $next $ni] $($rest)*);
    };
}

world_keys!([]
    K0 0 K1 1 K2 2 K3 3 K4 4 K5 5 K6 6 K7 7 K8 8 K9 9 K10 10 K11 11 K12 12 K13 13 K14 14 K15 15
    K16 16 K17 17 K18 18 K19 19 K20 20 K21 21 K22 22 K23 23 K24 24 K25 25 K26 26 K27 27 K28 28
    K29 29 K30 30 K31 31
);

/// The key whose part reads `name`: the first listed. Two different parts
/// that read the same block are an error.
fn owner<K: WorldKeys<A>, A>(parts: &A, node: &Node) -> Result<usize> {
    let mut found: Option<(usize, (usize, core::any::TypeId))> = None;
    let mut all: Vec<&str> = Vec::new();
    let mut i = 0;
    while let Some(blocks) = K::blocks(i) {
        all.extend(blocks.iter().copied());
        if blocks.contains(&node.name.as_str()) {
            let place = K::place(parts, i);
            match found {
                None => found = Some((i, place)),
                Some((_, first)) if first == place => {}
                Some(_) => {
                    return Err(node.error(format!(
                        "two parts of the root read `{}`; list one of them",
                        node.name
                    )));
                }
            }
        }
        i += 1;
    }
    match found {
        Some((i, _)) => Ok(i),
        None => {
            all.sort_unstable();
            all.dedup();
            Err(node.error(format!(
                "no part of the root reads `{}`{}",
                node.name,
                suggest(&node.name, &all)
            )))
        }
    }
}

/// Sets up the parts of `root` serving the effects `K` from a world file:
/// each block goes to the part that reads it, and a part serving several of
/// the effects is set up once.
///
/// ```
/// # #[cfg(all(feature = "fs", feature = "env"))] {
/// use seam_core::effects as fx;
/// use seam_test::Sim;
///
/// let mut root = seam_core::root![Sim<fx::Var, fx::View> = Sim::new()];
/// seam_test::load_world::<(fx::Var, fx::View), _>(&mut root, r#"
///     env { var USER ada }
///     fs { file "/notes.txt" "buy milk" }
/// "#).unwrap();
/// seam_test::check_world::<(fx::View,), _>(&root, r#"fs { file "/notes.txt" "buy milk" }"#).unwrap();
/// # }
/// ```
pub fn load_world<K: WorldKeys<A>, A>(
    root: &mut seam_core::Root<A>,
    text: &str,
) -> core::result::Result<(), WorldError> {
    for node in &parse(text)? {
        let i = owner::<K, A>(root.parts(), node)?;
        K::load(root.parts_mut(), i, node)?;
    }
    Ok(())
}

/// Checks the parts of `root` serving the effects `K` against expectations
/// written as a world file. Every failing expectation is reported, with its
/// line.
pub fn check_world<K: WorldKeys<A>, A>(
    root: &seam_core::Root<A>,
    text: &str,
) -> core::result::Result<(), WorldMismatch> {
    let invalid = |e: WorldError| WorldMismatch {
        failures: std::vec![Failure {
            line: e.line,
            message: format!("invalid expectations (column {}): {}", e.column, e.message),
        }],
    };
    let mut failures = Vec::new();
    for node in &parse(text).map_err(invalid)? {
        let i = owner::<K, A>(root.parts(), node).map_err(invalid)?;
        K::check(root.parts(), i, node, &mut failures).map_err(invalid)?;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorldMismatch { failures })
    }
}
