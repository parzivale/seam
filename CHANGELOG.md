# Changelog

## 0.7.0

The GPU over Vulkan, the wrapper crates working under every executor, and
each wrapper exposing its library's whole API bar the effectful parts.

### Breaking

- `seam-jiff`: the helpers and the `TimeZoneFor` and `SystemTimeZone`
  queries give the facade's types (`seam_jiff::Timestamp`, `Zoned`,
  `tz::TimeZone`, ...) in place of jiff's. Each converts with `From`
  (or `into_jiff`) where jiff's own is needed.
- `seam-tracing`: `Log` gains `fields` (structured fields), and its
  `target` is `&'static str`.
- `seam-notify`: `Event` and `EventKind` are notify's own (from
  notify-types), with its finer kinds; `Watch` gains `kinds`, a filter;
  `Notify` holds a `notify::Config` (`Notify::new`, `with_config`).
- `seam-reqwest`: `Reqwest` is no longer a tuple struct: build it with
  `Reqwest::new`, `with_client`, `on` or `builder`.

### Wrappers under every executor

The wrappers work under every executor, not only Tokio's:

- `Reqwest` runs under any executor: on the current Tokio runtime inside
  one, and otherwise on a one-thread runtime of its own, started on first
  use (`Reqwest::on` picks one; `Reqwest::with_client` takes a configured
  client).
- `Sqlx` runs on Tokio (feature `tokio`, default) or smol (feature `smol`;
  the `seam` feature `smol` turns it on).
- `Notify`'s async waits (`NextEvent`, `Timeout<NextEvent>`) do not block
  the executor.

The `seam` features `reqwest`, `sqlx` and `notify` bring in the real
handlers (`Reqwest` with rustls, `Sqlx` with SQLite on Tokio, `Notify`),
and `test` turns on the wrappers' world-file blocks.

### The wrappers expose their libraries

Each wrapper exposes its library's API, bar the effectful parts, so a
core needs no direct dependency on the library:

- `seam-jiff` mirrors jiff's whole API (`civil`, `tz`, `fmt`, spans,
  rounding, ...) as newtypes and enums, generated from jiff's rustdoc
  (`scripts/facades/`), with `From` both ways. The functions that read
  the clock or the tz database take a capability instead:
  `Timestamp::now(c)`, `Zoned::now(c)`, `TimeZone::get(c, name)`,
  `TimeZone::system(c)`, `in_tz(c, name)`, `Zoned::parse(c, s)`,
  `BrokenDownTime::to_zoned(c)`, and `DateTimeParser::parse_zoned(c, ..)`.
- `seam-rand` re-exports rand's traits, `distr`, `seq`, `rngs` and
  `prelude` (features `std_rng`, `chacha`); `seam-uuid` re-exports
  `Uuid`, `Builder`, `fmt`, `timestamp` and `uuid!` (v1, v3, v5, v6, v7,
  v8); `seam-futures` re-exports futures-rs' combinators, streams, sinks,
  `io` and `task` items and macros.
- `seam-reqwest`: `Reqwest::builder()`, every `reqwest::ClientBuilder`
  setting, and reqwest's `Proxy`, `redirect`, `retry`, `tls`, `dns` and
  `Url`.
- `seam-dirs`: user directories (`UserDir`: audio, desktop, documents,
  downloads, pictures, public, templates, videos, fonts), read
  from `user-dirs.dirs` on Linux.
- `seam-notify`: notify's own `Event` and `EventKind`, a `Watch` filter
  (`kinds`), and `Notify::with_config`; `Polling` reports the same
  kinds, per path.
- `seam-tracing`: structured fields (`info!(c, user = %name, n = 3,
  "...")`), and spans as a scope (`span!`, `info_span!`, ...), sent to
  tracing with explicit parents, discarded, or captured for tests.
- `seam-sqlx`: typed reads through std's `TryFrom`: `Row::try_get` and
  `try_get_at` into integers, floats, `bool`, `String`, `Vec<u8>` and
  `Option`s of them (and `Uuid` and `serde_json::Value` with features
  `uuid` and `json`); tuples and a core's own `TryFrom<&Row>` types
  through `fetch_all_as`, `fetch_one_as` and `fetch_optional_as`.

### The GPU over Vulkan

A new crate, `seam-ash` (`seam-rs-ash`, `seam::ash` with the `ash`
feature), makes the GPU effects:

- Scopes for resources: `CreateBuffer`, `CreateImage` and `CreateProgram`
  (compute, or vertex and fragment drawing triangles), lending plain
  `Buffer`, `Image` and `Program` IDs.
- Operations: `Upload`, `UploadImage`, `CopyBuffer`, `Dispatch`, `Draw`
  (with clears, vertex buffers, instances, bindings and push constants),
  `Download`, `DownloadImage`, `Finish`, and the `DeviceInfo` query. Only
  downloads and `Finish` wait.
- `Submit`: a scope that sends the work so far without waiting, lending a
  submission to inspect `IsDone` on; closing the scope is the wait.
- `Import<B>`: an image from outside the handler, lent as an `Image`
  without a CPU copy. The source is per platform: `drm::Dmabuf` (Linux
  and the BSDs: a Wayland client's or GBM's buffer, by fd, fourcc and
  modifier), `win32::SharedHandle` (Windows: a Direct3D 11 or 12 texture,
  or Vulkan memory, by NT handle) and `metal::IoSurface` (Apple systems,
  through MoltenVK). Opening waits for a dmabuf's writers (its implicit
  fences, by `poll`); closing hands the memory back to its owner and
  waits until the GPU is done with it, so a Wayland client's buffer can be
  released once the scope has closed. `drm::DmabufFormats` lists the
  format and modifier pairs a GPU imports, for linux-dmabuf feedback.
  `Ash` imports a dmabuf as the image itself where the device has
  `VK_EXT_image_drm_format_modifier`, and otherwise copies a linear one
  into an image on the GPU; `SimGpu` reads fake dmabufs (memfds).
  `Ash` also now finds portability implementations such as MoltenVK.
- The handler tracks what each operation reads and writes and inserts the
  barriers and image layout transitions; a core never mentions
  synchronization. The tests run with the Khronos validation layer's
  synchronization validation, which reports no hazards.
- `Ash`: Vulkan 1.3 (dynamic rendering, synchronization2) through ash. One
  implementation gives both forms: blocking (`Perform`) waits on the
  calling thread, async (`AsyncPerform`, `SendPerform`) awaits a waiter
  thread under any executor. A scope closed while the GPU still uses its
  resource defers the destruction; a download cancelled mid-flight frees
  its staging memory once the GPU is done with it. Programs compile
  outside the device lock (on their own thread in the async form).
  `AshOptions` sets how long waits spin before sleeping (`spin`, 20 µs by
  default; spinning removes most of the async form's wake-up latency) and
  the waiter thread's stack size.
- `SimGpu`: buffers and images as bytes in memory, the same checks as
  `Ash`, clears applied, and programs run by CPU functions given per
  label, for testing a core's data flow without a GPU.

### Documentation

- Every wrapper crate's docs list its handlers (sync or async, under which
  executors), what else a root needs for it, a program using it beside
  real handlers (compiled as a doctest) and its features. The README has
  a Handlers section: what each handler covers, and what each wrapper
  needs beside it in a root.
- "Backend" is gone from the docs: seam has effects and handlers, and
  `Std`, `Tokio`, `Smol` and `Sim` are handlers like any other.

## 0.6.0

Split handles, a helper for most everyday tasks, and backends and
wrappers for the crates programs use most: smol, futures-rs, jiff, rand,
uuid, dirs, tracing, notify, reqwest, sqlx, rustix and nix. Rust 1.99 is
now required.

### Breaking

- `World` (seam-test's world-file trait) gains a required method,
  `to_world`, which writes a part's state out as world-file nodes; an
  implementation must add it. In return `check` now has a default, a
  subset of `to_world` (`world_file::check_subset`), so most
  implementations can drop theirs.
- `Sim::script_command` returns `&mut Sim`, for chaining.
- The minimum Rust version is 1.99 (it was 1.85).

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

### Split handles

Two new acquisitions lend a handle's directions separately, so they can
be used at the same time (joined futures in one task, or scoped threads),
each still behind its own `&mut`:

- `io::Split<R, W>`, from a TCP stream: `Halves { read, write }`. The read
  half reads (and times out); the write half writes, flushes, shuts down
  (tell the peer the request is done while still reading the reply) and
  times out. Both know their addresses.
- `process::SplitChild<I, O, E>`, from a child: `ChildPipes { stdin,
  stdout, stderr }`, to feed a child while draining both outputs. The body
  still has the child, to `Wait` or `Kill`.

Every backend implements both: std (`TcpReadHalf`, `TcpWriteHalf`,
`ChildStdin`, `ChildStdout`, `ChildStderr`), Tokio, `Sim`, the simulated
runtime and simulated threads. Closing the scope gives the pieces back. A
failed scope drops them: std and the simulation lose nothing, Tokio closes
the stream (it fails with `NotConnected` after). The simulated child still
runs when its output is first read, on the input written by then.

### futures-rs backend

A new crate, `seam-futures` (`seam-rs-futures`, `seam::futures` with the
`futures` feature), wraps futures-rs as `seam-tokio` wraps Tokio:

- `Futures`: channels (`futures::channel::mpsc`) and shared cells
  (`futures::lock::Mutex`). They need no runtime, so they also run on the
  simulated runtime. `try_send` and `try_recv` work; futures-rs has no
  clock, so a `Timeout` that would wait panics, pointing at `within`.
- `Executor<S>`: `Spawn` and `SpawnSet` on any `futures::task::Spawn`
  (`ThreadPool`, a `LocalPool`'s spawner, ...), with `Join`, `Abort`,
  `Detach`, `IsFinished`, `JoinNext` and `YieldNow`.
- `io::Compat` makes a futures-io type a seam handle; `io::AsyncReader`
  and `io::AsyncWriter` make a seam handle a futures-io one.
- `stream::messages`, `stream::lines` and `stream::entries`, as `Stream`s.
- Helpers that run several operations at once, so are async only (each
  with a `_send` form): `time::within` (race any future against a clock's
  `Sleep`), `net::copy_bidirectional_async` (a proxy, over split streams)
  and `process::pipe_through_async` (`RunCommand` with input, over a split
  child).

### Time zones over jiff

A new crate, `seam-jiff` (`seam-rs-jiff`, `seam::jiff` with the `jiff`
feature), makes time zones effects:

- Two queries, `TimeZoneFor` (a zone by IANA name) and `SystemTimeZone`
  (`TZ`, then `/etc/timezone`, then `/etc/localtime`, else UTC), with keys
  in `seam_jiff::effects` for roots.
- `TzDb<C, F>` answers both by reading TZif files through the filesystem
  (and environment) of the capability it is built over: `Std` in
  production, a `Sim` in tests, whose files then are the tz database.
- `Bundled` (`bundled` / `jiff-bundled`) answers `TimeZoneFor` from the
  database compiled in, with no effect.
- `timestamp`, `now_zoned`, `now_in` and `now_local` read the wall clock
  through seam, in sync, `_async` and `_send` forms.
- `templates/clippy.toml` bans jiff's own `Zoned::now`, `Timestamp::now`,
  `TimeZone::system`, `TimeZone::get` and `tz::db`, and futures-rs's
  `ThreadPool::new`; `println!` and `eprintln!` now point at the new
  `println` and `eprintln` helpers.

### smol backend

A new crate, `seam-smol` (`seam-rs-smol`, `seam::smol` with the `smol`
feature): a `Smol` capability for time, files, TCP and UDP, processes,
stdio, Ctrl-C (Unix), tasks and task sets on smol's global executor,
channels, shared cells and every timeout, as `seam-tokio` does for Tokio.
`Split` lends two clones of a stream, so a failed split loses nothing.

### Wrappers of popular crates

Each is its own crate (`seam-rs-<name>`, and a `seam` feature of that
name re-exporting it as `seam::<name>`): the library's effects as seam
operations, a real handler, and one for tests. `templates/clippy.toml`
bans each library's own direct calls.

| Crate | Effects | Real | For tests |
|---|---|---|---|
| `seam-rand` | rand's API over `FillRandom` (`rng`, `seeded`) | any `FillRandom` | a seeded `Sim` |
| `seam-uuid` | `new_v4`, `new_v7` over `FillRandom` and `WallClock` | `Std` | `Sim` |
| `seam-dirs` | platform directories (`dir`, `ProjectDirs`) over `Var` | `Std` | `Sim`'s `set_var` |
| `seam-tracing` | `Log`, with `error!` .. `trace!` | `Tracing` | `Captured` (world-file `logs` block) |
| `seam-notify` | the `Watch` scope, `NextEvent` | `Notify` (the OS) | `Polling` over a `Sim` |
| `seam-reqwest` | `Fetch` (an `http::Request` to an `http::Response`), `get`, `get_text`, `post` | `Reqwest` | `Scripted` (world-file `http` block) |
| `seam-sqlx` | `Execute`, `Fetch`, `Begin` (a transaction scope), `Acquire` | `Sqlx` (an `AnyPool`) | `Sqlx::memory` (in-memory SQLite) |
| `seam-unix` | `Mount`, `Unmount`, `Chmod`, `Chown`, `Symlink`, `ReadLink`, `Ids`, `ProcessId`, `Hostname`, `Kill`, `SetUmask` | `seam-rustix`'s `Rustix`, `seam-nix`'s `Nix` | `SimUnix` (world-file `unix` block) |

Each is on the latest release of its library: uuid 1.29, sqlx 0.9,
reqwest 0.13 (rustls by default), nix 0.31, rustix 1.1.

### Simulation

- `Sim::to_world` and `seam::test::to_world::<(K, ..), _>(&root)`
  write a whole world out, and it loads back to the same world.
- `Node` builders (`Node::new`, `with_arg`, `with_prop`, `with_child`,
  `with_bytes`), `Node::contents`, KDL output (`Display`), and
  `world_file::show`. `World` is also exported from `world_file`.
- World files: `net`, `seed`, `parallelism` and `signal` can be checked
  (as a subset); `process` loads `ran` lines into the log; `stdio` loads
  `stdout` and `stderr`; `clock` takes `now-ns=` and `wall-ns=`. Files a
  world file creates are stamped at the Unix epoch.

### Rust 1.99

With 1.99 as the minimum, the code uses let-chains and `is_multiple_of`,
among others, and every dependency is on its newest release.

### Documentation

- A `docs` feature adds `seam::_docs`: what seam is, usage guides, the
  world-file format and implementation notes. Off by default, so it costs
  nothing to build; docs.rs builds it.

## 0.5.0

Every root part is built the way Rust builds its type, and a simulated
world is shared by cloning it. All of these are breaking.

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

### Simulation

- `Sim` is `Clone` (with std), and every clone is a handle to one world:
  the clock, random numbers, environment, files, network, stdio, scripted
  commands and Ctrl-C. A part built over the simulation is given a clone.
  Without std, `Sim` holds its world itself and is not `Clone`.
- `Sim::invocations` returns a `Vec<Invocation>`, since the log lives
  behind the world's lock.
- `Sim`'s world-file blocks are an ordinary `World` impl over public API
  only, and a test compiles it outside the crate to keep it so.
- New for every part: `world_file::load` and `world_file::check` set up
  and check one part outside a root (`Sim::load_world` and
  `Sim::check_world` are these). `Sim::script_command` is public.

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
