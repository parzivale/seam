# World files

A world file describes a simulated world in [KDL](https://kdl.dev): the
world a core starts in, what it should look like after a run, or a
snapshot of it. One block per *state* of the world (`clock`, `env`, `fs`,
`net`, ...), with directories that nest the way they do on disk.

Each block is read by the state that names it. The built-in states read
the blocks on this page; a state of a program's own reads its own (see
[Blocks of your own](#blocks-of-your-own)). Nothing about the built-in
blocks is special.

```kdl
seed 7
clock now=0 wall=1767225600

env {
    var USER ada
    args word_count "/home/ada/notes.txt"
    cwd "/home/ada"
}

fs {
    dir "/home/ada" {
        file notes.txt "buy milk"
        dir projects {
            file seam.md """
                sans-effect
                """
        }
    }
    file "/data/blob.bin" hex="00ff10ab"
}

net { host example.com "93.184.216.34" }
process { command git exit=0 stdout="clean\n" }
stdio { stdin "y\n" }
signal { ctrl-c }
```

## Using one

| Call | Does |
|---|---|
| `Sim::from_world(text)` | a fresh `Sim`, every built-in state, set up from the file |
| `world.load_world(text)` | sets any world up further: a shared base, then a test's own changes |
| `world.check_world(text)` | checks a world against expectations; every failure, with its line |
| `world.assert_world(text)` | the same, panicking with the failures |
| `world.to_world()` | the whole world, written out as a world file |
| `world_file::load(&mut state, text)`, `world_file::check(&state, text)` | one state of your own, on its own |

```rust
# #[cfg(all(feature = "test", feature = "fs", feature = "env", feature = "io-std"))] {
use seam::test::Sim;

const BASE: &str = r#"
    env { var USER ada }
    fs { file "/etc/app.conf" "port = 80" }
"#;

let mut sim = Sim::from_world(BASE).unwrap();
sim.load_world(r#"env { var USER bob }"#).unwrap(); // the last setting wins
let mismatch = sim
    .check_world(r#"
        env { var USER ada }
        fs { file "/etc/app.conf" "port = 8080" }
    "#)
    .unwrap_err();
assert_eq!(mismatch.failures.len(), 2);
assert_eq!(mismatch.failures[0].line, 2);
# }
```

## Syntax

World files use a subset of KDL v2:

- **Nodes** have a name, then arguments, then `key=value` properties, then
  optionally a block in braces. A node ends at a line break or `;`, and a
  `\` continues it onto the next line.
- **Comments**: `//` to the end of the line, `/* .. */` (nesting), and
  `/-` to comment out the next node, argument, property or block.
- **Strings**: bare words (`ada`, `notes.txt`), quoted strings with escapes
  (`"clean\n"`), raw strings (`#"C:\path"#`), and multiline strings:

  ```kdl
  file notes.md """
      first line
      second line
      """
  ```

  The closing `"""`'s indentation is removed from every line. The string
  does not end in a newline unless a blank line comes before the `"""`.
- **Numbers**: integers only, in decimal, `0x`, `0o` or `0b`, with `_`
  separators.
- **Keywords**: `#true`, `#false`, `#null`.

Paths are quoted, since KDL does not allow `/` in a bare word; the parser
says so when it meets one. Type annotations and fractional numbers are
errors.

## Setting up

Directives apply top to bottom, as the matching `Sim` setup methods would,
and something set twice keeps its last value.

| Block | Directive | Does | Feature |
|---|---|---|---|
| | `seed N` | `World::set_seed` | `random` |
| | `parallelism N` | `World::set_parallelism` (at least 1) | `rt` |
| | `clock now=S wall=S` | advances both clocks `now` seconds, then sets the wall clock to `wall` seconds after the Unix epoch; either is optional, and `now-ns=` / `wall-ns=` give nanoseconds | `time` |
| `env` | `var NAME VALUE` | `World::set_var` | `env` |
| `env` | `unset NAME` | `World::remove_var` | `env` |
| `env` | `args A...` | `World::set_args`, including the program name | `env` |
| `env` | `cwd P` | `World::set_current_dir` | `env` |
| `env` | `temp P` | `World::set_temp_dir` | `env` |
| `fs` | `dir P { ... }` | creates a directory, its parents and its contents | `fs` |
| `fs` | `file P CONTENTS` or `file P hex="00ff"` | creates a file and its parents | `fs` |
| `net` | `host NAME IP` | `World::add_host` | `net` |
| `process` | `command PROGRAM [ARGS...] exit=N stdout=".." stderr=".."` | a fixed response, for exactly those arguments if any are given; `exit` defaults to 0 | `process` |
| `process` | `ran PROGRAM [ARGS...] stdin=".."` | adds to the log of commands run, as if it had run | `process` |
| `stdio` | `stdin CONTENTS` or `stdin hex=".."` | `World::feed_stdin` | `io-std` |
| `stdio` | `stdout ..`, `stderr ..` (or `hex=`) | output already written | `io-std` |
| `signal` | `ctrl-c [N]` | `World::raise_ctrl_c`, `N` times (default 1) | `signal` |

Inside a `dir`, paths are relative to it, and an absolute one is an error.
In `hex`, spaces and `_` are ignored.

Later `command` responses win, and one for exact arguments wins over one
for any arguments. A command that is scripted, but not with the arguments
it was run with, fails with `NotFound`.

## Expectations

The same format describes what the world should look like afterwards.
Only what is listed is checked, and every expectation is checked before
reporting, so one run shows every difference.

```kdl
clock now=3
env { var MODE done; unset LOCK_HELD }
fs {
    file "/tmp/out/report.txt" "total 6\n"
    dir "/tmp/out" { file count.txt "1" }
    absent "/tmp/scratch"
}
stdio { stdout "done\n"; stderr "" }
process { ran git status; ran cat stdin="hi" }
net { host example.com "93.184.216.34" }
```

By default, an expected block holds when it is a *subset* of what the
state writes out (`to_world`): each expected node is there, with the same
name and arguments and at least the expected properties, and each node in
an expected block holds the same way inside it. A state whose
expectations have another shape checks them itself; the built-in ones do,
as below.

| Block | Expectation | Holds when |
|---|---|---|
| | `clock now=S wall=S` | the clock reads `S` seconds since the start; the wall clock, `S` seconds after the epoch |
| `env` | `var NAME VALUE` | the variable is set, to exactly that |
| `env` | `unset NAME` | the variable is not set |
| `fs` | `file P CONTENTS` or `file P hex=".."` | a file is there, with exactly those bytes |
| `fs` | `dir P { ... }` | a directory is there, and what its block lists holds inside it |
| `fs` | `absent P` | nothing is there |
| `stdio` | `stdout ..`, `stderr ..` (or `hex=`) | everything written so far is exactly that |
| `process` | `ran PROGRAM [ARGS...] [stdin=".."]` | the `ran` lines are every command run or spawned, in order; `stdin` also checks what a child was given |
| `net`, `seed`, `parallelism`, `signal` | as written out | a subset of the state's `to_world` |

A failure names the expectation's line and what differs:

```text
the world does not match its expectations:
  line 3: `/tmp/out/report.txt`: expected "total 6\n", found "total 5\n"
  line 7: stdout: expected "done\n", found ""
```

## Writing a world out

`world.to_world()` writes every state's blocks, in the order the states
were added, so a test can snapshot a world, diff two of them, or start a
new world file from a running one:

```rust
# #[cfg(all(feature = "test", feature = "fs", feature = "env", feature = "time"))] {
use seam::effects as fx;
use seam::ops::fs::write_file;

let mut world = seam::test::world![fx::Var, fx::View, fx::Open<seam::ops::fs::WriteOnly>];
world.load_world(r#"env { var USER ada }"#).unwrap();
write_file(&mut world, "/tmp/out", "ok").unwrap();
let text = world.to_world();
assert!(text.contains(r#"var USER ada"#));
assert!(text.contains(r#"file "/tmp/out" ok"#));

// It loads back to the same world.
let mut copy = seam::test::world![fx::Var, fx::View, fx::Open<seam::ops::fs::WriteOnly>];
copy.load_world(&text).unwrap();
assert_eq!(copy.to_world(), text);
# }
```

Bytes that are not UTF-8 are written as `hex=`. A clock that is not on a
whole second is written in nanoseconds (`now-ns=`). The log of commands
run is written as `ran` lines, which load back into the log. Scripted
command *handlers* written in Rust have no world-file form; only fixed
`command` responses are written out.

## Errors

A mistake in a world file is a `WorldError`, with the line and column of
what is wrong and a suggestion when a name is close to a known one:

```rust
# #[cfg(all(feature = "test", feature = "fs", feature = "io-std"))] {
let err = seam::test::Sim::from_world("stdo { stdin x }").unwrap_err();
assert_eq!(
    err.to_string(),
    "line 1, column 1: unknown `stdo` in the world (did you mean `stdio`?)"
);
# }
```

A block whose feature is off is an error naming the feature
("`net` needs seam-test's `net` feature"). Expectations that cannot be read
are reported as a single failure, at their line.

## Blocks of your own

A state of a program's own implements `seam::test::sim::SimState`: the
blocks it reads, `load` to set it up from one, and `to_world` to write it
out. Its expectations are then a subset of its `to_world` with no more
code; it overrides `check` only for expectations of another shape.

```rust,ignore
impl SimState for Mounts {
    const BLOCKS: &'static [&'static str] = &["mounts"];
    fn load(&mut self, block: &Node) -> world_file::Result<()> { .. }
    fn to_world(&self) -> Vec<Node> {
        let mut block = Node::new("mounts");
        for m in &self.0 {
            block = block.with_child(Node::new("bind").with_arg(&m.source).with_arg(&m.target));
        }
        vec![block]
    }
}
```

A world holds the states of the effects it lists, and sends each block to
the state that reads it:

- A block no state of the world reads is an error, with a suggestion
  ("unknown `mount` in the world (did you mean `mounts`?)").
- Two states reading the same block is an error.
- A built-in block whose feature is off says so ("`net` needs
  seam-test's `net` feature").

Nodes are read through `Node` and `Value`, whose checks (`block`,
`arity`, `props_only`, `prop`, `no_children`, `contents`, `string`,
`uint`, `bool`, `unknown`) report errors at the offending line and column,
and built with `Node::new(..).with_arg(..).with_prop(..).with_child(..)`
and `with_bytes`. `node.failure(message)` makes a failed expectation at a
node. See [effects of your own](crate::_docs::guide::your_own_effects)
for a complete example.

## Why KDL

KDL is a tree, which fits a world: blocks per area, and directories that
nest as they do on disk. Its ordering rules fit too: nodes and arguments
are ordered and properties are not, so anything whose order matters
(commands run, arguments) is written as nodes or arguments, and properties
only carry named options (`exit=`, `hex=`). The parser is seam-test's own,
for the subset above, so seam-test has no dependencies and its errors speak
in the world's terms.
