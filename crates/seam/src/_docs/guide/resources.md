# Resources

A file, a socket, a child process or a task is something a core *uses*, not
something it owns. A core asks for the capability to provide one, and gets
it for the length of a closure: a scope.

## Scopes

[`acquire`](crate::Provide::acquire) opens a resource, lends it to a
closure along with the capability, and closes it when the closure returns:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::io;
use seam::prelude::*;
use seam::ops::fs::CreateAtomic;
use seam::ops::io::{Write, write_all};

/// Replaces `settings.conf` with `text`, all or nothing.
fn save<C, F>(c: &mut C, text: &str) -> io::Result<()>
where
    C: for<'a> Provide<CreateAtomic<'a, F>>,
    F: for<'a> Perform<Write<'a>>,
{
    c.acquire(CreateAtomic::new("/settings.conf"), |_c, file| {
        write_all(file, text.as_bytes())?;
        if text.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty settings"));
        }
        Ok(())
    })
}

let mut sim = seam::test::Sim::new();
save(&mut sim, "port = 80").unwrap();
assert!(save(&mut sim, "").is_err());
// The failed save left the old file in place.
assert_eq!(sim.file("/settings.conf").unwrap(), b"port = 80");
# }
```

What happens at the end of the scope depends on how the body ends:

| The body | The resource is |
|---|---|
| returns `Ok` | **committed**: a file synced, an atomic file renamed into place, a stream shut down cleanly, a task or child waited for. An error from that is returned. |
| returns `Err` | **rolled back**: the temporary file deleted, a task aborted, a child killed |
| panics | rolled back, and the panic continues |
| is an async scope whose future is dropped | rolled back, at whichever `.await` it was dropped |

The body borrows the resource, so it cannot keep, return or leak it, and
it gets the capability back as its first argument, so it can perform other
effects and open nested scopes.

When the body's only error path is a `?`, Rust may not be able to infer its
error type; end it with `io::Result::Ok(..)` (or your own error type).

## Lending: scopes that are queries

A scope that changes nothing outside itself is a query, and is *lent* from
`&C` with [`lend`](crate::Lend::lend). Reading a regular file through a
`View` is one: it reads at offsets (`ReadAt`) and moves no cursor.
`read_file` and `read_to_string` are built on it, so they take `&C`:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::io;
use seam::prelude::*;
use seam::ops::fs::{ReadAt, View, read_to_string};

fn word_count<C, F>(c: &C, path: &str) -> io::Result<usize>
where
    C: for<'a> Lend<View<'a, F>>,
    F: for<'a> Inspect<ReadAt<'a>>,
{
    Ok(read_to_string(c, path)?.split_whitespace().count())
}

let mut sim = seam::test::Sim::new();
sim.add_file("/a.txt", "one two three");
assert_eq!(word_count(&sim, "/a.txt").unwrap(), 3);
# }
```

Every lending capability is also a providing one, so a bound on `Provide`
accepts a handler that only lends.

## Files

| Acquisition | Gives | Commits by |
|---|---|---|
| `View<'a, F>` | a regular file to read at offsets (`ReadAt`) | nothing: it is lent |
| `Open<'a, M, F>` | a file in mode `M` | syncing it, if writable |
| `CreateAtomic<'a, F>` | a temporary file beside the target | syncing it and renaming it over the target |
| `ReadDir<'a, D>` | a directory listing, stepped with `NextEntry` | nothing: it is lent |

The mode of an `Open` is a type, `ReadOnly`, `WriteOnly`, `Append` or
`ReadWrite`, and a handler's file implements `Read` only for readable
modes, and `Write`, `SetLen` and the syncs only for writable ones. A core
bounded on `Open<'a, ReadOnly, F>` cannot write, and the compiler checks it:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::io;
use seam::prelude::*;
use seam::ops::fs::{Open, WriteOnly};
use seam::ops::io::{Write, write_all};

fn log<C, F>(c: &mut C, line: &str) -> io::Result<()>
where
    C: for<'a> Provide<Open<'a, WriteOnly, F>>,
    F: for<'a> Perform<Write<'a>>,
{
    c.acquire(Open::create("/app.log"), |_c, file| write_all(file, line.as_bytes()))
}

let mut sim = seam::test::Sim::new();
log(&mut sim, "started\n").unwrap();
assert_eq!(sim.file("/app.log").unwrap(), b"started\n");
# }
```

A directory is listed one path at a time, so a core holds one entry and can
stop early:

```rust
# #[cfg(all(feature = "test", feature = "fs"))] {
use seam::io;
use seam::prelude::*;
use seam::ops::fs::{NextEntry, ReadDir};

fn count_logs<C, D>(c: &mut C, dir: &str) -> io::Result<usize>
where
    C: for<'a> Provide<ReadDir<'a, D>>,
    D: Perform<NextEntry>,
{
    c.acquire(ReadDir::new(dir), |_c, entries| {
        let mut n = 0;
        while let Some(path) = entries.perform(NextEntry)? {
            n += usize::from(path.extension().is_some_and(|e| e == "log"));
        }
        Ok(n)
    })
}

let mut sim = seam::test::Sim::new();
sim.add_file("/logs/a.log", "").add_file("/logs/b.txt", "");
assert_eq!(count_logs(&mut sim, "/logs").unwrap(), 1);
# }
```

A process killed outright runs no cleanup, so an atomic write can leave its
temporary file (`.<name>.seam-<suffix>.tmp`) behind; `sweep_atomic` removes
the ones old enough to be abandoned.

## Standard I/O, sockets and children

The same pattern covers the rest:

- **stdio.** `OpenStdin<H>`, `OpenStdout<H>` and `OpenStderr<H>` lend
  handles that `Read`, or `Write` and `Flush`. `print` and `eprint` open,
  write and close; read lines with a `BufReader` over the stdin handle.
- **sockets.** `TcpConnect<S>`, `TcpBind<L>`, `Accept<S>` (on a listener)
  and `UdpBind<U>`. Closing a stream shuts it down cleanly.
- **children.** `SpawnChild` lends a running process, written to and read
  like a stream, with `ReadErr`, `CloseStdin`, `Wait` and `Kill`. A scope
  that succeeds waits for it; one that fails kills it.

## Tasks and threads

Spawning is an acquisition too, with the same rules: a scope that succeeds
waits for its task, and one that fails (or is cancelled) aborts it, or on
OS threads joins it, since a thread cannot be stopped from outside.
`Detach` opts a task out.

- `Spawn<F, H>` runs one task.
- `SpawnSet<S>` opens a set to spawn a batch into and collect from, since a
  core cannot collect handles outside their scopes.
- `SpawnWith<A, B, H>` has the provider acquire a resource for the new task
  (a listener accepting a connection into the task that serves it), since a
  core cannot move a resource itself.

See [the async guide](crate::_docs::guide::async_and_concurrency) for examples.

## Why scopes are operations

Opening and closing are themselves operations, [`Opening<A>`](crate::Opening)
and [`Closing<A>`](crate::Closing), and [`Provide<A>`](crate::Provide) is a
name for "performs both". So a scope works wherever operations do: through
the async and `Send` forms, through a root (routed by the acquisition's
key), and through a wrapper that forwards every operation.

`Opening` carries a `Token` that only seam can create, so a core cannot
open a resource outside a scope, and only a scope ever holds a resource by
value, so only a scope can close one.

## Scopes in async code

`acquire_async` and `lend_async` take an async closure. For a core that
must run on a multithreaded runtime, bound it with `SendProvide` /
`SendLend` and call `acquire_send` / `lend_send`, from
[`SendScope`](crate::SendScope): its future is then `Send` whenever what
the body holds is.
