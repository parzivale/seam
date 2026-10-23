use std::io;

use std::io::BufRead;

use seam_core::ops::stdio::{EPrint, Line, Print, ReadLine, ReadLineInto};
use tokio::io::AsyncWriteExt;

use crate::Tokio;

async_op!(perform ['a] Tokio, Print<'a>, |_s, op| {
    let mut out = tokio::io::stdout();
    out.write_all(op.0).await?;
    out.flush().await
});
async_op!(perform ['a] Tokio, EPrint<'a>, |_s, op| {
    let mut err = tokio::io::stderr();
    err.write_all(op.0).await?;
    err.flush().await
});
// Read through std's global stdin buffer, so that no input is lost between
// calls the way it would be with a fresh `BufReader` each time.
async_op!(perform [] Tokio, ReadLine, |_s, _op| {
    tokio::task::spawn_blocking(|| -> io::Result<Option<String>> {
        let mut line = String::new();
        match io::stdin().read_line(&mut line)? {
            0 => Ok(None),
            _ => Ok(Some(line)),
        }
    })
    .await
    .map_err(|_| seam_core::io::Error::other("the stdin reader was cancelled"))?
});

/// How much of a line one blocking read hands back. The line is read in
/// chunks this size, so that it needs no allocation and no buffer of
/// its own.
const CHUNK: usize = 512;

// Through std's stdin buffer, like `ReadLine`, a chunk at a time: the
// blocking task cannot borrow the caller's buffer, so it copies into an
// array it returns.
async_op!(perform ['a] Tokio, ReadLineInto<'a>, |_s, op| {
    let buf = op.0;
    let mut len = 0;
    while len < buf.len() {
        let want = (buf.len() - len).min(CHUNK);
        let (chunk, n, newline) = tokio::task::spawn_blocking(move || {
            let mut chunk = [0; CHUNK];
            fill_line(&mut io::stdin().lock(), &mut chunk[..want]).map(|(n, nl)| (chunk, n, nl))
        })
        .await
        .map_err(|_| seam_core::io::Error::other("the stdin reader was cancelled"))??;
        buf[len..len + n].copy_from_slice(&chunk[..n]);
        len += n;
        if newline || n < want {
            // A newline, or the end of input.
            return Ok((len > 0).then_some(Line { len, complete: true }));
        }
    }
    seam_core::io::Result::Ok(Some(Line { len, complete: false }))
});

/// Reads from `r`'s buffer into `buf` until a newline (included), the end of
/// input, or `buf` is full. Returns how much it read and whether it ended
/// at a newline.
fn fill_line(r: &mut impl BufRead, buf: &mut [u8]) -> io::Result<(usize, bool)> {
    let mut len = 0;
    while len < buf.len() {
        let available = match r.fill_buf() {
            Ok(a) => a,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            break;
        }
        let take = available.len().min(buf.len() - len);
        let (n, newline) = match available[..take].iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (take, false),
        };
        buf[len..len + n].copy_from_slice(&available[..n]);
        r.consume(n);
        len += n;
        if newline {
            return Ok((len, true));
        }
    }
    Ok((len, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_line_stops_at_newline_end_or_full() {
        let mut r = io::BufReader::with_capacity(3, &b"ab\ncdefg"[..]);
        let mut buf = [0; 4];
        assert_eq!(fill_line(&mut r, &mut buf).unwrap(), (3, true));
        assert_eq!(&buf[..3], b"ab\n");
        assert_eq!(fill_line(&mut r, &mut buf).unwrap(), (4, false));
        assert_eq!(&buf, b"cdef");
        assert_eq!(fill_line(&mut r, &mut buf).unwrap(), (1, false));
        assert_eq!(fill_line(&mut r, &mut buf).unwrap(), (0, false));
    }
}
