//! Guides and background: what seam is, how to use it, the world-file
//! format, and how it works inside.
//!
//! This module holds no code; each page is documentation only, and every
//! example in it is compiled and run as a test.
//!
//! | Page | For |
//! |---|---|
//! | [`what_is_seam`] | what problem seam solves, what it is in effect-system terms, and how it compares to other approaches |
//! | [`guide`] | usage guides, from a first core to effects and simulated parts of your own |
//! | [`world_files`] | the world-file format: every directive and expectation, and how parts add blocks of their own |
//! | [`implementation`] | how seam works inside: the traits, the lifts, scopes, roots, the handlers and the simulation |
//!
//! Start with [`what_is_seam`] for the idea, then
//! [`guide::getting_started`] for the code.

#[doc = include_str!("what_is_seam.md")]
pub mod what_is_seam {}

pub mod guide;

#[doc = include_str!("world_files.md")]
pub mod world_files {}

#[doc = include_str!("implementation.md")]
pub mod implementation {}
