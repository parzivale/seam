//! Dice rolls, with rand's API over seam's randomness (`seam::rand`).
//!
//! The core parses dice like `3d6+2` and rolls them with `RngExt`, drawing
//! through `FillRandom`. The program takes the system's randomness; the
//! tests take a seeded simulation, where every roll is reproducible.
//!
//! ```text
//! cargo run -p seam-rs --example dice --features std,test,rand,env -- 3d6+2 1d20
//! ```

use seam::ops::random::FillRandom;
use seam::prelude::*;
use seam::rand::RngExt;
use seam::rand::seq::IndexedRandom;

/// Dice to roll: `count` dice of `sides` sides, plus `bonus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Dice {
    count: u32,
    sides: u32,
    bonus: i64,
}

impl std::str::FromStr for Dice {
    type Err = String;

    /// `NdS`, `NdS+B` or `NdS-B`; `dS` is one die.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("`{s}` is not dice, like 3d6 or 1d20+2");
        let (count, rest) = s.split_once('d').ok_or_else(bad)?;
        let count = if count.is_empty() {
            1
        } else {
            count.parse().map_err(|_| bad())?
        };
        let (sides, bonus) = match rest.find(['+', '-']) {
            Some(at) => (&rest[..at], rest[at..].parse().map_err(|_| bad())?),
            None => (rest, 0),
        };
        let sides: u32 = sides.parse().map_err(|_| bad())?;
        if count == 0 || sides == 0 {
            return Err(bad());
        }
        Ok(Self {
            count,
            sides,
            bonus,
        })
    }
}

/// Each die's face, and the total with the bonus.
fn roll<C>(c: &mut C, dice: Dice) -> (Vec<u32>, i64)
where
    C: for<'a> Perform<FillRandom<'a>>,
{
    let mut rng = seam::rand::rng(c);
    let faces: Vec<u32> = (0..dice.count)
        .map(|_| rng.random_range(1..=dice.sides))
        .collect();
    let total = faces.iter().map(|&f| i64::from(f)).sum::<i64>() + dice.bonus;
    (faces, total)
}

/// A flavour line, chosen at random.
fn cheer<C>(c: &mut C) -> &'static str
where
    C: for<'a> Perform<FillRandom<'a>>,
{
    [
        "The dice have spoken.",
        "Fortune favours you.",
        "Roll again?",
    ]
    .choose(&mut seam::rand::rng(c))
    .expect("not empty")
}

/// The edge: the system's randomness, and the arguments.
fn main() {
    use seam::effects as fx;
    let mut root = seam::root![seam::stdlib::Std<fx::FillRandom, fx::Args>];
    let args: Vec<String> = root
        .inspect(seam::ops::env::Args)
        .into_iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let specs = if args.is_empty() {
        vec!["2d6".to_owned()]
    } else {
        args
    };
    for spec in specs {
        match spec.parse::<Dice>() {
            Ok(dice) => {
                let (faces, total) = roll(&mut root, dice);
                println!("{spec}: {faces:?} = {total}");
            }
            Err(e) => eprintln!("{e}"),
        }
    }
    println!("{}", cheer(&mut root));
}

#[cfg(test)]
mod tests {
    use seam::test::Sim;

    use super::*;

    #[test]
    fn dice_parse() {
        assert_eq!(
            "3d6+2".parse(),
            Ok(Dice {
                count: 3,
                sides: 6,
                bonus: 2
            })
        );
        assert_eq!(
            "d20-1".parse(),
            Ok(Dice {
                count: 1,
                sides: 20,
                bonus: -1
            })
        );
        assert!("6".parse::<Dice>().is_err());
        assert!("0d6".parse::<Dice>().is_err());
    }

    #[test]
    fn a_seed_fixes_every_roll() {
        let dice = "4d6".parse().unwrap();
        let first = roll(&mut Sim::with_seed(3), dice);
        assert_eq!(roll(&mut Sim::with_seed(3), dice), first);
        assert!(first.0.iter().all(|f| (1..=6).contains(f)));
        assert_eq!(first.1, first.0.iter().map(|&f| i64::from(f)).sum::<i64>());
    }

    /// Over many seeds, every face of a d6 comes up.
    #[test]
    fn every_face_comes_up() {
        let mut seen = [false; 6];
        for seed in 0..200 {
            let (faces, _) = roll(&mut Sim::with_seed(seed), "1d6".parse().unwrap());
            seen[faces[0] as usize - 1] = true;
        }
        assert_eq!(seen, [true; 6]);
    }
}
