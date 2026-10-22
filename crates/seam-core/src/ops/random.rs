//! Randomness.

/// Produce a random `u64`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomU64;

/// Fill the buffer with random bytes.
#[derive(Debug)]
pub struct FillRandom<'a>(pub &'a mut [u8]);

operations! {
    RandomU64 => u64;
    FillRandom<'_> => ();
}
