//! Queries and transactions on in-memory SQLite.
#![cfg(feature = "sqlite")]

use seam_core::io;
use seam_core::prelude::*;
use seam_sqlx::{
    Acquire, Begin, Connection, Execute, Fetch, Sqlx, Transaction, Value, execute_async,
    execute_send, fetch_all_async, fetch_one_async, fetch_optional_send,
};

/// A core: moves money between accounts, in a transaction.
async fn transfer<C, T>(c: &mut C, from: i64, to: i64, amount: i64) -> io::Result<()>
where
    C: AsyncProvide<Begin<T>>,
    T: for<'a> AsyncPerform<Execute<'a>> + for<'a> AsyncPerform<Fetch<'a>>,
{
    c.acquire_async(Begin::new(), async |_, tx| {
        execute_async(
            tx,
            "UPDATE accounts SET balance = balance - ? WHERE id = ?",
            &[amount.into(), from.into()],
        )
        .await?;
        let left = fetch_one_async(
            tx,
            "SELECT balance FROM accounts WHERE id = ?",
            &[from.into()],
        )
        .await?;
        if left.get("balance").and_then(Value::as_int).unwrap_or(0) < 0 {
            // Failing the scope rolls the update back.
            return Err(io::Error::other("insufficient funds"));
        }
        execute_async(
            tx,
            "UPDATE accounts SET balance = balance + ? WHERE id = ?",
            &[amount.into(), to.into()],
        )
        .await?;
        Ok(())
    })
    .await
}

async fn balances(db: &mut Sqlx) -> Vec<i64> {
    fetch_all_async(db, "SELECT balance FROM accounts ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get("balance").and_then(Value::as_int).unwrap())
        .collect()
}

#[test]
fn transactions_commit_or_roll_back() {
    seam_tokio::block_on(async {
        let mut db = Sqlx::memory().await.unwrap();
        execute_async(
            &mut db,
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance INTEGER)",
            &[],
        )
        .await
        .unwrap();
        let added = execute_async(&mut db, "INSERT INTO accounts VALUES (1, 100), (2, 0)", &[])
            .await
            .unwrap();
        assert_eq!(added, 2);

        transfer::<_, Transaction>(&mut db, 1, 2, 30).await.unwrap();
        assert_eq!(balances(&mut db).await, [70, 30]);

        let failed = transfer::<_, Transaction>(&mut db, 1, 2, 500).await;
        assert_eq!(failed.unwrap_err().to_string(), "insufficient funds");
        assert_eq!(balances(&mut db).await, [70, 30]);
    });
}

#[test]
fn values_round_trip() {
    seam_tokio::block_on(async {
        let mut db = Sqlx::memory().await.unwrap();
        execute_send(
            &mut db,
            "CREATE TABLE t (i INTEGER, f REAL, s TEXT, b BLOB, n TEXT)",
            &[],
        )
        .await
        .unwrap();
        let values = [
            Value::Int(-7),
            Value::Float(1.5),
            Value::from("text"),
            Value::from(&b"\x00\x01"[..]),
            Value::Null,
        ];
        execute_send(&mut db, "INSERT INTO t VALUES (?, ?, ?, ?, ?)", &values)
            .await
            .unwrap();
        let row = fetch_optional_send(&mut db, "SELECT * FROM t", &[])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.columns, ["i", "f", "s", "b", "n"]);
        assert_eq!(row.values, values);
        assert!(
            fetch_optional_send(&mut db, "SELECT * FROM t WHERE i = 0", &[])
                .await
                .unwrap()
                .is_none()
        );
        let none = fetch_one_async(&mut db, "SELECT * FROM t WHERE i = 0", &[]).await;
        assert_eq!(none.unwrap_err().kind(), io::ErrorKind::NotFound);
        let bad = execute_async(&mut db, "SELEKT", &[]).await;
        assert!(bad.is_err());
    });
}

#[test]
fn a_connection_keeps_its_session() {
    seam_tokio::block_on(async {
        let mut db = Sqlx::memory().await.unwrap();
        let n = db
            .acquire_async(Acquire::<Connection>::new(), async |_, conn| {
                execute_async(conn, "CREATE TEMP TABLE scratch (x INTEGER)", &[]).await?;
                execute_async(conn, "INSERT INTO scratch VALUES (1), (2), (3)", &[]).await?;
                let rows = fetch_all_async(conn, "SELECT x FROM scratch", &[]).await?;
                io::Result::Ok(rows.len())
            })
            .await
            .unwrap();
        assert_eq!(n, 3);
    });
}

#[test]
fn in_a_root() {
    seam_tokio::block_on(async {
        use seam_sqlx::effects as sql;
        let db = Sqlx::memory().await.unwrap();
        let mut root = seam_core::root! {Sqlx<sql::Execute, sql::Fetch, sql::Begin>::clone(&db)};
        execute_async(&mut root, "CREATE TABLE k (v TEXT)", &[])
            .await
            .unwrap();
        root.acquire_async(Begin::<Transaction>::new(), async |_, tx| {
            execute_async(tx, "INSERT INTO k VALUES ('in a root')", &[]).await
        })
        .await
        .unwrap();
        let row = fetch_one_async(&mut root, "SELECT v FROM k", &[])
            .await
            .unwrap();
        assert_eq!(row.get("v").and_then(Value::as_text), Some("in a root"));
    });
}

/// With the `smol` feature, the same core runs under smol, which has no
/// Tokio runtime.
#[cfg(feature = "smol")]
#[test]
fn under_smol() {
    seam_smol::block_on(async {
        let mut db = Sqlx::memory().await?;
        execute_async(
            &mut db,
            "CREATE TABLE accounts (id INTEGER, balance INTEGER)",
            &[],
        )
        .await?;
        execute_async(&mut db, "INSERT INTO accounts VALUES (1, 10), (2, 0)", &[]).await?;
        transfer(&mut db, 1, 2, 4).await?;
        let rows =
            fetch_all_async(&mut db, "SELECT balance FROM accounts ORDER BY id", &[]).await?;
        assert_eq!(rows[1].get("balance"), Some(&Value::from(4_i64)));
        io::Result::Ok(())
    })
    .unwrap();
}
