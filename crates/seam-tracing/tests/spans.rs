//! Structured fields and spans: captured, and sent to tracing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use seam_core::prelude::*;
use seam_tracing::{Captured, Level, Log, Span, Tracing, info, info_span, warn};

/// The core: handles a request in a span, logging with fields inside it.
fn handle<C, S>(c: &mut C, user: &str, attempts: u32)
where
    C: for<'a> Provide<Span<'a, S>>,
    S: for<'a> Perform<Log<'a>> + for<'a> Provide<Span<'a, S>>,
{
    c.acquire(info_span!("request", user = user, id = 7), |_, span| {
        info!(span, attempts = attempts, path = %"/index", "handling {}", user);
        span.acquire(info_span!("db"), |_, db| {
            warn!(db, rows = ?[1, 2], "slow query");
            seam_core::io::Result::Ok(())
        })
    })
    .unwrap();
}

#[test]
fn captured_fields_and_spans() {
    let mut logs = Captured::new();
    handle(&mut logs, "ada", 2);
    let records = logs.records();
    assert_eq!(records.len(), 2);
    let first = &records[0];
    assert_eq!(first.message, "handling ada");
    assert_eq!(first.field("attempts"), Some("2"));
    assert_eq!(first.field("path"), Some("/index"));
    assert_eq!(first.spans.len(), 1);
    assert_eq!(first.spans[0].name, "request");
    assert_eq!(
        first.spans[0].fields,
        [("user".into(), "ada".into()), ("id".into(), "7".into())]
    );
    let second = &records[1];
    assert_eq!(second.level, Level::WARN);
    assert_eq!(second.field("rows"), Some("[1, 2]"));
    let names: Vec<_> = second.spans.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["request", "db"]);
}

#[cfg(feature = "test")]
#[test]
fn world_files_check_fields_and_spans() {
    use seam_test::world_file::check;
    let mut logs = Captured::new();
    handle(&mut logs, "ada", 2);
    check(
        &logs,
        r#"logs { record "slow query" level="warn" { span "db"; field "rows" "[1, 2]" } }"#,
    )
    .unwrap();
    assert!(
        check(
            &logs,
            r#"logs { record "slow query" { field "rows" "[3]" } }"#
        )
        .is_err()
    );
}

type Pairs = Vec<(String, String)>;

/// What a subscriber saw: spans by id (name, parent, fields), and events
/// (level, target, parent, fields).
#[derive(Default)]
struct Seen {
    spans: Vec<(String, Option<u64>, Pairs)>,
    events: Vec<(Level, String, Option<u64>, Pairs)>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Seen>>, Arc<AtomicU64>);

struct Fields(Vec<(String, String)>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().into(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().into(), value.into()));
    }
}

impl tracing::Subscriber for Recorder {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let id = self.1.fetch_add(1, Ordering::SeqCst) + 1;
        let mut fields = Fields(Vec::new());
        span.record(&mut fields);
        self.0.lock().unwrap().spans.push((
            span.metadata().name().into(),
            span.parent().map(|p| p.into_u64()),
            fields.0,
        ));
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(Vec::new());
        event.record(&mut fields);
        self.0.lock().unwrap().events.push((
            *event.metadata().level(),
            event.metadata().target().into(),
            event.parent().map(|p| p.into_u64()),
            fields.0,
        ));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn sent_to_tracing_with_fields_and_parents() {
    let recorder = Recorder::default();
    tracing::subscriber::with_default(recorder.clone(), || {
        // Twice: the second time the callsites are reused.
        handle(&mut Tracing, "ada", 2);
        handle(&mut Tracing, "bob", 3);
    });
    let seen = recorder.0.lock().unwrap();
    assert_eq!(seen.spans.len(), 4);
    let (name, parent, fields) = &seen.spans[0];
    assert_eq!((name.as_str(), *parent), ("request", None));
    assert_eq!(
        fields,
        &[("user".into(), "ada".into()), ("id".into(), "7".into())]
    );
    // The db span's parent is the request span (id 1).
    assert_eq!((seen.spans[1].0.as_str(), seen.spans[1].1), ("db", Some(1)));
    let (level, target, parent, fields) = &seen.events[0];
    assert_eq!(*level, Level::INFO);
    assert_eq!(target, module_path!());
    assert_eq!(*parent, Some(1));
    assert_eq!(
        fields,
        &[
            ("message".into(), "handling ada".into()),
            ("attempts".into(), "2".into()),
            ("path".into(), "/index".into()),
        ]
    );
    assert_eq!(seen.events[1].2, Some(2));
    assert_eq!(seen.events[2].3[1], ("attempts".into(), "3".into()));
}
