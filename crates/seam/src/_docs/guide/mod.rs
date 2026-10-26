//! Usage guides, in the order you are likely to need them.
//!
//! 1. [`getting_started`]: a first core, run on the real world and tested
//!    on a simulated one.
//! 2. [`roots`]: combining backends and your own parts into one
//!    capability.
//! 3. [`resources`]: files, sockets, tasks and other things a core uses for
//!    the length of a scope.
//! 4. [`async_and_concurrency`]: async cores, `Send` cores, tasks and
//!    threads, and testing them deterministically.
//! 5. [`your_own_effects`]: effects of a program's own, their real and
//!    simulated handlers, and world-file blocks for them.
//! 6. [`testing`]: the simulated world in detail.

#[doc = include_str!("getting_started.md")]
pub mod getting_started {}

#[doc = include_str!("roots.md")]
pub mod roots {}

#[doc = include_str!("resources.md")]
pub mod resources {}

#[doc = include_str!("async_and_concurrency.md")]
pub mod async_and_concurrency {}

#[doc = include_str!("your_own_effects.md")]
pub mod your_own_effects {}

#[doc = include_str!("testing.md")]
pub mod testing {}
