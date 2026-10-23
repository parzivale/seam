use std::io::{self, BufRead, Write};

use seam_core::Perform;
use seam_core::ops::stdio::{EPrint, Line, Print, ReadLine, ReadLineInto};

use crate::Std;

impl Perform<Print<'_>> for Std {
    fn perform(&mut self, op: Print<'_>) -> seam_core::io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(op.0)?;
        crate::sio(out.flush())
    }
}

impl Perform<EPrint<'_>> for Std {
    fn perform(&mut self, op: EPrint<'_>) -> seam_core::io::Result<()> {
        crate::sio(io::stderr().lock().write_all(op.0))
    }
}

impl Perform<ReadLine> for Std {
    fn perform(&mut self, _: ReadLine) -> seam_core::io::Result<Option<String>> {
        let mut line = String::new();
        match io::stdin().lock().read_line(&mut line)? {
            0 => Ok(None),
            _ => Ok(Some(line)),
        }
    }
}

impl Perform<ReadLineInto<'_>> for Std {
    fn perform(&mut self, op: ReadLineInto<'_>) -> seam_core::io::Result<Option<Line>> {
        crate::sio(read_line_into(&mut io::stdin().lock(), op.0))
    }
}

/// Reads from `r`'s buffer into `buf`, up to and including a newline,
/// leaving the rest buffered. Allocates nothing.
fn read_line_into(r: &mut impl BufRead, buf: &mut [u8]) -> io::Result<Option<Line>> {
    let mut len = 0;
    while len < buf.len() {
        let available = match r.fill_buf() {
            Ok(a) => a,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok((len > 0).then_some(Line {
                len,
                complete: true,
            }));
        }
        let room = &mut buf[len..];
        let take = available.len().min(room.len());
        let (n, newline) = match available[..take].iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (take, false),
        };
        room[..n].copy_from_slice(&available[..n]);
        r.consume(n);
        len += n;
        if newline {
            return Ok(Some(Line {
                len,
                complete: true,
            }));
        }
    }
    Ok(Some(Line {
        len,
        complete: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &[u8], cap: usize) -> Vec<(Vec<u8>, bool)> {
        let mut r = io::BufReader::with_capacity(3, input);
        let mut buf = vec![0; cap];
        let mut out = Vec::new();
        while let Some(line) = read_line_into(&mut r, &mut buf).unwrap() {
            out.push((buf[..line.len].to_vec(), line.complete));
        }
        out
    }

    #[test]
    fn lines_split_across_reads_and_buffers() {
        assert_eq!(
            lines(b"ab\ncdefg\nh", 4),
            [
                (b"ab\n".to_vec(), true),
                (b"cdef".to_vec(), false),
                (b"g\n".to_vec(), true),
                (b"h".to_vec(), true),
            ]
        );
        assert_eq!(lines(b"", 4), []);
        assert_eq!(
            lines(b"\n\n", 1),
            [(b"\n".to_vec(), true), (b"\n".to_vec(), true)]
        );
    }
}
