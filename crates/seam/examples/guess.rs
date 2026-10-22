//! A number guessing game.
//!
//! The game is a core: it needs randomness, a line of input at a time, and
//! somewhere to print, and its bounds say exactly that. `main` runs it
//! against the real world; the tests run it against a seeded simulation,
//! where the secret number is reproducible and stdin is scripted.
//!
//! ```text
//! cargo run -p seam --example guess --features std,test,io-std,random
//! ```

use std::cmp::Ordering;
use std::io;

use seam::ops::random::RandomU64;
use seam::ops::stdio::{Print, ReadLine};
use seam::prelude::*;

/// Plays one game. Returns the number of guesses, or `None` if the player
/// ran out of input.
fn play<C>(c: &mut C) -> io::Result<Option<u32>>
where
    C: Perform<RandomU64> + Perform<ReadLine> + for<'a> Perform<Print<'a>>,
{
    let secret = 1 + c.perform(RandomU64) % 100;
    say(c, "I'm thinking of a number from 1 to 100.\n")?;
    let mut guesses = 0;
    loop {
        say(c, "Your guess: ")?;
        let Some(line) = c.perform(ReadLine)? else {
            say(c, &format!("\nGiving up? It was {secret}.\n"))?;
            return Ok(None);
        };
        let Ok(guess) = line.trim().parse::<u64>() else {
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
}

fn say<C: for<'a> Perform<Print<'a>>>(c: &mut C, text: &str) -> io::Result<()> {
    c.perform(Print(text.as_bytes()))
}

/// The edge: the one place that picks the real world.
fn main() -> io::Result<()> {
    play(&mut seam::stdlib::Std::new()).map(drop)
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;

    use super::*;

    /// The secret a game with this seed will pick.
    fn secret(seed: u64) -> u64 {
        1 + Sim::with_seed(seed).perform(RandomU64) % 100
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
