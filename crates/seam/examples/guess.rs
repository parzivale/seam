//! A number guessing game.
//!
//! The game is a core: it needs randomness, standard input to read lines
//! from, and standard output to print to, and its bounds say exactly that. `main` runs it
//! against the real world; the tests run it against a seeded simulation,
//! where the secret number is reproducible and stdin is scripted.
//!
//! ```text
//! cargo run -p seam-rs --example guess --features std,test,io-std,random
//! ```

use seam::io;
use seam::ops::io::{BufReader, Line, Read, Write};
use seam::ops::random::{FillRandom, random_u64};
use seam::ops::stdio::{OpenStdin, OpenStdout, print};
use std::cmp::Ordering;

use seam::prelude::*;

/// Plays one game. Returns the number of guesses, or `None` if the player
/// ran out of input. `I` and `O` are the backend's standard input and
/// output; both are inferred from the backend.
fn play<C, I, O>(c: &mut C) -> io::Result<Option<u32>>
where
    C: for<'r> Perform<FillRandom<'r>> + Provide<OpenStdin<I>> + Provide<OpenStdout<O>>,
    I: for<'a> Perform<Read<'a>>,
    O: for<'a> Perform<Write<'a>>,
{
    let secret = 1 + random_u64(c) % 100;
    say(c, "I'm thinking of a number from 1 to 100.\n")?;
    // The game holds standard input for as long as it runs, and reads it a
    // line at a time through one reader.
    c.acquire(OpenStdin::new(), |c, stdin| {
        let mut reader = BufReader::<256>::new();
        let mut line = [0; 64];
        let mut guesses = 0;
        loop {
            say(c, "Your guess: ")?;
            let Some(Line { len, .. }) = reader.read_line(stdin, &mut line)? else {
                say(c, &format!("\nGiving up? It was {secret}.\n"))?;
                return Ok(None);
            };
            let text = core::str::from_utf8(&line[..len]).unwrap_or_default();
            let Ok(guess) = text.trim().parse::<u64>() else {
                say(c, "That's not a number.\n")?;
                continue;
            };
            guesses += 1;
            match guess.cmp(&secret) {
                Ordering::Less => say(c, "Too low.\n")?,
                Ordering::Greater => say(c, "Too high.\n")?,
                Ordering::Equal => {
                    say(c, &format!("Got it in {guesses}!\n"))?;
                    return Ok(Some(guesses));
                }
            }
        }
    })
}

fn say<C, O>(c: &mut C, text: &str) -> io::Result<()>
where
    C: Provide<OpenStdout<O>>,
    O: for<'a> Perform<Write<'a>>,
{
    print(c, text.as_bytes())
}

/// The edge: the one place that picks the real world.
fn main() -> io::Result<()> {
    play(&mut seam::stdlib::Std).map(drop)
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;

    use super::*;

    /// The secret a game with this seed will pick.
    fn secret(seed: u64) -> u64 {
        1 + random_u64(&mut Sim::with_seed(seed)) % 100
    }

    #[test]
    fn hints_then_wins() {
        let secret = secret(7);
        let mut sim = Sim::with_seed(7);
        sim.feed_stdin(format!("0\nlots\n101\n{secret}\n"));

        assert_eq!(play(&mut sim).unwrap(), Some(3));
        let out = String::from_utf8(sim.take_stdout()).unwrap();
        assert!(out.contains("Too low."));
        assert!(out.contains("That's not a number."));
        assert!(out.contains("Too high."));
        assert!(out.ends_with("Got it in 3!\n"));
    }

    #[test]
    fn end_of_input_reveals_the_number() {
        let mut sim = Sim::with_seed(1);
        assert_eq!(play(&mut sim).unwrap(), None);
        let out = String::from_utf8(sim.take_stdout()).unwrap();
        assert!(out.ends_with(&format!("It was {}.\n", secret(1))));
    }

    /// A strategy can be tested too: binary search always wins within 7.
    #[test]
    fn binary_search_wins_within_seven() {
        for seed in 0..50 {
            let secret = secret(seed);
            let (mut lo, mut hi, mut script) = (1, 100, String::new());
            loop {
                let mid = (lo + hi) / 2;
                script.push_str(&format!("{mid}\n"));
                match mid.cmp(&secret) {
                    Ordering::Less => lo = mid + 1,
                    Ordering::Greater => hi = mid - 1,
                    Ordering::Equal => break,
                }
            }
            let mut sim = Sim::with_seed(seed);
            sim.feed_stdin(script);
            assert!(play(&mut sim).unwrap().unwrap() <= 7);
        }
    }
}
