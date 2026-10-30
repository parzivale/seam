use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::boxed::Box;
use std::vec::Vec;

use futures::io::{AsyncRead, AsyncSeek, AsyncWrite};
use seam_core::future::SendPerform;
use seam_core::ops::io::{Flush, Read, Seek, SeekFrom, Write};

/// A futures-io reader or writer, as a seam handle.
///
/// `Read` needs `AsyncRead`, `Write` and `Flush` need `AsyncWrite`, `Seek`
/// needs `AsyncSeek`, and `Shutdown` of the write half closes the writer.
/// The inner value must be `Unpin` (pin it in a `Box` if it is not).
#[derive(Debug, Default, Clone)]
pub struct Compat<T>(pub T);

async_op!(perform ['a, T: AsyncRead + Unpin + Send] Compat<T>, Read<'a>, |s, op| {
    futures::io::AsyncReadExt::read(&mut s.0, op.0).await
});
async_op!(perform ['a, T: AsyncWrite + Unpin + Send] Compat<T>, Write<'a>, |s, op| {
    futures::io::AsyncWriteExt::write(&mut s.0, op.0).await
});
async_op!(perform [T: AsyncWrite + Unpin + Send] Compat<T>, Flush, |s, _op| {
    futures::io::AsyncWriteExt::flush(&mut s.0).await
});
async_op!(perform [T: AsyncSeek + Unpin + Send] Compat<T>, Seek, |s, op| {
    let to = match op.0 {
        SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
        SeekFrom::End(n) => std::io::SeekFrom::End(n),
        SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
    };
    futures::io::AsyncSeekExt::seek(&mut s.0, to).await
});

// Closing is the only shutdown futures-io has: of the write half.
#[cfg(feature = "net")]
async_op!(perform [T: AsyncWrite + Unpin + Send] Compat<T>, seam_core::ops::net::Shutdown, |s, op| {
    match op.0 {
        seam_core::ops::net::How::Write => futures::io::AsyncWriteExt::close(&mut s.0).await,
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "futures-io can only shut down the write half, by closing it",
        )),
    }
});

/// How much an [`AsyncReader`] or [`AsyncWriter`] moves at once.
const CHUNK: usize = 8 * 1024;

/// A handle operation in flight, owning the handle's borrow and the buffer
/// until it finishes.
type InFlight<'a, H> =
    Pin<Box<dyn Future<Output = (&'a mut H, Vec<u8>, seam_core::io::Result<usize>)> + Send + 'a>>;

enum State<'a, H> {
    Idle(&'a mut H, Vec<u8>),
    Busy(InFlight<'a, H>),
    /// Only while switching between the two.
    Empty,
}

/// A seam handle that performs `Read`, as a futures-io `AsyncRead`.
///
/// Each read is a boxed `SendPerform` future that owns the borrow of the
/// handle while it runs, so a dropped reader cancels it. Reads of up to
/// 8 KiB at a time; whatever a read returns beyond the caller's buffer is
/// kept for the next one.
pub struct AsyncReader<'a, H> {
    state: State<'a, H>,
    /// Read but not yet handed out: `buf[pos..len]`.
    pos: usize,
    len: usize,
}

impl<'a, H> AsyncReader<'a, H>
where
    H: for<'b> SendPerform<Read<'b>> + Send,
{
    /// Reads from `handle` for as long as the reader lives.
    pub fn new(handle: &'a mut H) -> Self {
        Self {
            state: State::Idle(handle, std::vec![0; CHUNK]),
            pos: 0,
            len: 0,
        }
    }
}

impl<H> core::fmt::Debug for AsyncReader<'_, H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AsyncReader").finish_non_exhaustive()
    }
}

impl<'a, H> AsyncRead for AsyncReader<'a, H>
where
    H: for<'b> SendPerform<Read<'b>> + Send,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        loop {
            match core::mem::replace(&mut this.state, State::Empty) {
                State::Idle(handle, buf) if this.pos < this.len => {
                    let n = out.len().min(this.len - this.pos);
                    out[..n].copy_from_slice(&buf[this.pos..this.pos + n]);
                    this.pos += n;
                    this.state = State::Idle(handle, buf);
                    return Poll::Ready(Ok(n));
                }
                State::Idle(handle, mut buf) => {
                    if out.is_empty() {
                        this.state = State::Idle(handle, buf);
                        return Poll::Ready(Ok(0));
                    }
                    this.state = State::Busy(Box::pin(async move {
                        let result = handle.perform_send(Read(&mut buf)).await;
                        (handle, buf, result)
                    }));
                }
                State::Busy(mut fut) => match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        this.state = State::Busy(fut);
                        return Poll::Pending;
                    }
                    Poll::Ready((handle, buf, result)) => {
                        this.state = State::Idle(handle, buf);
                        match result {
                            Ok(n) => {
                                (this.pos, this.len) = (0, n);
                                if n == 0 {
                                    return Poll::Ready(Ok(0));
                                }
                            }
                            Err(e) => return Poll::Ready(Err(e.into())),
                        }
                    }
                },
                State::Empty => unreachable!("an AsyncReader is never left switching"),
            }
        }
    }
}

/// A seam handle that performs `Write` and `Flush`, as a futures-io
/// `AsyncWrite`.
///
/// Each write copies up to 8 KiB of the caller's data and performs a boxed
/// `SendPerform` write of it. A write that returns `Pending` is finished by
/// the next call, whose data is assumed to start with the same bytes, as
/// `write_all` and every other well-behaved caller ensures. Closing
/// flushes; to shut a stream down, perform `Shutdown` on it afterwards.
pub struct AsyncWriter<'a, H> {
    state: State<'a, H>,
}

impl<'a, H> AsyncWriter<'a, H>
where
    H: for<'b> SendPerform<Write<'b>> + SendPerform<Flush> + Send,
{
    /// Writes to `handle` for as long as the writer lives.
    pub fn new(handle: &'a mut H) -> Self {
        Self {
            state: State::Idle(handle, Vec::with_capacity(CHUNK)),
        }
    }

    /// Polls an operation in flight, or starts one with `start`.
    fn drive(
        &mut self,
        cx: &mut Context<'_>,
        start: impl FnOnce(&'a mut H, Vec<u8>) -> InFlight<'a, H>,
    ) -> Poll<std::io::Result<usize>> {
        let mut fut = match core::mem::replace(&mut self.state, State::Empty) {
            State::Idle(handle, buf) => start(handle, buf),
            State::Busy(fut) => fut,
            State::Empty => unreachable!("an AsyncWriter is never left switching"),
        };
        match fut.as_mut().poll(cx) {
            Poll::Pending => {
                self.state = State::Busy(fut);
                Poll::Pending
            }
            Poll::Ready((handle, buf, result)) => {
                self.state = State::Idle(handle, buf);
                Poll::Ready(result.map_err(Into::into))
            }
        }
    }
}

impl<H> core::fmt::Debug for AsyncWriter<'_, H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AsyncWriter").finish_non_exhaustive()
    }
}

impl<'a, H> AsyncWrite for AsyncWriter<'a, H>
where
    H: for<'b> SendPerform<Write<'b>> + SendPerform<Flush> + Send,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let chunk = &data[..data.len().min(CHUNK)];
        self.drive(cx, |handle, mut buf| {
            buf.clear();
            buf.extend_from_slice(chunk);
            Box::pin(async move {
                let result = handle.perform_send(Write(&buf)).await;
                (handle, buf, result)
            })
        })
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.drive(cx, |handle, buf| {
            Box::pin(async move {
                let result = handle.perform_send(Flush).await.map(|()| 0);
                (handle, buf, result)
            })
        })
        .map_ok(drop)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}
