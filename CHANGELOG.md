# Changelog

## 0.5.0

Every root part is built the way Rust builds its type, a simulated world
is shared by cloning it, and world files can write a world out as well as
read it. All of these are breaking.

### Building roots

A part is written as Rust builds a value of its type, with the effects it
serves in brackets after the type's name, and in no other way:

| Form | Example |
|---|---|
| unit struct | `Std<fx::Args, fx::View>` |
| tuple struct | `Pool<fx::Query>(4)` |
| struct literal | `Pool<fx::Query> { size: 4 }` |
| constructor, with or without `?` | `Sim<fx::Now>::from_world(W)?` |

| Removed | Use |
|---|---|
| `Sim<fx::Now> = sim` | a constructor: `Sim<fx::Now>::clone(&sim)` |
| `name: value => [keys]` | one of the four forms |
| a bare part keyed by `Effect` (`mounts::Bind`) | its keys in brackets: `mounts::Bind<BindMount>` |

The named form is `root! { struct App { clock: Clock<fx::Now> } }`.
Anything else is a compile error naming the four forms.

### Helpers

New helpers, each in sync, `_async` and `_send` forms, built only from
existing primitives, so every backend (and `Sim`) runs them unchanged:

| Module | Helpers |
|---|---|
| `fs` | `exists`, `is_file`, `is_dir`, `append_file`, `write_file_atomic`, `read_lines`, `remove_dir_contents`, `copy_dir_all` |
| `time` | `sleep_until`, `elapsed`, and `tick` over an `Interval` (missed deadlines are skipped) |
| `random` | `random_range` (unbiased), `random_bool`, `shuffle`, `choose` |
| `env` | `var_string`, `var_or` |
| `stdio` | `println`, `eprintln`, `read_line` (byte at a time, so it never reads past the line) |
| `process` | `run_checked` (fails unless the exit code is 0), `output_string` |
| `net` | `connect_any`: resolve, connect to the first address that answers, run the body once |

### Documentation

- A `docs` feature adds `seam::_docs`: what seam is, usage guides, the
  world-file format and implementation notes. Off by default, so it costs
  nothing to build; docs.rs builds it.

### Simulation

- `Sim` is `Clone` (with std), and every clone is a handle to one world:
  the clock, random numbers, environment, files, network, stdio, scripted
  commands and Ctrl-C. A part built over the simulation is given a clone.
  Without std, `Sim` holds its world itself and is not `Clone`.
- `Sim::invocations` returns a `Vec<Invocation>`, since the log lives
  behind the world's lock.
- `World` (a handler's world-file blocks) gains `to_world`, which writes
  the state out as world-file nodes, and `check` now defaults to a subset
  of it (`world_file::check_subset`), so a fake gets expectations with no
  more code. `Sim::to_world` and `seam::test::to_world::<(K, ..), _>(&root)`
  write a whole world out, and it loads back to the same world.
- `Node` builders (`Node::new`, `with_arg`, `with_prop`, `with_child`,
  `with_bytes`), `Node::contents`, KDL output (`Display`), and
  `world_file::show`. `World` is also exported from `world_file`.
- World files: `net`, `seed`, `parallelism` and `signal` can be checked
  (as a subset); `process` loads `ran` lines into the log; `stdio` loads
  `stdout` and `stderr`; `clock` takes `now-ns=` and `wall-ns=`. Files a
  world file creates are stamped at the Unix epoch.
- `world_file::load` and `world_file::check` set up and check one part
  outside a root (`Sim::load_world` and `Sim::check_world` are these).
  `Sim::script_command` is public, and returns `&mut Sim`.

## 0.4.0

Roots route each effect to the part that serves it, and every simulated
part reads its own share of a world file. All of these are breaking.

### Roots route per effect

| Removed | Use |
|---|---|
| `kinds::Fs`, `kinds::Time`, ... | one key per effect, in `effects`: `effects::View`, `effects::Open<ReadOnly>`, `effects::Now`, ... |
| `InKind { type Kind }` | `Effect { type Key }`; a program's own effect is usually its own key |

A root lists the effects each part serves and grants a core exactly those.
`Open` is keyed by its mode, so reading and writing are separate grants.
`root!` no longer lets a part's name shadow a path in its keys.

A root is a list of parts, each with the effects it serves:
`root![Std<fx::Args, fx::View>, Sim<fx::Now> = sim, Pool<fx::Query>(4),
mounts::Bind]`. Each part is built with its own construction syntax; a
handler of the root's own crate can be listed bare, keyed by `Effect`. The
`name: value => [keys]` form remains, for parts whose type is inferred.
`root.part::<K>()` returns the part serving `K`. `Std` and `Tokio` are
plain unit structs (no longer `non_exhaustive`), and `Copy`.

### World files for every simulated part

- `World`: a simulated part's share of a world file, the blocks it reads
  and `load` and `check` for them. `Sim` implements it.
- `load_world::<(K, ..), _>(&mut root, text)` and `check_world` set up and
  check a whole root through the effects listed, up to 32.
- `world_file::Node` and `Value` are public, with the checks `Sim`'s
  loader uses, so other crates' parts can read their blocks.

## 0.3.0

Every effect is now a primitive or built from primitives. A backend
implements only the primitives; everything else is a plain function in
seam-core, in three forms (sync, `_async`, `_send`), that works on every
backend. All of these are breaking.

### Removed operations, and what replaces them

| Removed | Use |
|---|---|
| `ReadExact`, `WriteAll`, `ReadToEnd` | `read_exact`, `write_all`, `read_to_end` (and `copy`) on any `Read`/`Write` handle |
| `ReadFile`, `ReadToString` | `read_file`, `read_to_string`, through `View` and `ReadAt`, from `&C` |
| `WriteFile` | `write_file`, through `Open` |
| `CreateDir { recursive: true }` | `create_dir_all`; `CreateDir` creates one directory |
| `SweepAtomic` | `sweep_atomic` (also needs `WallClock`); `SweepAtomic::is_temp_name` is `is_temp_name` |
| `Resolve` | `resolve` (alloc), built on `ResolveInto` |
| `ReadErrToEnd` | `read_err_to_end`, built on `ReadErr` |
| `RandomU64` | `random_u64`, built on `FillRandom` |
| `TrySendMsg`, `TryRecvMsg` | `try_send`, `try_recv`: a `Timeout` of zero |
| `Store` | `store`, an `Update` |
| `Print`, `EPrint` | `print`, `eprint`, or `write_all` on a stdout or stderr handle |
| `ReadLine`, `ReadLineInto` | `BufReader::read_line` (or `read_until`) on any `Read` handle, such as stdin's |

### Scopes are operations

| Removed | Use |
|---|---|
| `impl Provide<A>`: `provide`, `close`, `abandon` | `impl Perform<Opening<A>>` and `impl Perform<Closing<A>>`; dropping the resource rolls back |
| `impl AsyncProvide<A>` | `AsyncPerform` and `SendPerform` of `Opening<A>` and `Closing<A>` |
| `Acquire`, `AsyncAcquire` | `acquire` and `acquire_async` are methods of `Provide` and `AsyncProvide` |
| `AsyncProvide` bound in a `Send` core | `SendProvide`, scoped with `acquire_send` (on `SendScope`) |

`Provide<A>` is now the name for `Perform<Opening<A>> + Perform<Closing<A>>`,
so scopes work wherever operations do: the async forms and lifts, a `Root`
(which no longer forwards scopes separately), and wrappers.

- `Lend<A>`, `AsyncLend<A>`, `SendLend<A>`: a scope that changes nothing
  outside itself, a query, from `&C`. Every lending capability also
  provides.
- `View<'a, F>`, lent: a regular file opened to read at offsets, with
  `ReadAt`, a query. Backends' handle is `FileView`. A FIFO or a device is
  refused (`InvalidInput`): open those with `Open`.
- `ReadDir` is lent.

### New

- Standard input, output and error are handles: `OpenStdin<H>`,
  `OpenStdout<H>` and `OpenStderr<H>` lend handles that `Read`, or `Write`
  and `Flush`. Raw, unbuffered bytes from stdin, not only lines.
- `BufReader<N>`, in `ops::io`: reads lines, or records ending in any byte,
  from any handle through a fixed buffer, with no allocation or std. `Line`
  moved here from `ops::stdio`.
- `Open<'a, M, F>` takes a mode type, `ReadOnly`, `WriteOnly`, `Append` or
  `ReadWrite`, and backends' files are `File<M>`: `Read` only for readable
  modes, `Write`, `SetLen` and the syncs only for writable ones, checked at
  compile time. `OpenOptions` keeps only `create`, `create_new` and
  `truncate`.
- `copy(src, dst, buf)` in `ops::io`.

### Changed

- `Timeout` covers primitives only. Backends no longer time `ReadExact`,
  `ReadToEnd`, `WriteAll` or `ReadErrToEnd`.
- `sync` turns on `time`, for `try_send` and `try_recv`.
- `io-std` turns on `io`.
- In the simulation, stdio lives in the shared world, and `Sim::stdout` and
  `Sim::stderr` return a copy (`Vec<u8>`) instead of a slice.
- `write_file` syncs the file when it closes it, as every writable file is
  synced on close; `std::fs::write` did not.
- Tokio's and the simulated runtime's receivers have no sync non-blocking
  receive: `try_recv` is a zero `Timeout`, which they implement
  asynchronously. Use `try_recv_send` from async code.

## 0.2.0

Breaking changes are marked **(breaking)**.

### I/O errors

- **(breaking)** I/O operations fail with `seam::io::Error` instead of
  `std::io::Error`. It is an `ErrorKind` (std's kinds, `non_exhaustive`)
  and a `&'static str`, allocates nothing, and is the same type with or
  without std. With std it keeps the `std::io::Error` it came from and
  converts both ways (`?`, `.into()`, `as_std()`, `into_std()`).
- `io`, `net`, `stdio` (`io-std`) and `signal` no longer need std. A new
  `io` feature turns on the handle operations alone.
- **(breaking)** `Shutdown` takes seam's own `How` instead of
  `std::net::Shutdown`, which converts into it.

### Fewer allocations

- `ReadExact`, `ReadLineInto` (returns a `Line { len, complete }`) and
  `ResolveInto` fill a buffer the caller passes. They need no `alloc`;
  `ReadToEnd`, `ReadLine` and `Resolve` now need `alloc`.
- **(breaking)** Operations borrow their inputs: paths are `&Path`,
  `WriteFile`'s contents `&[u8]`, `Var`'s name `&OsStr`, `Resolve`'s host
  `&str`. Constructors take `&impl AsRef<_>`, so string literals work as
  before; an owned `PathBuf` or `String` needs a `&`. Bounds name every
  lifetime: `for<'a> Inspect<ReadFile<'a>>`.
- **(breaking)** `ReadDir` is an acquisition, `ReadDir<'a, D>`, lending a
  listing that `NextEntry` steps through one path at a time, instead of a
  query returning a `Vec`. Each backend has an `fs::Dir` handle.

### New operations

- Child processes: `SpawnChild` lends a running child to a scope, written
  to and read like a stream, with `ReadErr`, `ReadErrToEnd`, `CloseStdin`,
  `Wait`, `Kill` and `ChildId`. A successful scope waits for the child; a
  failed one kills it.
- File handles: `Seek(SeekFrom)` (in `ops::io`, with seam's own
  `SeekFrom`), `SetLen`, `SyncAll`, `SyncData` and the query `Stat`, on
  `File` and `AtomicFile` in every backend.
- Environment: `SetVar` and `RemoveVar`, which check names and values
  (`InvalidInput`) instead of panicking. On std they are served by
  `seam_std::env::WritableEnv`, made with an `unsafe fn new()` carrying
  `std::env::set_var`'s contract.

### Simulation

- Simulated child processes, scripted with `on_command` and
  `on_command_with_stdin`; `Invocation::stdin` shows what a child was given.
- World-file expectations check variables (`env { var NAME VALUE; unset
  NAME }`) and what a spawned child was given (`ran cat stdin="hi"`).

### Other

- The workspace lint for `unsafe_code` is `deny`; every crate but
  seam-std forbids it, and seam-std allows it only on `WritableEnv`.
- The clippy template also bans `std::env::set_var` and `remove_var`.
