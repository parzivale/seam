//! A two-stage pipeline in one task, over futures-rs (`seam::futures`): a
//! reader sends lines down a channel, a counter counts their words, and
//! both run at once under `join!`, the counter giving up on a quiet
//! channel with `within`.
//!
//! The core takes its channels from `Futures` (which needs no runtime) and
//! its clock from whatever the root has: smol in the program, and the
//! simulated runtime's virtual time in the tests.
//!
//! ```text
//! cargo run -p seam-rs --example word_pipe --features std,test,futures,smol,sync,time
//! ```

use std::time::Duration;

use seam::futures::sync::{Receiver, Sender};
use seam::ops::sync::{Channel, RecvMsg, SendMsg};
use seam::ops::time::Sleep;
use seam::prelude::*;

/// Sends `text`'s lines down a channel while counting the words that come
/// out the other end. Stops counting once nothing arrives for `quiet`.
async fn count_words<C>(c: &mut C, text: &str, quiet: Duration) -> (usize, usize)
where
    C: Perform<Channel<Sender<String>, Receiver<String>>> + AsyncPerform<Sleep>,
{
    let (mut tx, mut rx): (Sender<String>, Receiver<String>) = c.perform(Channel::bounded(2));
    let produce = async move {
        let mut lines = 0;
        for line in text.lines() {
            if tx.perform_async(SendMsg(line.to_owned())).await.is_err() {
                break;
            }
            lines += 1;
        }
        lines
    };
    let consume = async {
        let mut words = 0;
        while let Some(Ok(line)) =
            seam::futures::time::within(c, quiet, rx.perform_async(RecvMsg::new())).await
        {
            words += line.split_whitespace().count();
        }
        words
    };
    seam::futures::join!(produce, consume)
}

/// The edge: futures' channels, smol's clock.
fn main() {
    use seam::effects as fx;
    let text = "the quick brown fox\njumps over\nthe lazy dog";
    let (lines, words) = seam::smol::block_on(async {
        let mut root = seam::root![
            seam::futures::Futures<fx::Channel>,
            seam::smol::Smol<fx::Sleep>
        ];
        count_words(&mut root, text, Duration::from_millis(100)).await
    });
    println!("{lines} lines, {words} words");
}

#[cfg(test)]
mod tests {
    use seam::effects as fx;
    use seam::test::Sim;
    use seam::test::rt::{AsyncSim, Runtime};

    use super::*;

    #[test]
    fn counts_every_word_under_every_schedule() {
        let text = "one two\nthree\n\nfour five six";
        for seed in 0..8 {
            let rt = Runtime::with_seed(Sim::new(), seed);
            let got = rt.block_on(|sim| async move {
                let mut root = seam::root![
                    seam::futures::Futures<fx::Channel>,
                    AsyncSim<fx::Sleep>::clone(&sim),
                ];
                count_words(&mut root, text, Duration::from_secs(1)).await
            });
            assert_eq!(got, (4, 6), "seed {seed}");
        }
    }
}
