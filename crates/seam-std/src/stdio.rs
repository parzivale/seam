use std::io::{self, BufRead, Write};

use seam_core::Perform;
use seam_core::ops::stdio::{EPrint, Print, ReadLine};

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
