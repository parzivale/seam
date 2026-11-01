# Costs

What seam does that the std call it stands for would not. None of it is
hidden on purpose, but most of it is easy to miss from a signature. Sizes
are for 64-bit targets.

## Files

| What | Costs | Why |
|---|---|---|
| A scope that opened a file for writing (`Open` in `WriteOnly`, `Append` or `ReadWrite`), including `write_file` and `append_file` | a flush and an fsync when it succeeds | closing commits, and a commit that a power cut can undo is not one; `std::fs::write` does not sync |
| `CreateAtomic`, and `write_file_atomic` | create a temporary file, fsync it, rename it, then open and fsync the directory | the rename is what makes the write atomic, and the directory sync is what makes the rename last |
| `View`, and `read_file` and `read_to_string` on it | a lookup, the open, and a lookup of the open file: three calls where `File::open` is one | a FIFO or a device would block or lose data, so the path is checked first, and the file again in case the path was replaced in between |
| `ReadAt` on Tokio and smol | a buffer the size of the caller's, allocated per read, and copied back | the read runs on the blocking pool, which cannot borrow the caller's buffer |
| `Open` and `View` on Tokio and smol | the path is copied into a `PathBuf`, and each operation hops to the blocking pool | the same as `tokio::fs`, which seam uses |

A core writing many small records keeps one scope open and writes them all
in it, rather than calling `append_file` per record: each scope waits for
the disk once.

## Errors

`seam::io::Error` is 32 bytes; `std::io::Error` is 8. So `io::Result<()>` is
32 bytes where std's is 8, and `io::Result<usize>`, what every `Read` and
`Write` returns, is 32 where std's is 16. It holds a kind, a static message,
and with std the `std::io::Error` it came from: the message is what makes an
error readable without std. Without std it is 24 bytes.

## Roots and async

Forwarding through a [`Root`](crate::Root) is free for the sync forms and
the `Send` forms, which hand back the part's own result or future. The
plain async forms (`AsyncInspect`, `AsyncPerform`) wrap the part's future
in one of their own, so that a part need not be `'static`: a core awaiting
through them has a future 16 bytes larger than one given the part itself.

The lifts from sync to async cost nothing at run time, but a raised
operation does its work in its first poll, so a raised `Std` operation that
waits (reading a file, a socket, sleeping) blocks the runtime's thread.
Give an async core on a runtime a native async handler for those; see
[the async guide](crate::_docs::guide::async_and_concurrency).

## The network on `Std`

| What | Costs | Why |
|---|---|---|
| A timed operation on a socket (`Timeout`) | two extra calls, setting the socket's timeout and restoring it | std sockets time out per socket, not per call |
| A timed `Accept` | polls the listener, sleeping up to 10ms between tries, so it can notice a connection up to 10ms late | std has no timed accept |

## Checking the numbers

`seam-core/tests/sizes.rs` asserts the sizes above, so a change that grows
them fails the tests. To measure a core of your own, take
`core::mem::size_of_val(&core(&mut c))` of its future, through a root and
without.
