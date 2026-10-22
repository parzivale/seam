use std::io;

use seam_core::ops::stdio::{EPrint, Print, ReadLine};
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
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match io::stdin().read_line(&mut line)? {
            0 => Ok(None),
            _ => Ok(Some(line)),
        }
    })
    .await
    .map_err(io::Error::other)?
});
