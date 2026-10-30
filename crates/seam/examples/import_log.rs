//! Logging as an effect (`seam::tracing`): an import that logs in spans,
//! with structured fields.
//!
//! The core logs through `Log` and opens `Span`s, so the program sends
//! them to tracing (and a `tracing-subscriber` prints them), while the
//! tests capture them and check what was said, where.
//!
//! ```text
//! cargo run -p seam-rs --example import_log --features std,test,tracing
//! ```

use seam::prelude::*;
use seam::tracing::{Log, Span, info, info_span, warn};

/// Imports `name = amount` rows, skipping bad ones. Returns the total.
fn import<C, S>(c: &mut C, batch: &str, rows: &[&str]) -> i64
where
    C: for<'a> Provide<Span<'a, S>>,
    S: for<'a> Perform<Log<'a>>,
{
    c.acquire(
        info_span!("import", batch = batch, rows = rows.len() as u64),
        |_, span| {
            let mut total = 0;
            let mut skipped = 0u64;
            for (line, row) in rows.iter().enumerate() {
                match row
                    .split_once('=')
                    .map(|(k, v)| (k.trim(), v.trim().parse::<i64>()))
                {
                    Some((name, Ok(amount))) => total += amount_for(span, name, amount),
                    _ => {
                        skipped += 1;
                        warn!(span, line = line as u64 + 1, row = %row, "skipping a bad row");
                    }
                }
            }
            info!(span, total = total, skipped = skipped, "imported");
            seam::io::Result::Ok(total)
        },
    )
    .expect("the span opens")
}

fn amount_for<S: for<'a> Perform<Log<'a>>>(span: &mut S, name: &str, amount: i64) -> i64 {
    if amount < 0 {
        warn!(span, name = name, amount = amount, "a refund");
    }
    amount
}

/// The edge: a subscriber that prints, and tracing for the logs.
fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let rows = ["rent = 1200", "coffee = 4", "oops", "refund = -30"];
    let total = import(&mut seam::tracing::Tracing, "october", &rows);
    println!("total: {total}");
}

#[cfg(test)]
mod tests {
    use seam::tracing::{Captured, Level};

    use super::*;

    #[test]
    fn bad_rows_are_skipped_and_said_so() {
        let mut logs = Captured::new();
        assert_eq!(import(&mut logs, "b1", &["a = 2", "?", "b = 3"]), 5);
        let records = logs.records();
        assert_eq!(logs.messages_at(Level::WARN), ["skipping a bad row"]);
        assert_eq!(records[0].field("line"), Some("2"));
        let done = records.last().unwrap();
        assert_eq!(
            (done.field("total"), done.field("skipped")),
            (Some("5"), Some("1"))
        );
        // Every record is inside the import's span, with its fields.
        assert!(records.iter().all(|r| r.spans[0].name == "import"));
        assert_eq!(records[0].spans[0].fields[0], ("batch".into(), "b1".into()));
    }

    /// A world file says what should have been logged.
    #[test]
    fn checked_in_a_world_file() {
        let mut logs = Captured::new();
        import(&mut logs, "b2", &["refund = -5"]);
        seam::test::world_file::check(
            &logs,
            r#"logs {
                record "a refund" level="warn" { span "import"; field "amount" "-5" }
                record "imported" { field "total" "-5" }
            }"#,
        )
        .unwrap();
    }
}
