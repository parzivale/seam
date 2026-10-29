//! Logging for seam: a log record is an effect.
//!
//! A core that logs bounds on [`Perform<Log>`](Log), and its records go
//! wherever the edge sends them:
//!
//! - [`Tracing`] emits each as a tracing event, for whatever subscriber
//!   the program installed.
//! - [`Captured`] keeps them, for a test to assert on. With the `test`
//!   feature it also reads and writes a `logs` world-file block, so
//!   `check_world` can check what a core logged.
//! - [`Discard`] drops them.
//!
//! All three are sync, and async through the lifts: logging never waits, so
//! they serve sync and async cores alike, under any executor.
//! The macros [`error!`], [`warn!`], [`info!`], [`debug!`] and [`trace!`]
//! format a message and perform it, tagged with the calling module; for
//! async cores, [`log_async`] and [`log_send`].
//!
//! # In tests
//! ```
//! use seam_core::Perform;
//! use seam_tracing::{Captured, Level, Log, info, warn};
//!
//! fn retry<C: for<'a> Perform<Log<'a>>>(c: &mut C, attempt: u32) {
//!     if attempt > 2 {
//!         warn!(c, "giving up after {attempt} attempts");
//!     } else {
//!         info!(c, "retrying, attempt {attempt}");
//!     }
//! }
//!
//! let mut logs = Captured::new();
//! retry(&mut logs, 1);
//! retry(&mut logs, 3);
//! let levels: Vec<Level> = logs.records().iter().map(|r| r.level).collect();
//! assert_eq!(levels, [Level::INFO, Level::WARN]);
//! assert_eq!(logs.records()[1].message, "giving up after 3 attempts");
//! ```
//!
//! In a world file, the `logs` block (feature `test`) lists the records,
//! and `check_world` finds each one expected:
//!
//! ```text
//! logs {
//!     record "giving up after 3 attempts" level="warn"
//! }
//! ```
//!
//! # In a program
//!
//! `Tracing` is a part like any other: the program installs a subscriber
//! as usual (`tracing-subscriber`, say), and the root sends `Log` there,
//! next to whatever backends the rest of the core needs:
//!
//! ```no_run
//! use seam_core::effects as fx;
//! use seam_core::ops::time::Sleep;
//! use seam_core::prelude::*;
//! use seam_tracing::{Log, Tracing, effects as log, log_send};
//! use std::time::Duration;
//!
//! /// An async core that logs and waits.
//! async fn warm_up<C>(c: &mut C)
//! where
//!     C: for<'a> SendPerform<Log<'a>> + SendPerform<Sleep> + Send,
//! {
//!     log_send(c, seam_tracing::Level::INFO, module_path!(), "warming up").await;
//!     c.perform_send(Sleep(Duration::from_millis(100))).await;
//! }
//!
//! // The program: tracing for the logs, Tokio for the time.
//! seam_tokio::block_on(async {
//!     let mut root = seam_core::root![Tracing<log::Log>, seam_tokio::Tokio<fx::Sleep>];
//!     warm_up(&mut root).await;
//! });
//! ```
//!
//! Through the `seam` crate, this crate is `seam::tracing` (feature
//! `tracing`; with `test`, the `logs` block).
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `test` | no | `World` for [`Captured`]: the `logs` world-file block |
//!
//! # Notes
//! Logging this way is a choice: it puts logging in a core's bounds. A
//! core that would rather log as a side channel can keep using tracing's
//! own macros, which seam does not ban.
#![forbid(unsafe_code)]

use std::string::String;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::{Effect, Operation, Perform};
pub use tracing::Level;

/// Log `message`, at `level`, from `target` (by convention the module that
/// logs it: the macros pass `module_path!()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Log<'a> {
    /// How important it is.
    pub level: Level,
    /// Where it comes from.
    pub target: &'a str,
    /// What it says.
    pub message: &'a str,
}

impl<'a> Log<'a> {
    /// Describes logging `message` at `level` from `target`.
    pub const fn new(level: Level, target: &'a str, message: &'a str) -> Self {
        Self {
            level,
            target,
            message,
        }
    }
}

impl Operation for Log<'_> {
    type Outcome = ();
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    /// Routes [`Log`](crate::Log).
    #[derive(Debug, Clone, Copy)]
    pub struct Log;
}

impl Effect for Log<'_> {
    type Key = effects::Log;
}

/// Sends each record to tracing, as an event at its level, with its
/// message and, since tracing's own targets are fixed where the event is
/// written, its target as the field `origin`. Events have tracing's target
/// `seam`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tracing;

impl Perform<Log<'_>> for Tracing {
    fn perform(&mut self, op: Log<'_>) {
        let (origin, message) = (op.target, op.message);
        match op.level {
            Level::ERROR => tracing::event!(target: "seam", Level::ERROR, origin, "{message}"),
            Level::WARN => tracing::event!(target: "seam", Level::WARN, origin, "{message}"),
            Level::INFO => tracing::event!(target: "seam", Level::INFO, origin, "{message}"),
            Level::DEBUG => tracing::event!(target: "seam", Level::DEBUG, origin, "{message}"),
            _ => tracing::event!(target: "seam", Level::TRACE, origin, "{message}"),
        }
    }
}

/// Drops every record.
#[derive(Debug, Default, Clone, Copy)]
pub struct Discard;

impl Perform<Log<'_>> for Discard {
    fn perform(&mut self, _: Log<'_>) {}
}

/// A record [`Captured`] kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Its level.
    pub level: Level,
    /// Its target.
    pub target: String,
    /// Its message.
    pub message: String,
}

/// Keeps every record, in order, for a test to look at. Every clone is a
/// handle to the same records, so a part built over a clone sees them too.
#[derive(Debug, Default, Clone)]
pub struct Captured(Arc<Mutex<Vec<Record>>>);

impl Captured {
    /// No records yet.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Record>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The records so far.
    pub fn records(&self) -> Vec<Record> {
        self.lock().clone()
    }

    /// The records so far, removing them.
    pub fn take(&self) -> Vec<Record> {
        core::mem::take(&mut *self.lock())
    }

    /// The messages at `level` or more important, in order.
    pub fn messages_at(&self, level: Level) -> Vec<String> {
        self.lock()
            .iter()
            .filter(|r| r.level <= level)
            .map(|r| r.message.clone())
            .collect()
    }
}

impl Perform<Log<'_>> for Captured {
    fn perform(&mut self, op: Log<'_>) {
        self.lock().push(Record {
            level: op.level,
            target: op.target.into(),
            message: op.message.into(),
        });
    }
}

/// `logs { record "message" level="info" target="my_crate::module" }`:
/// loading adds records; written out, it lists them; checking finds each
/// expected record (an expectation may leave out `level` or `target`).
#[cfg(feature = "test")]
impl seam_test::World for Captured {
    const BLOCKS: &'static [&'static str] = &["logs"];

    fn load(&mut self, block: &seam_test::world_file::Node) -> seam_test::world_file::Result<()> {
        for child in block.block()? {
            if child.name() != "record" {
                return Err(child.unknown("`logs`", &["record"]));
            }
            child.no_children()?;
            child.props_only(&["level", "target"])?;
            child.arity(1, Some(1), "the message")?;
            let message = child.args()[0].string("the message")?.into();
            let level = match child.prop("level") {
                None => Level::INFO,
                Some(v) => {
                    let name = v.string("the level")?;
                    name.parse()
                        .map_err(|_| v.error(std::format!("`{name}` is not a level")))?
                }
            };
            let target = match child.prop("target") {
                None => String::new(),
                Some(v) => v.string("the target")?.into(),
            };
            self.lock().push(Record {
                level,
                target,
                message,
            });
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<seam_test::world_file::Node> {
        use seam_test::world_file::Node;
        let mut block = Node::new("logs");
        for r in self.lock().iter() {
            block = block.with_child(
                Node::new("record")
                    .with_arg(r.message.as_str())
                    .with_prop("level", level_name(r.level))
                    .with_prop("target", r.target.as_str()),
            );
        }
        std::vec![block]
    }
}

#[cfg(feature = "test")]
fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        _ => "trace",
    }
}

// The lifts let async cores log through them too.
impl seam_core::Capability for Tracing {}
impl seam_core::Capability for Discard {}
impl seam_core::Capability for Captured {}

/// Logs `message` at `level` from `target`: [`Log`].
pub fn log<C>(c: &mut C, level: Level, target: &str, message: &str)
where
    C: for<'a> Perform<Log<'a>> + ?Sized,
{
    c.perform(Log::new(level, target, message));
}

/// [`log()`], for async capabilities.
pub async fn log_async<C>(c: &mut C, level: Level, target: &str, message: &str)
where
    C: for<'a> seam_core::future::AsyncPerform<Log<'a>> + ?Sized,
{
    c.perform_async(Log::new(level, target, message)).await;
}

/// [`log()`], with a `Send` future: for multithreaded runtimes.
pub async fn log_send<C>(c: &mut C, level: Level, target: &str, message: &str)
where
    C: for<'a> seam_core::future::SendPerform<Log<'a>> + Send + ?Sized,
{
    c.perform_send(Log::new(level, target, message)).await;
}

/// Formats a message and logs it at `level` through `c`, from the calling
/// module: `log!(c, Level::INFO, "{} files", n)`.
#[macro_export]
macro_rules! log {
    ($c:expr, $level:expr, $($arg:tt)+) => {
        $crate::log($c, $level, ::core::module_path!(), &::std::format!($($arg)+))
    };
}

/// [`log!`] at `ERROR`.
#[macro_export]
macro_rules! error {
    ($c:expr, $($arg:tt)+) => { $crate::log!($c, $crate::Level::ERROR, $($arg)+) };
}

/// [`log!`] at `WARN`.
#[macro_export]
macro_rules! warn {
    ($c:expr, $($arg:tt)+) => { $crate::log!($c, $crate::Level::WARN, $($arg)+) };
}

/// [`log!`] at `INFO`.
#[macro_export]
macro_rules! info {
    ($c:expr, $($arg:tt)+) => { $crate::log!($c, $crate::Level::INFO, $($arg)+) };
}

/// [`log!`] at `DEBUG`.
#[macro_export]
macro_rules! debug {
    ($c:expr, $($arg:tt)+) => { $crate::log!($c, $crate::Level::DEBUG, $($arg)+) };
}

/// [`log!`] at `TRACE`.
#[macro_export]
macro_rules! trace {
    ($c:expr, $($arg:tt)+) => { $crate::log!($c, $crate::Level::TRACE, $($arg)+) };
}
