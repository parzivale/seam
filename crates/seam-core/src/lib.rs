//! Sans-effect capability traits.
//!
//! Every effect (I/O, time, randomness, shared state) is an [`Operation`]
//! type. Code that needs an effect asks for the capability to run it, as a
//! trait bound:
//!
//! - [`Inspect<Q>`] runs a side-effect-free [`Query`] through `&self`.
//! - [`Perform<O>`] runs any operation through `&mut self`.
//!
//! Anything that can inspect a query can also perform it, so a stronger
//! capability is accepted wherever a weaker one is required.
//!
//! The core of a program is written against these bounds and never touches
//! the outside world directly. Real implementations (`seam-std`,
//! `seam-tokio`) or test fakes are plugged in at the edge.
//!
//! Mutation through `&mut` is not an effect: the borrow checker already makes
//! it visible. Control effects (errors, early return, suspension) use
//! `Result` and futures as usual.
//!
//! This crate is `no_std`. Operation types whose outcomes need std types
//! (`io::Error`, `Path`, `Instant`, ...) are behind the `std` feature, which
//! the capability features that need it turn on.
#![no_std]

#[cfg(feature = "std")]
extern crate std;

mod capability;
mod finalize;
pub mod future;
mod operation;
pub mod ops;
mod push;

pub use capability::{Inspect, Perform};
pub use finalize::{Finalize, Scoped};
pub use operation::{Operation, Query};
pub use push::{Push, Rejected};
