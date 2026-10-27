//! Concurrent helpers for child processes.

use std::process::Command;
use std::vec::Vec;

use futures::future::join3;
use seam_core::io;
use seam_core::ops::io::{Read, Write};
use seam_core::ops::process::{ChildPipes, CloseStdin, Output, Pipe, SpawnChild, SplitChild, Wait};

two_ways! {
    /// Runs `command` with `input` on its standard input, and collects
    /// both of its outputs and its exit code: `RunCommand`, with input,
    /// built from `SpawnChild` and [`SplitChild`]. It feeds the input
    /// while draining both outputs, so a child that writes a lot before
    /// reading all its input cannot block it.
    ///
    /// Fails if the child cannot be spawned or a pipe fails; a child that
    /// exits with an error is an `Output` with that code.
    pub async fn pipe_through_async, pipe_through_send
    [C, P, I, O, E] (c: &mut C, command: Command, input: &[u8]) -> io::Result<Output>
    where
        async {
            C: seam_core::AsyncProvide<SpawnChild<P>>,
            P: seam_core::AsyncProvide<SplitChild<I, O, E>> + seam_core::future::AsyncPerform<Wait>,
            I: for<'a> seam_core::future::AsyncPerform<Write<'a>>
                + seam_core::future::AsyncPerform<CloseStdin>,
            O: for<'a> seam_core::future::AsyncPerform<Read<'a>>,
            E: for<'a> seam_core::future::AsyncPerform<Read<'a>>,
        }
        send {
            C: seam_core::SendProvide<SpawnChild<P>> + Send,
            P: seam_core::SendProvide<SplitChild<I, O, E>> + seam_core::future::SendPerform<Wait> + Send,
            I: for<'a> seam_core::future::SendPerform<Write<'a>>
                + seam_core::future::SendPerform<CloseStdin>
                + Send,
            O: for<'a> seam_core::future::SendPerform<Read<'a>> + Send,
            E: for<'a> seam_core::future::SendPerform<Read<'a>> + Send,
        }
    {
        let spawn = SpawnChild::new(command)
            .stdin(Pipe::Piped)
            .stdout(Pipe::Piped)
            .stderr(Pipe::Piped);
        acquire!(c, spawn, |_, child| {
            let (stdout, stderr) = acquire!(child, SplitChild::new(), |_, ChildPipes { stdin, stdout, stderr }| {
                let feed = async {
                    let fed = call!(seam_core::ops::io::write_all_async, seam_core::ops::io::write_all_send; (stdin, input)).await;
                    // End the input even if writing failed (the child may
                    // have exited without reading it all).
                    run!(stdin, CloseStdin);
                    match fed {
                        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
                        fed => fed,
                    }
                };
                let mut out = Vec::new();
                let mut err = Vec::new();
                let (fed, read_out, read_err) = join3(
                    feed,
                    call!(seam_core::ops::io::read_to_end_async, seam_core::ops::io::read_to_end_send; (stdout, &mut out)),
                    call!(seam_core::ops::io::read_to_end_async, seam_core::ops::io::read_to_end_send; (stderr, &mut err)),
                )
                .await;
                fed?;
                read_out?;
                read_err?;
                io::Result::Ok((out, err))
            })?;
            let status = run!(child, Wait)?;
            io::Result::Ok(Output { code: status.code, stdout, stderr })
        })
    }
}
