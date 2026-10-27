//! Concurrent helpers for streams.

use futures::future::join;
use seam_core::io;
use seam_core::ops::io::{Flush, Halves, Read, Split, Write};
use seam_core::ops::net::{How, Shutdown};

two_ways! {
    /// Copies `a` to `b` and `b` to `a` at the same time, until each side
    /// ends its input, as a proxy does. When one direction ends, the other
    /// side's write half is flushed and shut down, so its peer sees the
    /// end too. Returns how many bytes went each way: `(a to b, b to a)`.
    ///
    /// Both streams are [`Split`] for the copy, and whole again after.
    pub async fn copy_bidirectional_async, copy_bidirectional_send
    [A, B, AR, AW, BR, BW] (a: &mut A, b: &mut B) -> io::Result<(u64, u64)>
    where
        async {
            A: seam_core::AsyncProvide<Split<AR, AW>>,
            B: seam_core::AsyncProvide<Split<BR, BW>>,
            AR: for<'x> seam_core::future::AsyncPerform<Read<'x>>,
            BR: for<'x> seam_core::future::AsyncPerform<Read<'x>>,
            AW: for<'x> seam_core::future::AsyncPerform<Write<'x>>
                + seam_core::future::AsyncPerform<Flush>
                + seam_core::future::AsyncPerform<Shutdown>,
            BW: for<'x> seam_core::future::AsyncPerform<Write<'x>>
                + seam_core::future::AsyncPerform<Flush>
                + seam_core::future::AsyncPerform<Shutdown>,
        }
        send {
            A: seam_core::SendProvide<Split<AR, AW>> + Send,
            B: seam_core::SendProvide<Split<BR, BW>> + Send,
            AR: for<'x> seam_core::future::SendPerform<Read<'x>> + Send,
            BR: for<'x> seam_core::future::SendPerform<Read<'x>> + Send,
            AW: for<'x> seam_core::future::SendPerform<Write<'x>>
                + seam_core::future::SendPerform<Flush>
                + seam_core::future::SendPerform<Shutdown>
                + Send,
            BW: for<'x> seam_core::future::SendPerform<Write<'x>>
                + seam_core::future::SendPerform<Flush>
                + seam_core::future::SendPerform<Shutdown>
                + Send,
        }
    {
        acquire!(a, Split::new(), |_, Halves { read: a_read, write: a_write }| {
            acquire!(b, Split::new(), |_, Halves { read: b_read, write: b_write }| {
                let there = async {
                    let mut buf = [0; 8192];
                    let n = call!(seam_core::ops::io::copy_async, seam_core::ops::io::copy_send; (a_read, b_write, &mut buf)).await?;
                    run!(b_write, Flush)?;
                    run!(b_write, Shutdown(How::Write))?;
                    io::Result::Ok(n)
                };
                let back = async {
                    let mut buf = [0; 8192];
                    let n = call!(seam_core::ops::io::copy_async, seam_core::ops::io::copy_send; (b_read, a_write, &mut buf)).await?;
                    run!(a_write, Flush)?;
                    run!(a_write, Shutdown(How::Write))?;
                    io::Result::Ok(n)
                };
                let (there, back) = join(there, back).await;
                io::Result::Ok((there?, back?))
            })
        })
    }
}
