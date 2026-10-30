//! Rows read back as tuples, a core's own types, UUIDs and JSON.
#![cfg(feature = "sqlite")]

use seam_core::io;
use seam_sqlx::{
    Row, Sqlx, Value, execute_async, fetch_all_as_async, fetch_one_as_async, fetch_optional_as_send,
};

#[derive(Debug, PartialEq)]
struct Note {
    id: i64,
    body: String,
    pinned: bool,
    score: Option<f64>,
}

impl TryFrom<&Row> for Note {
    type Error = io::Error;
    fn try_from(row: &Row) -> io::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            body: row.try_get("body")?,
            pinned: row.try_get("pinned")?,
            score: row.try_get("score")?,
        })
    }
}

async fn notes() -> Sqlx {
    let mut db = Sqlx::memory().await.unwrap();
    execute_async(
        &mut db,
        "CREATE TABLE notes (id INTEGER, body TEXT, pinned INTEGER, score REAL)",
        &[],
    )
    .await
    .unwrap();
    for (id, body, pinned, score) in [(1, "first", true, Some(0.5)), (2, "second", false, None)] {
        execute_async(
            &mut db,
            "INSERT INTO notes VALUES (?, ?, ?, ?)",
            &[id.into(), body.into(), pinned.into(), score.into()],
        )
        .await
        .unwrap();
    }
    db
}

#[test]
fn rows_as_tuples_and_own_types() {
    seam_tokio::block_on(async {
        let mut db = notes().await;
        let all: Vec<Note> = fetch_all_as_async(&mut db, "SELECT * FROM notes ORDER BY id", &[])
            .await
            .unwrap();
        assert_eq!(
            all,
            [
                Note {
                    id: 1,
                    body: "first".into(),
                    pinned: true,
                    score: Some(0.5)
                },
                Note {
                    id: 2,
                    body: "second".into(),
                    pinned: false,
                    score: None
                },
            ]
        );

        let (n,): (u32,) = fetch_one_as_async(&mut db, "SELECT COUNT(*) FROM notes", &[])
            .await
            .unwrap();
        assert_eq!(n, 2);

        let pair: Option<(String, Option<f64>)> = fetch_optional_as_send(
            &mut db,
            "SELECT body, score FROM notes WHERE id = ?",
            &[2.into()],
        )
        .await
        .unwrap();
        assert_eq!(pair, Some(("second".into(), None)));

        let none: Option<(i64,)> =
            fetch_optional_as_send(&mut db, "SELECT id FROM notes WHERE id = 9", &[])
                .await
                .unwrap();
        assert_eq!(none, None);
    });
}

#[test]
fn mismatches_fail_and_name_the_column() {
    seam_tokio::block_on(async {
        let mut db = notes().await;
        let wrong = fetch_all_as_async::<(i64, i64), _>(&mut db, "SELECT id, body FROM notes", &[])
            .await
            .unwrap_err();
        assert_eq!(wrong.kind(), io::ErrorKind::InvalidData);
        assert!(wrong.to_string().contains("column 1"), "{wrong}");

        let width = fetch_one_as_async::<(i64,), _>(&mut db, "SELECT id, body FROM notes", &[])
            .await
            .unwrap_err();
        assert!(width.to_string().contains("expected 1 columns"), "{width}");

        let empty = fetch_one_as_async::<(i64,), _>(&mut db, "SELECT id FROM notes WHERE 0", &[])
            .await
            .unwrap_err();
        assert_eq!(empty.kind(), io::ErrorKind::NotFound);
    });
}

#[test]
fn values_convert_both_ways() {
    let row = Row {
        columns: vec!["big".into(), "small".into()],
        values: vec![Value::Int(300), Value::Int(-1)],
    };
    assert_eq!(row.try_get::<i16>("big").unwrap(), 300);
    assert!(row.try_get::<u8>("big").is_err());
    assert!(row.try_get::<u64>("small").is_err());
    assert_eq!(row.try_get::<i8>("small").unwrap(), -1);
    assert_eq!(row.try_get::<f64>("big").unwrap(), 300.0);
    assert_eq!(
        row.try_get::<i64>("gone").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(Value::from(7u8), Value::Int(7));
    assert_eq!(Value::from(None::<&str>), Value::Null);
    assert!(Value::Null.is_null());
    assert_eq!(Value::from(b"ab".as_slice()).as_bytes(), Some(&b"ab"[..]));
}

#[cfg(all(feature = "uuid", feature = "json"))]
#[test]
fn uuids_and_json_as_text() {
    seam_tokio::block_on(async {
        let mut db = Sqlx::memory().await.unwrap();
        execute_async(
            &mut db,
            "CREATE TABLE things (id TEXT, raw BLOB, data TEXT)",
            &[],
        )
        .await
        .unwrap();
        let id = uuid::Uuid::from_u128(0x1234);
        let data = serde_json::json!({ "tags": ["a", "b"], "n": 3 });
        execute_async(
            &mut db,
            "INSERT INTO things VALUES (?, ?, ?)",
            &[id.into(), id.as_bytes().as_slice().into(), (&data).into()],
        )
        .await
        .unwrap();
        let (text, raw, back): (uuid::Uuid, uuid::Uuid, serde_json::Value) =
            fetch_one_as_async(&mut db, "SELECT id, raw, data FROM things", &[])
                .await
                .unwrap();
        assert_eq!((text, raw), (id, id));
        assert_eq!(back, data);
    });
}
