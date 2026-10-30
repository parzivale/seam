//! Seam sources as `Stream`s, for `futures::StreamExt` and anything else
//! written against `Stream`.
//!
//! Each borrows its source for as long as the stream lives, so it is used
//! inside the source's scope, and each comes in two forms: one over the
//! async traits, and a `_send` one whose stream is `Send`.
//!
//! - `messages`: a channel receiver's messages, until every sender is gone.
//! - `lines` (`io`): a handle's lines, without their line endings.
//! - `entries` (`fs`): a directory's entries, from a `ReadDir` scope.

// futures' `stream`: its combinators and constructors.
pub use futures::stream::*;

#[cfg(feature = "sync")]
pub use self::sync::{messages, messages_send};

#[cfg(feature = "sync")]
mod sync {
    use futures::Stream;
    use seam_core::future::{AsyncPerform, SendPerform};
    use seam_core::ops::sync::RecvMsg;

    /// The messages `rx` receives, ending when every sender is gone (or,
    /// on a handler that cannot wait, when none is waiting).
    pub fn messages<R, T>(rx: &mut R) -> impl Stream<Item = T> + '_
    where
        R: AsyncPerform<RecvMsg<T>>,
    {
        futures::stream::unfold(rx, |rx| async move {
            let message = rx.perform_async(RecvMsg::new()).await.ok()?;
            Some((message, rx))
        })
    }

    /// [`messages`], as a `Send` stream.
    pub fn messages_send<R, T>(rx: &mut R) -> impl Stream<Item = T> + Send + '_
    where
        R: SendPerform<RecvMsg<T>> + Send,
        T: Send,
    {
        futures::stream::unfold(rx, |rx| async move {
            let message = rx.perform_send(RecvMsg::new()).await.ok()?;
            Some((message, rx))
        })
    }
}

#[cfg(feature = "io")]
pub use self::lines::{lines, lines_send};

#[cfg(feature = "io")]
mod lines {
    use std::string::String;
    use std::vec::Vec;

    use futures::Stream;
    use seam_core::future::{AsyncPerform, SendPerform};
    use seam_core::io;
    use seam_core::ops::io::Read;

    /// Read so far and not yet returned, and whether the handle has ended.
    struct Pending<'h, H> {
        handle: &'h mut H,
        buf: Vec<u8>,
        ended: bool,
    }

    impl<H> Pending<'_, H> {
        /// The next whole line in the buffer, or at the end, what is left.
        fn take_line(&mut self) -> Option<io::Result<String>> {
            let end = match self.buf.iter().position(|&b| b == b'\n') {
                Some(i) => i + 1,
                None if self.ended && !self.buf.is_empty() => self.buf.len(),
                None => return None,
            };
            let mut line: Vec<u8> = self.buf.drain(..end).collect();
            if line.last() == Some(&b'\n') {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            Some(String::from_utf8(line).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "a line is not valid UTF-8")
            }))
        }
    }

    macro_rules! lines_fn {
        ($(#[$m:meta])* $name:ident, $method:ident, [$($bounds:tt)*], [$($send:tt)*]) => {
            $(#[$m])*
            pub fn $name<H>(handle: &mut H) -> impl Stream<Item = io::Result<String>> $($send)* + '_
            where
                H: $($bounds)*,
            {
                let start = Pending { handle, buf: Vec::new(), ended: false };
                futures::stream::unfold(Some(start), |state| async move {
                    let mut state = state?;
                    loop {
                        if let Some(line) = state.take_line() {
                            let failed = line.is_err();
                            return Some((line, (!failed).then_some(state)));
                        }
                        if state.ended {
                            return None;
                        }
                        let mut chunk = [0; 4096];
                        match state.handle.$method(Read(&mut chunk)).await {
                            Ok(0) => state.ended = true,
                            Ok(n) => state.buf.extend_from_slice(&chunk[..n]),
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            // The error, and then the stream ends.
                            Err(e) => return Some((Err(e), None)),
                        }
                    }
                })
            }
        };
    }

    lines_fn!(
        /// The lines `handle` reads, without `\n` or `\r\n`; a last line
        /// with no newline is returned as is. A line that is not UTF-8
        /// fails with `io::ErrorKind::InvalidData`, and a read error is
        /// returned; either ends the stream.
        lines, perform_async, [for<'a> AsyncPerform<Read<'a>>], []
    );
    lines_fn!(
        /// [`lines`], as a `Send` stream.
        lines_send, perform_send, [for<'a> SendPerform<Read<'a>> + Send], [+ Send]
    );
}

#[cfg(feature = "fs")]
pub use self::entries::{entries, entries_send};

#[cfg(feature = "fs")]
mod entries {
    use std::path::PathBuf;

    use futures::Stream;
    use seam_core::future::{AsyncPerform, SendPerform};
    use seam_core::io;
    use seam_core::ops::fs::NextEntry;

    /// The paths in a directory opened with `ReadDir`, from the `Dir` the
    /// scope lends. An error is returned and ends the stream.
    pub fn entries<D>(dir: &mut D) -> impl Stream<Item = io::Result<PathBuf>> + '_
    where
        D: AsyncPerform<NextEntry>,
    {
        futures::stream::unfold(Some(dir), |dir| async move {
            let dir = dir?;
            match dir.perform_async(NextEntry).await {
                Ok(Some(path)) => Some((Ok(path), Some(dir))),
                Ok(None) => None,
                Err(e) => Some((Err(e), None)),
            }
        })
    }

    /// [`entries`], as a `Send` stream.
    pub fn entries_send<D>(dir: &mut D) -> impl Stream<Item = io::Result<PathBuf>> + Send + '_
    where
        D: SendPerform<NextEntry> + Send,
    {
        futures::stream::unfold(Some(dir), |dir| async move {
            let dir = dir?;
            match dir.perform_send(NextEntry).await {
                Ok(Some(path)) => Some((Ok(path), Some(dir))),
                Ok(None) => None,
                Err(e) => Some((Err(e), None)),
            }
        })
    }
}
