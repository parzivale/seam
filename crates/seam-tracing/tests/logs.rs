//! Records, captured, sent to tracing, and checked in world files.

use std::sync::{Arc, Mutex};

use seam_core::Perform;
use seam_core::effects as fx;
use seam_tracing::{Captured, Discard, Level, Log, Tracing, debug, error, info};

fn work<C: for<'a> Perform<Log<'a>>>(c: &mut C) {
    info!(c, "starting {} jobs", 2);
    debug!(c, "job details");
    error!(c, "job {} failed", 1);
}

#[test]
fn captured_records() {
    let mut logs = Captured::new();
    work(&mut logs);
    let records = logs.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].message, "starting 2 jobs");
    assert_eq!(records[0].target, module_path!());
    assert_eq!(
        logs.messages_at(Level::INFO),
        ["starting 2 jobs", "job 1 failed"]
    );
    assert_eq!(logs.take().len(), 3);
    assert!(logs.records().is_empty());
    work(&mut Discard);
}

#[test]
fn in_a_root_beside_the_simulation() {
    let logs = Captured::new();
    let mut root = seam_core::root![
        seam_test::Sim<fx::Now>::new(),
        Captured<seam_tracing::effects::Log>::clone(&logs),
    ];
    work(&mut root);
    seam_core::future::block_on(seam_tracing::log_send(
        &mut root,
        Level::WARN,
        "here",
        "async",
    ));
    assert_eq!(logs.records().len(), 4);
}

#[cfg(feature = "test")]
#[test]
fn world_files_check_what_was_logged() {
    let mut root = seam_core::root![Captured<seam_tracing::effects::Log>::new()];
    work(&mut root);
    seam_test::check_world::<(seam_tracing::effects::Log,), _>(
        &root,
        r#"logs { record "job 1 failed" level="error" }"#,
    )
    .unwrap();
    let missing = seam_test::check_world::<(seam_tracing::effects::Log,), _>(
        &root,
        r#"logs { record "never said" }"#,
    );
    assert!(missing.is_err());

    // Written out, it loads back the same.
    let text = seam_test::to_world::<(seam_tracing::effects::Log,), _>(&root);
    let mut again = seam_core::root![Captured<seam_tracing::effects::Log>::new()];
    seam_test::load_world::<(seam_tracing::effects::Log,), _>(&mut again, &text).unwrap();
    assert_eq!(
        again.part::<seam_tracing::effects::Log>().records(),
        root.part::<seam_tracing::effects::Log>().records()
    );
}

/// A subscriber that keeps each event's level and message.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<(Level, String)>>>);

impl tracing::Subscriber for Events {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), message.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn sent_to_tracing() {
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || work(&mut Tracing));
    let got = events.0.lock().unwrap().clone();
    assert_eq!(
        got,
        [
            (Level::INFO, "starting 2 jobs".to_owned()),
            (Level::DEBUG, "job details".to_owned()),
            (Level::ERROR, "job 1 failed".to_owned()),
        ]
    );
}
