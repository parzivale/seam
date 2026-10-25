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

use std::fmt;
use std::format;
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
        load(self, text)?;
        Ok(self)
    }

    /// Checks this world against expectations written as a world file.
    /// Every failing expectation is reported, with its line. See
    /// [the format](crate::world_file#expectations).
    pub fn check_world(&self, text: &str) -> core::result::Result<(), WorldMismatch> {
        check(self, text)
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
}

/// A simulated part's share of a world file: the top-level blocks it reads,
/// set up from them, and checked against them after a run.
///
/// [`Sim`] reads the blocks above. A part of a program's own, or of another
/// crate's, reads its own, so one world file sets up and checks a whole
/// root with [`load_world`] and [`check_world`], or one part on its own
/// with [`load`] and [`check`]. `Sim`'s own impl is written the same way,
/// over this module's public API and `Sim`'s public methods only; a test
/// compiles it outside the crate to keep it so.
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

/// Sets up one part from a world file, outside a root: each block must be
/// one of its [`World::BLOCKS`]. [`Sim::load_world`] is this.
pub fn load<W: World>(part: &mut W, text: &str) -> core::result::Result<(), WorldError> {
    for node in &parse(text)? {
        if !W::BLOCKS.contains(&node.name()) {
            return Err(node.unknown("the world", W::BLOCKS));
        }
        part.load(node)?;
    }
    Ok(())
}

/// Checks one part against expectations written as a world file, outside
/// a root. [`Sim::check_world`] is this.
pub fn check<W: World>(part: &W, text: &str) -> core::result::Result<(), WorldMismatch> {
    let mut failures = Vec::new();
    for node in &parse(text).map_err(invalid)? {
        if !W::BLOCKS.contains(&node.name()) {
            return Err(invalid(node.unknown("the world", W::BLOCKS)));
        }
        part.check(node, &mut failures).map_err(invalid)?;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorldMismatch { failures })
    }
}

/// Expectations that cannot be read, as a mismatch.
fn invalid(e: WorldError) -> WorldMismatch {
    WorldMismatch {
        failures: std::vec![Failure {
            line: e.line,
            message: format!("invalid expectations (column {}): {}", e.column, e.message),
        }],
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
/// let mut root = seam_core::root![Sim<fx::Var, fx::View>::new()];
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
