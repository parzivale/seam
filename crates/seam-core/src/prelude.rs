//! The traits you need to write and run a core, in one import.
//!
//! ```
//! use seam_core::prelude::*;
//! ```
//!
//! This brings in the capability traits ([`Inspect`], [`Perform`] and their
//! async and `Send` forms), the traits for defining operations
//! ([`Operation`], [`Query`]), resource cleanup ([`Finalize`], [`Scoped`]),
//! the [`Push`] sink operation, and [`block_on`].
//!
//! Operation types are not included: import the ones a core uses from
//! [`ops`](crate::ops), so that its imports, like its bounds, list what it
//! can do.

pub use crate::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform, block_on};
pub use crate::{Finalize, Inspect, Operation, Perform, Push, Query, Rejected, Scoped};
