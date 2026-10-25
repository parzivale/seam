//! The traits you need to write and run a core, in one import.
//!
//! ```
//! use seam_core::prelude::*;
//! ```
//!
//! This brings in the capability traits ([`Inspect`], [`Perform`] and their
//! async and `Send` forms), the traits for defining operations
//! ([`Operation`], [`Query`]), resources ([`Provide`] and [`Lend`], and their async forms),
//! the [`Push`] sink operation, and [`block_on`].
//!
//! Operation types are not included: import the ones a core uses from
//! [`ops`](crate::ops), so that its imports, like its bounds, list what it
//! can do.

pub use crate::future::{AsyncInspect, AsyncPerform, SendInspect, SendPerform, block_on};
pub use crate::{
    Acquisition, AsyncLend, AsyncProvide, Capability, Effect, Inspect, Lend, Operation, Perform,
    Provide, Push, Query, Rejected, Root, SendLend, SendProvide, SendScope,
};
