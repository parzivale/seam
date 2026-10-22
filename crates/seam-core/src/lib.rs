//! Sans-effect capability traits.
//!
//! Every effect (I/O, time, randomness, shared state) is described by an
//! [`Operation`] type. Code that needs an effect asks for the capability to
//! run it, as a trait bound:
//!
//! - [`Inspect<Q>`] runs a side-effect-free [`Query`] through `&self`.
//! - [`Perform<O>`] runs any operation through `&mut self`.
//!
//! Anything that can inspect a query can also perform it, so a stronger
//! capability is accepted wherever a weaker one is required, the way
//! `&mut T` reborrows as `&T`.
//!
//! The core of a program is written against these bounds and never touches
//! the outside world directly. Real implementations (`seam-std`,
//! `seam-tokio`) or a simulation (`seam-test`) are plugged in at the edge.
//!
//! # A whole program in miniature
//!
//! Define the operations, write the core against them, then give it a real
//! implementation in production and a fake in tests:
//!
//! ```
//! use seam_core::prelude::*;
//!
//! // 1. Operations: one type per effect, each naming its outcome.
//! struct Load;
//! impl Operation for Load {
//!     type Outcome = Option<u64>;
//! }
//! impl Query for Load {} // read-only
//!
//! struct Store(u64);
//! impl Operation for Store {
//!     type Outcome = ();
//! }
//!
//! // 2. The core: pure logic. Its bounds say it can load and store a
//! //    counter, and nothing else.
//! fn bump<C: Perform<Load> + Perform<Store>>(c: &mut C) -> u64 {
//!     let next = c.perform(Load).unwrap_or(0) + 1;
//!     c.perform(Store(next));
//!     next
//! }
//!
//! // 3. An implementation. In production this might be a file or a
//! //    database; here it is a test fake.
//! #[derive(Default)]
//! struct Memory(Option<u64>);
//! impl Inspect<Load> for Memory {
//!     fn inspect(&self, _: Load) -> Option<u64> {
//!         self.0
//!     }
//! }
//! impl Perform<Store> for Memory {
//!     fn perform(&mut self, op: Store) {
//!         self.0 = Some(op.0);
//!     }
//! }
//!
//! let mut store = Memory::default();
//! assert_eq!(bump(&mut store), 1);
//! assert_eq!(bump(&mut store), 2);
//! ```
//!
//! `Memory` implements `Inspect<Load>`, not `Perform<Load>`, yet `bump`
//! accepts it: the blanket impl lifts every inspect to a perform.
//!
//! # What is and is not an effect
//!
//! An effect is anything that escapes unique ownership: I/O, clocks,
//! randomness, shared or interior-mutable state. Mutation through `&mut` is
//! not an effect: the borrow checker already makes it visible in signatures.
//! Control effects (errors, early return, suspension) use `Result` and
//! futures as usual.
//!
//! # Where to go next
//!
//! - [`ops`]: ready-made operation types for files, sockets, clocks,
//!   randomness, the environment, processes, stdio and signals, behind
//!   feature flags.
//! - [`future`]: async capabilities, and [`block_on`](future::block_on).
//! - [`finalize`]: resources that commit on success and roll back on error,
//!   panic or cancellation ([`Scoped`], [`AsyncScoped`]).
//! - [`Push`]: a sink operation for counting or capping what a core emits.
//! - [`prelude`]: all the traits in one import.
//!
//! # Features
//!
//! This crate is `no_std` with no dependencies. Each operation group is a
//! feature: `time`, `random` (no std needed), `fs`, `net`, `process`, `env`,
//! `io-std`, `signal` (these need std's types and turn on `std`), and `full`.
//! With `std`, [`block_on`](future::block_on) parks the thread instead of
//! spinning.
#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod capability;
pub mod finalize;
pub mod future;
mod operation;
pub mod ops;
pub mod prelude;
mod push;

pub use capability::{Inspect, Perform};
pub use finalize::{AsyncFinalize, AsyncScoped, Finalize, Scoped};
pub use operation::{Operation, Query};
pub use push::{Push, Rejected};
