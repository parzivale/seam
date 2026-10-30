//! tracing events and spans whose shape is known only when they happen.
//!
//! tracing needs `'static` metadata for every event and span: a callsite,
//! normally made by its macros where the event is written. A record here
//! is written in a core, and reaches tracing through [`Tracing`](crate::Tracing),
//! so its callsite is made here instead: one per distinct level, target,
//! name and set of field names, made on first use, registered with
//! tracing and kept for the program's life. Field names are `'static` and
//! targets are module paths, so there are as many as there are places
//! that log.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::vec::Vec;

use tracing::callsite::{Callsite, Identifier};
use tracing::field::{FieldSet, Value as TracingValue};
use tracing::metadata::Kind;
use tracing::subscriber::Interest;
use tracing::{Level, Metadata};

use crate::{Field, Log, Value};

/// A callsite made at run time.
struct Dynamic {
    meta: OnceLock<Metadata<'static>>,
}

impl Callsite for Dynamic {
    fn set_interest(&self, _: Interest) {}

    fn metadata(&self) -> &Metadata<'_> {
        self.meta.get().expect("set when made")
    }
}

#[derive(PartialEq, Eq, Hash)]
struct Key {
    span: bool,
    level: Level,
    target: &'static str,
    name: &'static str,
    fields: Vec<&'static str>,
}

/// The metadata for this shape of event or span, made once.
fn metadata(key: Key) -> &'static Metadata<'static> {
    static MADE: OnceLock<Mutex<HashMap<Key, &'static Dynamic>>> = OnceLock::new();
    let mut made = MADE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(site) = made.get(&key) {
        return site.meta.get().expect("set when made");
    }
    let site: &'static Dynamic = Box::leak(Box::new(Dynamic {
        meta: OnceLock::new(),
    }));
    let names: &'static [&'static str] = Box::leak(key.fields.clone().into_boxed_slice());
    let kind = if key.span { Kind::SPAN } else { Kind::EVENT };
    let meta = Metadata::new(
        key.name,
        key.target,
        key.level,
        None,
        None,
        Some(key.target),
        FieldSet::new(names, Identifier(site)),
        kind,
    );
    let _ = site.meta.set(meta);
    tracing::callsite::register(site);
    made.insert(key, site);
    site.meta.get().expect("just set")
}

/// A field's value, in a form tracing records.
enum Held<'a> {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(&'a str),
    Display(tracing::field::DisplayValue<&'a (dyn fmt::Display + Sync)>),
    Debug(tracing::field::DebugValue<&'a (dyn fmt::Debug + Sync)>),
}

impl<'a> Held<'a> {
    fn of(v: &Value<'a>) -> Self {
        match *v {
            Value::Bool(b) => Self::Bool(b),
            Value::I64(n) => Self::I64(n),
            Value::U64(n) => Self::U64(n),
            Value::F64(n) => Self::F64(n),
            Value::Str(s) => Self::Str(s),
            Value::Display(d) => Self::Display(tracing::field::display(d)),
            Value::Debug(d) => Self::Debug(tracing::field::debug(d)),
        }
    }

    fn value(&self) -> &dyn TracingValue {
        match self {
            Self::Bool(b) => b,
            Self::I64(n) => n,
            Self::U64(n) => n,
            Self::F64(n) => n,
            Self::Str(s) => s,
            Self::Display(d) => d,
            Self::Debug(d) => d,
        }
    }
}

/// Calls `f` with the values of `meta`'s fields, from `held`, in order, as
/// the fixed-size array tracing's `ValueSet` takes (at most 32).
macro_rules! with_values {
    ($meta:expr, $held:expr, |$values:ident| $body:expr, $($n:literal)*) => {{
        let fields: Vec<_> = $meta.fields().iter().collect();
        let held: &[&dyn TracingValue] = $held;
        match held.len() {
            $($n => {
                let pairs: [(&tracing::field::Field, Option<&dyn TracingValue>); $n] =
                    core::array::from_fn(|i| (&fields[i], Some(held[i])));
                let $values = $meta.fields().value_set(&pairs);
                $body
            })*
            _ => unreachable!("at most 32 fields"),
        }
    }};
}

/// The fields of a record or span: the message (for an event), then its
/// fields, at most 32 in all (the rest are left out).
fn names(message: bool, fields: &[Field<'_>]) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    if message {
        names.push("message");
    }
    names.extend(fields.iter().map(|(n, _)| *n));
    names.truncate(32);
    names
}

/// Emits `op` as a tracing event, inside `parent` if there is one, else
/// tracing's current span.
pub(crate) fn event(parent: Option<&tracing::Span>, op: &Log<'_>) {
    let meta = metadata(Key {
        span: false,
        level: op.level,
        target: op.target,
        name: "event",
        fields: names(true, op.fields),
    });
    if !tracing::dispatcher::get_default(|d| d.enabled(meta)) {
        return;
    }
    let message = tracing::field::display(op.message);
    let held: Vec<Held<'_>> = op.fields.iter().map(|(_, v)| Held::of(v)).collect();
    let mut values: Vec<&dyn TracingValue> = Vec::with_capacity(held.len() + 1);
    values.push(&message);
    values.extend(held.iter().map(Held::value));
    values.truncate(32);
    with_values!(meta, &values, |set| match parent {
        Some(span) => tracing::Event::child_of(span.id(), meta, &set),
        None => tracing::Event::dispatch(meta, &set),
    }, 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32)
}

/// Opens a tracing span, inside `parent` if there is one, else tracing's
/// current span.
pub(crate) fn span(
    parent: Option<&tracing::Span>,
    level: Level,
    target: &'static str,
    name: &'static str,
    fields: &[Field<'_>],
) -> tracing::Span {
    let meta = metadata(Key {
        span: true,
        level,
        target,
        name,
        fields: names(false, fields),
    });
    if !tracing::dispatcher::get_default(|d| d.enabled(meta)) {
        return tracing::Span::none();
    }
    let held: Vec<Held<'_>> = fields.iter().map(|(_, v)| Held::of(v)).collect();
    let mut values: Vec<&dyn TracingValue> = held.iter().map(Held::value).collect();
    values.truncate(32);
    with_values!(meta, &values, |set| match parent {
        Some(p) => tracing::Span::child_of(p, meta, &set),
        None => tracing::Span::new(meta, &set),
    }, 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32)
}
