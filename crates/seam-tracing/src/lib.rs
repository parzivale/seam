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
//! # Fields and spans
//!
//! Records carry structured fields, written as tracing writes them:
//! `info!(c, user = %name, tries = 3, "logged in")`, with `%` for a value
//! recorded as it displays and `?` for one as it debugs. A [`Span`] is a
//! scope: [`info_span!`] and friends describe one, `acquire` opens it and
//! lends a handle, and what is logged on the handle is inside the span
//! (spans opened on it, inside it too). The handle, not the thread, says
//! which span a record is in, so spans stay right across `.await`s:
//!
//! ```
//! use seam_core::prelude::*;
//! use seam_tracing::{Captured, Log, Span, info, info_span};
//!
//! fn handle<C, S>(c: &mut C, user: &str)
//! where
//!     C: for<'a> Provide<Span<'a, S>>,
//!     S: for<'a> Perform<Log<'a>>,
//! {
//!     c.acquire(info_span!("request", user = user), |_, span| {
//!         info!(span, items = 3, "handled");
//!         seam_core::io::Result::Ok(())
//!     })
//!     .unwrap();
//! }
//!
//! let mut logs = Captured::new();
//! handle(&mut logs, "ada");
//! let record = &logs.records()[0];
//! assert_eq!(record.field("items"), Some("3"));
//! assert_eq!(record.spans[0].name, "request");
//! ```
//!
//! [`Tracing`] sends fields as tracing fields and spans as tracing spans,
//! with each record's own target, so subscribers filter and format them
//! as they do any other.
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

use core::fmt;
use core::marker::PhantomData;
use std::string::{String, ToString};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use seam_core::{Acquisition, Closing, Effect, Opening, Operation, Perform, io};

mod dynamic;

#[doc(hidden)]
pub mod __private {
    pub use seam_core::Perform;
}

// tracing's pure types: levels and level filters.
pub use tracing::Level;
pub use tracing::level_filters::LevelFilter;

/// A field's value, as tracing records it: a number, a flag, text, or
/// anything `Display` or `Debug`.
#[derive(Clone, Copy)]
pub enum Value<'a> {
    /// A flag.
    Bool(bool),
    /// A signed integer.
    I64(i64),
    /// An unsigned integer.
    U64(u64),
    /// A float.
    F64(f64),
    /// Text.
    Str(&'a str),
    /// Anything `Display`, recorded as it displays (`%x` in the macros).
    Display(&'a (dyn fmt::Display + Sync)),
    /// Anything `Debug`, recorded as it debugs (`?x` in the macros).
    Debug(&'a (dyn fmt::Debug + Sync)),
}

impl<'a> Value<'a> {
    /// `x`, recorded as it displays.
    pub fn display(x: &'a (dyn fmt::Display + Sync)) -> Self {
        Self::Display(x)
    }

    /// `x`, recorded as it debugs.
    pub fn debug(x: &'a (dyn fmt::Debug + Sync)) -> Self {
        Self::Debug(x)
    }
}

impl fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => fmt::Debug::fmt(b, f),
            Self::I64(n) => fmt::Debug::fmt(n, f),
            Self::U64(n) => fmt::Debug::fmt(n, f),
            Self::F64(n) => fmt::Debug::fmt(n, f),
            Self::Str(s) => fmt::Debug::fmt(s, f),
            Self::Display(d) => fmt::Display::fmt(d, f),
            Self::Debug(d) => fmt::Debug::fmt(d, f),
        }
    }
}

impl fmt::Display for Value<'_> {
    /// As a field's value is shown: text as it is, the rest as recorded.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(s) => f.write_str(s),
            other => fmt::Debug::fmt(other, f),
        }
    }
}

macro_rules! value_from {
    ($($t:ty => $v:ident $(as $as:ty)?;)*) => {$(
        impl From<$t> for Value<'_> {
            fn from(x: $t) -> Self {
                Self::$v(x $(as $as)?)
            }
        }
    )*};
}

value_from! {
    bool => Bool;
    i8 => I64 as i64; i16 => I64 as i64; i32 => I64 as i64; i64 => I64; isize => I64 as i64;
    u8 => U64 as u64; u16 => U64 as u64; u32 => U64 as u64; u64 => U64; usize => U64 as u64;
    f32 => F64 as f64; f64 => F64;
}

impl<'a> From<&'a str> for Value<'a> {
    fn from(x: &'a str) -> Self {
        Self::Str(x)
    }
}

impl<'a> From<&'a String> for Value<'a> {
    fn from(x: &'a String) -> Self {
        Self::Str(x)
    }
}

/// A named field: names are `'static`, as tracing's are.
pub type Field<'a> = (&'static str, Value<'a>);

/// Log `message`, at `level`, from `target` (by convention the module that
/// logs it: the macros pass `module_path!()`), with `fields`.
#[derive(Debug, Clone, Copy)]
pub struct Log<'a> {
    /// How important it is.
    pub level: Level,
    /// Where it comes from.
    pub target: &'static str,
    /// What it says.
    pub message: &'a str,
    /// Its structured fields.
    pub fields: &'a [Field<'a>],
}

impl<'a> Log<'a> {
    /// Describes logging `message` at `level` from `target`.
    pub const fn new(level: Level, target: &'static str, message: &'a str) -> Self {
        Self {
            level,
            target,
            message,
            fields: &[],
        }
    }

    /// With these structured fields.
    #[must_use]
    pub const fn fields(mut self, fields: &'a [Field<'a>]) -> Self {
        self.fields = fields;
        self
    }
}

impl Operation for Log<'_> {
    type Outcome = ();
}

/// A span for a scope: a named stretch of work, at `level`, from `target`,
/// with `fields`, lending a span handle of type `S`. What is logged on the
/// handle (or in spans opened on it) is inside the span; closing the scope
/// ends it.
///
/// The handle, not the thread, says which span a record is in, so spans
/// stay right across `.await`s and executor threads.
pub struct Span<'a, S> {
    /// How important it is.
    pub level: Level,
    /// Where it comes from.
    pub target: &'static str,
    /// Its name.
    pub name: &'static str,
    /// Its structured fields.
    pub fields: &'a [Field<'a>],
    handle: PhantomData<fn() -> S>,
}

impl<'a, S> Span<'a, S> {
    /// Describes a span `name` at `level` from `target`.
    pub const fn new(level: Level, target: &'static str, name: &'static str) -> Self {
        Self {
            level,
            target,
            name,
            fields: &[],
            handle: PhantomData,
        }
    }

    /// With these structured fields.
    #[must_use]
    pub const fn fields(mut self, fields: &'a [Field<'a>]) -> Self {
        self.fields = fields;
        self
    }
}

impl<S> fmt::Debug for Span<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Span")
            .field("level", &self.level)
            .field("target", &self.target)
            .field("name", &self.name)
            .field("fields", &self.fields)
            .finish()
    }
}

impl<S> Acquisition for Span<'_, S> {
    type Resource = S;
    type Error = io::Error;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    /// Routes [`Log`](crate::Log).
    #[derive(Debug, Clone, Copy)]
    pub struct Log;

    /// Routes [`Span`](crate::Span).
    #[derive(Debug, Clone, Copy)]
    pub struct Span;
}

impl Effect for Log<'_> {
    type Key = effects::Log;
}

impl<S> Effect for Span<'_, S> {
    type Key = effects::Span;
}

/// Sends each record to tracing, as an event at its level and target, with
/// its message and fields, and each span as a tracing span; the program's
/// subscriber does the rest. Events and spans opened here (not on a span
/// handle) are in tracing's current span.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tracing;

/// A span from [`Tracing`]: what is logged on it is inside it.
#[derive(Debug, Clone)]
pub struct TracingSpan(tracing::Span);

impl TracingSpan {
    /// The tracing span, for code that takes one (`Instrument`, say).
    pub fn span(&self) -> &tracing::Span {
        &self.0
    }
}

impl Perform<Log<'_>> for Tracing {
    fn perform(&mut self, op: Log<'_>) {
        dynamic::event(None, &op);
    }
}

impl Perform<Log<'_>> for TracingSpan {
    fn perform(&mut self, op: Log<'_>) {
        dynamic::event(Some(&self.0), &op);
    }
}

impl Perform<Opening<Span<'_, TracingSpan>>> for Tracing {
    fn perform(
        &mut self,
        Opening(op, _): Opening<Span<'_, TracingSpan>>,
    ) -> io::Result<TracingSpan> {
        Ok(TracingSpan(dynamic::span(
            None, op.level, op.target, op.name, op.fields,
        )))
    }
}

impl Perform<Opening<Span<'_, TracingSpan>>> for TracingSpan {
    fn perform(
        &mut self,
        Opening(op, _): Opening<Span<'_, TracingSpan>>,
    ) -> io::Result<TracingSpan> {
        Ok(TracingSpan(dynamic::span(
            Some(&self.0),
            op.level,
            op.target,
            op.name,
            op.fields,
        )))
    }
}

/// Drops every record, and every span.
#[derive(Debug, Default, Clone, Copy)]
pub struct Discard;

impl Perform<Log<'_>> for Discard {
    fn perform(&mut self, _: Log<'_>) {}
}

impl Perform<Opening<Span<'_, Discard>>> for Discard {
    fn perform(&mut self, _: Opening<Span<'_, Discard>>) -> io::Result<Discard> {
        Ok(Discard)
    }
}

/// A span [`Captured`] kept: its name and fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanRecord {
    /// Its name.
    pub name: String,
    /// Its fields, each value as [`Value`] displays it.
    pub fields: Vec<(String, String)>,
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
    /// Its fields, each value as [`Value`] displays it.
    pub fields: Vec<(String, String)>,
    /// The spans it is in, outermost first.
    pub spans: Vec<SpanRecord>,
}

impl Record {
    /// The value of the field `name`, if it has one.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn captured_fields(fields: &[Field<'_>]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.to_string()))
        .collect()
}

/// Keeps every record, in order, for a test to look at. Every clone is a
/// handle to the same records, so a part built over a clone sees them too.
#[derive(Debug, Default, Clone)]
pub struct Captured(Arc<Mutex<Vec<Record>>>);

/// A span from [`Captured`]: records logged on it are kept with it (and
/// the spans around it) in their [`spans`](Record::spans).
#[derive(Debug, Clone)]
pub struct CapturedSpan {
    records: Captured,
    spans: Vec<SpanRecord>,
}

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

    fn keep(&self, op: &Log<'_>, spans: &[SpanRecord]) {
        self.lock().push(Record {
            level: op.level,
            target: op.target.into(),
            message: op.message.into(),
            fields: captured_fields(op.fields),
            spans: spans.to_vec(),
        });
    }
}

impl Perform<Log<'_>> for Captured {
    fn perform(&mut self, op: Log<'_>) {
        self.keep(&op, &[]);
    }
}

impl Perform<Log<'_>> for CapturedSpan {
    fn perform(&mut self, op: Log<'_>) {
        self.records.keep(&op, &self.spans);
    }
}

fn span_record(op: &Span<'_, CapturedSpan>) -> SpanRecord {
    SpanRecord {
        name: op.name.into(),
        fields: captured_fields(op.fields),
    }
}

impl Perform<Opening<Span<'_, CapturedSpan>>> for Captured {
    fn perform(
        &mut self,
        Opening(op, _): Opening<Span<'_, CapturedSpan>>,
    ) -> io::Result<CapturedSpan> {
        Ok(CapturedSpan {
            records: self.clone(),
            spans: std::vec![span_record(&op)],
        })
    }
}

impl Perform<Opening<Span<'_, CapturedSpan>>> for CapturedSpan {
    fn perform(
        &mut self,
        Opening(op, _): Opening<Span<'_, CapturedSpan>>,
    ) -> io::Result<CapturedSpan> {
        let mut spans = self.spans.clone();
        spans.push(span_record(&op));
        Ok(CapturedSpan {
            records: self.records.clone(),
            spans,
        })
    }
}

/// Closing a span's scope ends it.
macro_rules! span_closing {
    ($($handler:ty => $handle:ty;)*) => {$(
        impl Perform<Closing<Span<'_, $handle>>> for $handler {
            fn perform(&mut self, _: Closing<Span<'_, $handle>>) -> io::Result<()> {
                Ok(())
            }
        }
    )*};
}

span_closing! {
    Tracing => TracingSpan;
    TracingSpan => TracingSpan;
    Discard => Discard;
    Captured => CapturedSpan;
    CapturedSpan => CapturedSpan;
}

/// `logs { record "message" level="info" target="my_crate::module" { field
/// "user" "ada"; span "request" } }`: loading adds records; written out, it
/// lists them; checking finds each expected record (an expectation may
/// leave out `level`, `target`, fields and spans).
#[cfg(feature = "test")]
impl seam_test::World for Captured {
    const BLOCKS: &'static [&'static str] = &["logs"];

    fn load(&mut self, block: &seam_test::world_file::Node) -> seam_test::world_file::Result<()> {
        for child in block.block()? {
            if child.name() != "record" {
                return Err(child.unknown("`logs`", &["record"]));
            }
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
            let mut fields = Vec::new();
            let mut spans = Vec::new();
            for part in child.children() {
                match part.name() {
                    "field" => {
                        part.no_children()?;
                        part.arity(2, Some(2), "a name and a value")?;
                        fields.push((
                            part.args()[0].string("the name")?.into(),
                            part.args()[1].string("the value")?.into(),
                        ));
                    }
                    "span" => {
                        part.no_children()?;
                        part.arity(1, Some(1), "the span's name")?;
                        spans.push(SpanRecord {
                            name: part.args()[0].string("the span's name")?.into(),
                            fields: Vec::new(),
                        });
                    }
                    _ => return Err(part.unknown("`record`", &["field", "span"])),
                }
            }
            self.lock().push(Record {
                level,
                target,
                message,
                fields,
                spans,
            });
        }
        Ok(())
    }

    fn to_world(&self) -> Vec<seam_test::world_file::Node> {
        use seam_test::world_file::Node;
        let mut block = Node::new("logs");
        for r in self.lock().iter() {
            let mut node = Node::new("record")
                .with_arg(r.message.as_str())
                .with_prop("level", level_name(r.level))
                .with_prop("target", r.target.as_str());
            for (name, value) in &r.fields {
                node = node.with_child(
                    Node::new("field")
                        .with_arg(name.as_str())
                        .with_arg(value.as_str()),
                );
            }
            for span in &r.spans {
                node = node.with_child(Node::new("span").with_arg(span.name.as_str()));
            }
            block = block.with_child(node);
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

// The lifts let async cores log and open spans through them too.
impl seam_core::Capability for Tracing {}
impl seam_core::Capability for TracingSpan {}
impl seam_core::Capability for Discard {}
impl seam_core::Capability for Captured {}
impl seam_core::Capability for CapturedSpan {}

/// Logs `message` at `level` from `target`: [`Log`].
pub fn log<C>(c: &mut C, level: Level, target: &'static str, message: &str)
where
    C: for<'a> Perform<Log<'a>> + ?Sized,
{
    c.perform(Log::new(level, target, message));
}

/// [`log()`], for async capabilities.
pub async fn log_async<C>(c: &mut C, level: Level, target: &'static str, message: &str)
where
    C: for<'a> seam_core::future::AsyncPerform<Log<'a>> + ?Sized,
{
    c.perform_async(Log::new(level, target, message)).await;
}

/// [`log()`], with a `Send` future: for multithreaded runtimes.
pub async fn log_send<C>(c: &mut C, level: Level, target: &'static str, message: &str)
where
    C: for<'a> seam_core::future::SendPerform<Log<'a>> + Send + ?Sized,
{
    c.perform_send(Log::new(level, target, message)).await;
}

/// Formats a message and logs it at `level` through `c`, from the calling
/// module, with any fields before it, as tracing writes them:
/// `log!(c, Level::INFO, user = %name, tries = 3, "{} files", n)`. A
/// field is `name = value` (a number, flag or text), `name = %value`
/// (recorded as it displays) or `name = ?value` (as it debugs).
#[macro_export]
macro_rules! log {
    ($c:expr, $level:expr, $($rest:tt)+) => {
        $crate::__log!($c, $level, [] $($rest)+)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __log {
    ($c:expr, $level:expr, [$($f:tt)*] $name:ident = % $v:expr, $($rest:tt)+) => {
        $crate::__log!($c, $level, [$($f)* (::core::stringify!($name), $crate::Value::display(&$v)),] $($rest)+)
    };
    ($c:expr, $level:expr, [$($f:tt)*] $name:ident = ? $v:expr, $($rest:tt)+) => {
        $crate::__log!($c, $level, [$($f)* (::core::stringify!($name), $crate::Value::debug(&$v)),] $($rest)+)
    };
    ($c:expr, $level:expr, [$($f:tt)*] $name:ident = $v:expr, $($rest:tt)+) => {
        $crate::__log!($c, $level, [$($f)* (::core::stringify!($name), $crate::Value::from($v)),] $($rest)+)
    };
    ($c:expr, $level:expr, [$($f:tt)*] $($fmt:tt)+) => {
        $crate::__private::Perform::perform(
            $c,
            $crate::Log::new($level, ::core::module_path!(), &::std::format!($($fmt)+))
                .fields(&[$($f)*]),
        )
    };
}

/// A [`Span`] at `level`, from the calling module, with any fields after
/// its name, as tracing writes them: `span!(Level::INFO, "request", id = 7)`.
/// Open it with `acquire`, and log on the handle it lends.
#[macro_export]
macro_rules! span {
    ($level:expr, $name:expr $(,)?) => {
        $crate::Span::new($level, ::core::module_path!(), $name)
    };
    ($level:expr, $name:expr, $($rest:tt)+) => {
        $crate::__span!($level, $name, [] $($rest)+,)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __span {
    ($level:expr, $name:expr, [$($f:tt)*] $field:ident = % $v:expr, $($rest:tt)*) => {
        $crate::__span!($level, $name, [$($f)* (::core::stringify!($field), $crate::Value::display(&$v)),] $($rest)*)
    };
    ($level:expr, $name:expr, [$($f:tt)*] $field:ident = ? $v:expr, $($rest:tt)*) => {
        $crate::__span!($level, $name, [$($f)* (::core::stringify!($field), $crate::Value::debug(&$v)),] $($rest)*)
    };
    ($level:expr, $name:expr, [$($f:tt)*] $field:ident = $v:expr, $($rest:tt)*) => {
        $crate::__span!($level, $name, [$($f)* (::core::stringify!($field), $crate::Value::from($v)),] $($rest)*)
    };
    ($level:expr, $name:expr, [$($f:tt)*] $(,)*) => {
        $crate::Span::new($level, ::core::module_path!(), $name).fields(&[$($f)*])
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

/// [`span!`] at `ERROR`.
#[macro_export]
macro_rules! error_span {
    ($($arg:tt)+) => { $crate::span!($crate::Level::ERROR, $($arg)+) };
}

/// [`span!`] at `WARN`.
#[macro_export]
macro_rules! warn_span {
    ($($arg:tt)+) => { $crate::span!($crate::Level::WARN, $($arg)+) };
}

/// [`span!`] at `INFO`.
#[macro_export]
macro_rules! info_span {
    ($($arg:tt)+) => { $crate::span!($crate::Level::INFO, $($arg)+) };
}

/// [`span!`] at `DEBUG`.
#[macro_export]
macro_rules! debug_span {
    ($($arg:tt)+) => { $crate::span!($crate::Level::DEBUG, $($arg)+) };
}

/// [`span!`] at `TRACE`.
#[macro_export]
macro_rules! trace_span {
    ($($arg:tt)+) => { $crate::span!($crate::Level::TRACE, $($arg)+) };
}
