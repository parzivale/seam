//! Reading and writing through an open handle: a file, a socket, a child
//! process, standard input or output.
//!
//! The operations are the primitives [`Read`], [`Write`], [`Flush`] and
//! [`Seek`]. Everything else is built on them here, as plain functions
//! rather than operations: [`read_exact`], [`write_all`], [`read_to_end`],
//! [`copy`], and [`BufReader`] for lines and other delimited records. Each
//! comes in three forms, for sync capabilities, async ones (`_async`) and
//! async ones on a multithreaded runtime (`_send`).
//!
//! The operations borrow their buffer, so bound them for every lifetime:
//!
//! ```
//! use seam_core::io;
//! use seam_core::Perform;
//! use seam_core::ops::io::{self as ops, Flush, Read, Write};
//!
//! /// Copies everything from `src` to `dst`, in 4 KiB chunks.
//! fn pump<R, W>(src: &mut R, dst: &mut W) -> io::Result<u64>
//! where
//!     R: for<'a> Perform<Read<'a>>,
//!     W: for<'a> Perform<Write<'a>> + Perform<Flush>,
//! {
//!     let n = ops::copy(src, dst, &mut [0; 4096])?;
//!     dst.perform(Flush)?;
//!     Ok(n)
//! }
//! ```

use crate::io;
use crate::three_ways::three_ways;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// Read into the buffer. Returns how many bytes were read; 0 means end of
/// input (or an empty buffer). On a stream it waits for at least one byte,
/// and returns what has arrived without waiting for the buffer to fill.
#[derive(Debug)]
pub struct Read<'a>(pub &'a mut [u8]);

/// Write some of the buffer. Returns how many bytes were written.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a>(pub &'a [u8]);

/// Flush buffered writes.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flush;

/// Move a seekable handle (a file) to a new position. Returns the new
/// position, in bytes from the start. Seeking past the end is allowed; a
/// write there fills the gap with zeros. Seeking before the start fails
/// with `io::ErrorKind::InvalidInput`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seek(pub SeekFrom);

/// Where a [`Seek`] counts from: the same choices as `std::io::SeekFrom`,
/// which converts into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeekFrom {
    /// This many bytes from the start.
    Start(u64),
    /// This many bytes from the end, usually negative.
    End(i64),
    /// This many bytes from the current position.
    Current(i64),
}

#[cfg(feature = "std")]
impl From<std::io::SeekFrom> for SeekFrom {
    fn from(from: std::io::SeekFrom) -> Self {
        match from {
            std::io::SeekFrom::Start(n) => Self::Start(n),
            std::io::SeekFrom::End(n) => Self::End(n),
            std::io::SeekFrom::Current(n) => Self::Current(n),
        }
    }
}

#[cfg(feature = "std")]
impl From<SeekFrom> for std::io::SeekFrom {
    fn from(from: SeekFrom) -> Self {
        match from {
            SeekFrom::Start(n) => Self::Start(n),
            SeekFrom::End(n) => Self::End(n),
            SeekFrom::Current(n) => Self::Current(n),
        }
    }
}

operations! {
    Read<'_> => io::Result<usize>;
    Write<'_> => io::Result<usize>;
    Flush => io::Result<()>;
    Seek => io::Result<u64>;
}

/// Each gives up with `io::ErrorKind::TimedOut`, having read or written
/// nothing.
#[cfg(feature = "time")]
mod timeouts {
    use super::*;
    use crate::ops::time::{TimesOut, io_timed_out};

    impl TimesOut for Read<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }

    impl TimesOut for Write<'_> {
        type Expired = io::Result<usize>;
        fn timed_out(self) -> Self::Expired {
            Err(io_timed_out())
        }
    }
}

fn eof() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the input ended before the buffer was full",
    )
}

fn write_zero() -> io::Error {
    io::Error::new(
        io::ErrorKind::WriteZero,
        "the handle accepted no more bytes",
    )
}

three_ways! {
    /// Fills the whole buffer from `h`, reading as often as it takes. Fails
    /// with `io::ErrorKind::UnexpectedEof` if the input ends first; what
    /// arrived is then left at the start of the buffer.
    pub fn read_exact, read_exact_async, read_exact_send
    [H] (h: &mut H, mut buf: &mut [u8]) -> io::Result<()>
    where
        sync { H: for<'a> crate::Perform<Read<'a>> }
        async { H: for<'a> crate::future::AsyncPerform<Read<'a>> }
        send { H: for<'a> crate::future::SendPerform<Read<'a>> + Send }
    {
        while !buf.is_empty() {
            match run!(h, Read(&mut *buf)) {
                Ok(0) => return Err(eof()),
                Ok(n) => buf = &mut core::mem::take(&mut buf)[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Writes the whole buffer to `h`, writing as often as it takes. Fails
    /// with `io::ErrorKind::WriteZero` if the handle stops accepting bytes;
    /// part of the buffer may have been written.
    pub fn write_all, write_all_async, write_all_send
    [H] (h: &mut H, mut buf: &[u8]) -> io::Result<()>
    where
        sync { H: for<'a> crate::Perform<Write<'a>> }
        async { H: for<'a> crate::future::AsyncPerform<Write<'a>> }
        send { H: for<'a> crate::future::SendPerform<Write<'a>> + Send }
    {
        while !buf.is_empty() {
            match run!(h, Write(buf)) {
                Ok(0) => return Err(write_zero()),
                Ok(n) => buf = &buf[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Copies everything from `src` to `dst` until `src` ends, through
    /// `buf`. Returns how many bytes were copied. Does not flush `dst`.
    pub fn copy, copy_async, copy_send
    [R, W] (src: &mut R, dst: &mut W, buf: &mut [u8]) -> io::Result<u64>
    where
        sync { R: for<'a> crate::Perform<Read<'a>>, W: for<'a> crate::Perform<Write<'a>> }
        async {
            R: for<'a> crate::future::AsyncPerform<Read<'a>>,
            W: for<'a> crate::future::AsyncPerform<Write<'a>>,
        }
        send {
            R: for<'a> crate::future::SendPerform<Read<'a>> + Send,
            W: for<'a> crate::future::SendPerform<Write<'a>> + Send,
        }
    {
        let mut total = 0;
        loop {
            let n = match run!(src, Read(&mut *buf)) {
                Ok(0) => return Ok(total),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            call!(write_all, write_all_async, write_all_send; (dst, &buf[..n]))?;
            total += n as u64;
        }
    }
}

#[cfg(feature = "alloc")]
three_ways! {
    /// Reads from `h` until its input ends, appending to `out`. Returns how
    /// many bytes were read. On an error, what arrived is left in `out`.
    /// Needs the `alloc` feature.
    pub fn read_to_end, read_to_end_async, read_to_end_send
    [H] (h: &mut H, out: &mut Vec<u8>) -> io::Result<usize>
    where
        sync { H: for<'a> crate::Perform<Read<'a>> }
        async { H: for<'a> crate::future::AsyncPerform<Read<'a>> }
        send { H: for<'a> crate::future::SendPerform<Read<'a>> + Send }
    {
        let start = out.len();
        let mut chunk = [0; 4096];
        loop {
            match run!(h, Read(&mut chunk)) {
                Ok(0) => return Ok(out.len() - start),
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

/// What [`BufReader::read_until`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Line {
    /// How many bytes were read, from the start of the buffer.
    pub len: usize,
    /// Whether the record ended: with the delimiter, which is then the last
    /// byte read, or with the end of input. False when the buffer filled
    /// first; the next read continues the same record.
    pub complete: bool,
}

/// Reads delimited records (lines, or anything else ending in a known
/// byte) from a handle, through a fixed buffer of `N` bytes. No
/// allocation, no std.
///
/// A read can return more than one record. What comes after the
/// delimiter stays in this buffer for the next call, so read a stream
/// through one `BufReader` for as long as it is in use; [`read`](Self::read)
/// serves the buffered bytes first, for mixing records with raw bytes.
///
/// The reader does not own the handle: pass it to each call, so a reader
/// can outlive the scope a handle is borrowed in.
///
/// ```
/// use seam_core::io;
/// use seam_core::Perform;
/// use seam_core::ops::io::{BufReader, Line, Read};
///
/// /// Counts the newlines in `src`, as `wc -l` does, however long the lines.
/// fn count_lines<R: for<'a> Perform<Read<'a>>>(src: &mut R) -> io::Result<u64> {
///     let mut reader = BufReader::<512>::new();
///     let mut line = [0; 256];
///     let mut n = 0;
///     while let Some(Line { len, .. }) = reader.read_line(src, &mut line)? {
///         n += u64::from(line[..len].ends_with(b"\n"));
///     }
///     Ok(n)
/// }
/// ```
#[derive(Debug, Clone)]
pub struct BufReader<const N: usize> {
    buf: [u8; N],
    start: usize,
    end: usize,
}

impl<const N: usize> Default for BufReader<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BufReader<N> {
    /// An empty reader.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            start: 0,
            end: 0,
        }
    }

    /// The bytes read from the handle but not yet handed out.
    pub fn buffered(&self) -> &[u8] {
        &self.buf[self.start..self.end]
    }

    /// Hands out buffered bytes into `out`, up to the delimiter (included).
    /// Returns how many, and whether the delimiter was among them.
    fn take_until(&mut self, delim: u8, out: &mut [u8]) -> (usize, bool) {
        let avail = &self.buf[self.start..self.end];
        let take = avail.len().min(out.len());
        let (n, found) = match avail[..take].iter().position(|&b| b == delim) {
            Some(i) => (i + 1, true),
            None => (take, false),
        };
        out[..n].copy_from_slice(&avail[..n]);
        self.start += n;
        (n, found)
    }

    /// Hands out buffered bytes into `out`. Returns how many.
    fn take(&mut self, out: &mut [u8]) -> usize {
        let n = (self.end - self.start).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.start..self.start + n]);
        self.start += n;
        n
    }

    three_ways! {
        /// Reads up to and including the next `delim` byte into `out`.
        /// `None` at the end of input. A record longer than `out` fills it
        /// and comes back with [`complete`](Line::complete) false; the next
        /// call continues it. A record cut off by the end of input counts as
        /// complete. An empty `out` reads nothing.
        pub fn read_until, read_until_async, read_until_send
        [H] (&mut self, h: &mut H, delim: u8, out: &mut [u8]) -> io::Result<Option<Line>>
        where
            sync { H: for<'a> crate::Perform<Read<'a>> }
            async { H: for<'a> crate::future::AsyncPerform<Read<'a>> }
            send { H: for<'a> crate::future::SendPerform<Read<'a>> + Send }
        {
            let mut len = 0;
            while len < out.len() {
                if self.start == self.end {
                    self.start = 0;
                    self.end = match run!(h, Read(&mut self.buf)) {
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    };
                    if self.end == 0 {
                        // The end of input ends the record, if there is one.
                        return Ok((len > 0).then_some(Line { len, complete: true }));
                    }
                }
                let (n, found) = self.take_until(delim, &mut out[len..]);
                len += n;
                if found {
                    return Ok(Some(Line { len, complete: true }));
                }
            }
            Ok(Some(Line { len, complete: false }))
        }

        /// [`read_until`](Self::read_until) a newline.
        pub fn read_line, read_line_async, read_line_send
        [H] (&mut self, h: &mut H, out: &mut [u8]) -> io::Result<Option<Line>>
        where
            sync { H: for<'a> crate::Perform<Read<'a>> }
            async { H: for<'a> crate::future::AsyncPerform<Read<'a>> }
            send { H: for<'a> crate::future::SendPerform<Read<'a>> + Send }
        {
            call!(Self::read_until, Self::read_until_async, Self::read_until_send; (self, h, b'\n', out))
        }

        /// Reads into `out`: buffered bytes first, then from `h` once the
        /// buffer is empty. Returns how many bytes; 0 at the end of input.
        pub fn read, read_async, read_send
        [H] (&mut self, h: &mut H, out: &mut [u8]) -> io::Result<usize>
        where
            sync { H: for<'a> crate::Perform<Read<'a>> }
            async { H: for<'a> crate::future::AsyncPerform<Read<'a>> }
            send { H: for<'a> crate::future::SendPerform<Read<'a>> + Send }
        {
            if self.start < self.end {
                return Ok(self.take(out));
            }
            run!(h, Read(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Perform;

    /// Hands out its input `chunk` bytes at a time.
    struct Chunks<'a> {
        input: &'a [u8],
        chunk: usize,
    }

    impl crate::Capability for Chunks<'_> {}

    impl Perform<Read<'_>> for Chunks<'_> {
        fn perform(&mut self, op: Read<'_>) -> io::Result<usize> {
            let n = self.input.len().min(op.0.len()).min(self.chunk);
            op.0[..n].copy_from_slice(&self.input[..n]);
            self.input = &self.input[n..];
            Ok(n)
        }
    }

    fn lines(input: &[u8], chunk: usize, out: usize) -> std::vec::Vec<(std::vec::Vec<u8>, bool)> {
        let mut src = Chunks { input, chunk };
        let mut reader = BufReader::<3>::new();
        let mut buf = std::vec![0; out];
        let mut got = std::vec::Vec::new();
        while let Some(Line { len, complete }) = reader.read_line(&mut src, &mut buf).unwrap() {
            got.push((buf[..len].to_vec(), complete));
        }
        got
    }

    #[test]
    fn lines_across_reads_and_buffers() {
        for chunk in [1, 2, 5, 100] {
            assert_eq!(
                lines(b"ab\ncdefg\nh", chunk, 4),
                [
                    (b"ab\n".to_vec(), true),
                    (b"cdef".to_vec(), false),
                    (b"g\n".to_vec(), true),
                    (b"h".to_vec(), true),
                ],
                "chunks of {chunk}"
            );
        }
        assert_eq!(lines(b"", 2, 4), []);
        assert_eq!(
            lines(b"\n\n", 2, 1),
            [(b"\n".to_vec(), true), (b"\n".to_vec(), true)]
        );
    }

    #[test]
    fn raw_reads_get_the_buffered_bytes_first() {
        let mut src = Chunks {
            input: b"len 4\nbody",
            chunk: 100,
        };
        let mut reader = BufReader::<16>::new();
        let mut header = [0; 8];
        let line = reader.read_line(&mut src, &mut header).unwrap().unwrap();
        assert_eq!(&header[..line.len], b"len 4\n");
        assert_eq!(reader.buffered(), b"body");
        let mut body = [0; 4];
        assert_eq!(reader.read(&mut src, &mut body).unwrap(), 4);
        assert_eq!(&body, b"body");
        assert_eq!(reader.read(&mut src, &mut body).unwrap(), 0);
    }

    #[test]
    fn exact() {
        let mut src = Chunks {
            input: b"hello",
            chunk: 2,
        };
        let mut buf = [0; 3];
        read_exact(&mut src, &mut buf).unwrap();
        assert_eq!(&buf, b"hel");
        let err = read_exact(&mut src, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(&buf[..2], b"lo", "what arrived is kept");
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn to_end() {
        let mut src = Chunks {
            input: b"abcdef",
            chunk: 4,
        };
        let mut all = std::vec::Vec::new();
        assert_eq!(
            crate::future::block_on(read_to_end_async(&mut src, &mut all)).unwrap(),
            6
        );
        assert_eq!(all, b"abcdef");
    }
}
