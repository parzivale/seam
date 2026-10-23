# Changelog

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
