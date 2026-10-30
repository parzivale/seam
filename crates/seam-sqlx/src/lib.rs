//! SQL databases for seam: queries and transactions are effects.
//!
//! A core that uses a database bounds on [`Execute`] and [`Fetch`], with
//! [`Begin`] for a transaction, and gets portable [`Value`]s and [`Row`]s
//! back, so the same core runs against any database sqlx's `Any` driver
//! reaches (SQLite, Postgres, MySQL):
//!
//! - [`Sqlx`] performs them on a `sqlx::AnyPool`, connected by URL.
//! - [`Sqlx::memory`] is an in-memory SQLite database, for tests: a real
//!   database with real SQL, that touches nothing outside the process and
//!   starts empty every time.
//!
//! `Sqlx` is async only. It runs on Tokio (feature `tokio`, on by default)
//! inside a Tokio runtime, and on smol (feature `smol`) anywhere else, so
//! it works under smol and futures-rs' executors with `smol` on.
//! A transaction is a scope, as everything that must be closed is in seam:
//! closing it commits, and a scope that fails, panics or is cancelled
//! rolls it back.
//!
//! # In tests
//! ```
//! # #[cfg(feature = "sqlite")] {
//! use seam_core::prelude::*;
//! use seam_sqlx::{Begin, Sqlx, Value, execute_async, fetch_all_async};
//!
//! seam_tokio::block_on(async {
//!     let mut db = Sqlx::memory().await.unwrap();
//!     execute_async(&mut db, "CREATE TABLE notes (id INTEGER, body TEXT)", &[]).await.unwrap();
//!     db.acquire_async(Begin::new(), async |_, tx| {
//!         for (id, body) in [(1, "first"), (2, "second")] {
//!             execute_async(tx, "INSERT INTO notes VALUES (?, ?)", &[id.into(), body.into()]).await?;
//!         }
//!         seam_core::io::Result::Ok(())
//!     })
//!     .await
//!     .unwrap();
//!     let rows = fetch_all_async(&mut db, "SELECT body FROM notes ORDER BY id", &[]).await.unwrap();
//!     assert_eq!(rows[1].get("body"), Some(&Value::from("second")));
//! });
//! # }
//! ```
//!
//! Queries are runtime SQL with `?` placeholders (`$1` on Postgres). Pass
//! every value as a parameter, never formatted into the SQL: this crate
//! hands the SQL to sqlx as trusted (`AssertSqlSafe`). sqlx's `query!` macros check SQL against a live database at compile
//! time, which a seam core cannot route; use them in the edge if wanted.
//!
//! # Typed rows
//!
//! Rows read back as Rust types through std's `TryFrom`, with no traits of
//! this crate's own: a value is any `T: TryFrom<&Value>` ([`Row::try_get`]
//! by name, [`Row::try_get_at`] by position), and a row any
//! `T: TryFrom<&Row>`, which the `fetch_*_as` helpers return. Tuples of up
//! to 12 columns read by position; a core's own row type is an ordinary
//! `impl`:
//!
//! ```
//! # #[cfg(feature = "sqlite")] {
//! use seam_core::io;
//! use seam_sqlx::{Row, Sqlx, execute_async, fetch_all_as_async, fetch_one_as_async};
//!
//! struct Note {
//!     id: i64,
//!     body: String,
//!     tag: Option<String>,
//! }
//!
//! impl TryFrom<&Row> for Note {
//!     type Error = io::Error;
//!     fn try_from(row: &Row) -> io::Result<Self> {
//!         Ok(Self { id: row.try_get("id")?, body: row.try_get("body")?, tag: row.try_get("tag")? })
//!     }
//! }
//!
//! seam_tokio::block_on(async {
//!     let mut db = Sqlx::memory().await?;
//!     execute_async(&mut db, "CREATE TABLE notes (id INTEGER, body TEXT, tag TEXT)", &[]).await?;
//!     execute_async(&mut db, "INSERT INTO notes VALUES (1, 'hi', NULL)", &[]).await?;
//!     let notes: Vec<Note> = fetch_all_as_async(&mut db, "SELECT * FROM notes", &[]).await?;
//!     assert_eq!((notes[0].id, notes[0].body.as_str(), notes[0].tag.as_deref()), (1, "hi", None));
//!     let (count,): (u32,) = fetch_one_as_async(&mut db, "SELECT COUNT(*) FROM notes", &[]).await?;
//!     assert_eq!(count, 1);
//!     io::Result::Ok(())
//! })
//! .unwrap();
//! # }
//! ```
//!
//! | Rust type | From |
//! |---|---|
//! | `bool` | a boolean, or the integers 0 and 1 |
//! | `i8` .. `i64`, `u8` .. `u64` | an integer in the type's range |
//! | `f32`, `f64` | a float, or an integer |
//! | `String` | text |
//! | `Vec<u8>` | bytes |
//! | `Option<T>` of any of these | `NULL` as `None` |
//! | `uuid::Uuid` (feature `uuid`) | text, or 16 bytes |
//! | `serde_json::Value` (feature `json`) | text |
//!
//! Each converts into a [`Value`] too, for parameters: UUIDs as hyphenated
//! text and JSON as text, since `Any` has neither type. A mismatch fails
//! with `InvalidData`, naming the column.
//!
//! `Any` reads only the types every database shares: a column of another
//! type fails the query, whatever it is read as. So a SQLite table declares
//! booleans `INTEGER` (not `BOOLEAN`), and UUIDs and JSON `TEXT`; on
//! Postgres, cast in the query (`SELECT id::text`, `$1::uuid`).
//!
//! # In a program
//!
//! ```no_run
//! # #[cfg(feature = "sqlite")] {
//! use seam_core::io;
//! use seam_core::prelude::*;
//! use seam_sqlx::{Execute, Fetch, Sqlx, Value, execute_send, fetch_optional_send};
//!
//! /// The core: counts a visit, and says how many there have been.
//! async fn visit<C>(c: &mut C, page: &str) -> io::Result<i64>
//! where
//!     C: for<'a> SendPerform<Execute<'a>> + for<'a> SendPerform<Fetch<'a>> + Send,
//! {
//!     execute_send(c, "INSERT INTO visits (page) VALUES (?)", &[page.into()]).await?;
//!     let row = fetch_optional_send(c, "SELECT COUNT(*) AS n FROM visits WHERE page = ?", &[page.into()])
//!         .await?
//!         .expect("a count");
//!     match row.get("n") {
//!         Some(Value::Int(n)) => Ok(*n),
//!         _ => Err(io::ErrorKind::InvalidData.into()),
//!     }
//! }
//!
//! seam_tokio::block_on(async {
//!     let mut db = Sqlx::connect("sqlite://visits.db?mode=rwc").await?;
//!     execute_send(&mut db, "CREATE TABLE IF NOT EXISTS visits (page TEXT)", &[]).await?;
//!     println!("{} visits", visit(&mut db, "/").await?);
//!     io::Result::Ok(())
//! })?;
//! # }
//! # Ok::<(), seam_core::io::Error>(())
//! ```
//!
//! In a root, `Sqlx` routes [`effects::Execute`], [`effects::Fetch`],
//! [`effects::Begin`] and [`effects::Acquire`].
//!
//! Through the `seam` crate, this crate is `seam::sqlx` (feature `sqlx`,
//! which brings in SQLite on Tokio; `smol` adds smol). For Postgres or
//! MySQL, add `seam-rs-sqlx` with that feature alongside `seam`.
//!
//! # Features
//!
//! | Feature | Default | Gives |
//! |---|---|---|
//! | `sqlite` | yes | `sqlite://` URLs, and [`Sqlx::memory`] |
//! | `postgres` | no | `postgres://` URLs |
//! | `mysql` | no | `mysql://` URLs |
//! | `tokio` | yes | runs on Tokio, inside a Tokio runtime |
//! | `smol` | no | runs on smol, outside a Tokio runtime |
//! | `uuid` | no | `uuid::Uuid` values |
//! | `json` | no | `serde_json::Value` values |
//!
#![cfg_attr(
    not(feature = "sqlite"),
    doc = "[`Sqlx::memory`]: https://docs.rs/seam-rs-sqlx/latest/seam_sqlx/struct.Sqlx.html#method.memory"
)]
#![forbid(unsafe_code)]

use std::string::String;
use std::vec::Vec;

use seam_core::io;
use seam_core::{Acquisition, Closing, Effect, Operation};

mod typed;

/// A value going into or coming out of a query, in the types every
/// database `Any` reaches agrees on.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer, of any width.
    Int(i64),
    /// A floating-point number, of any width.
    Float(f64),
    /// Text.
    Text(String),
    /// Bytes.
    Bytes(Vec<u8>),
}

macro_rules! value_from {
    ($($t:ty => |$v:ident| $e:expr;)*) => {$(
        impl From<$t> for Value {
            fn from($v: $t) -> Self {
                $e
            }
        }
    )*};
}

value_from! {
    bool => |v| Value::Bool(v);
    i8 => |v| Value::Int(v.into());
    i16 => |v| Value::Int(v.into());
    i32 => |v| Value::Int(v.into());
    i64 => |v| Value::Int(v);
    u8 => |v| Value::Int(v.into());
    u16 => |v| Value::Int(v.into());
    u32 => |v| Value::Int(v.into());
    f32 => |v| Value::Float(v.into());
    f64 => |v| Value::Float(v);
    &str => |v| Value::Text(v.into());
    String => |v| Value::Text(v);
    &[u8] => |v| Value::Bytes(v.into());
    Vec<u8> => |v| Value::Bytes(v);
}

#[cfg(feature = "uuid")]
value_from! {
    // As hyphenated text: the `Any` driver has no UUID type.
    uuid::Uuid => |v| Value::Text(v.hyphenated().to_string());
}

#[cfg(feature = "json")]
value_from! {
    // As text: the `Any` driver has no JSON type.
    serde_json::Value => |v| Value::Text(v.to_string());
    &serde_json::Value => |v| Value::Text(v.to_string());
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

impl Value {
    /// Whether this is `NULL`.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// The float, if this is one.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(x) => Some(*x),
            _ => None,
        }
    }

    /// The text, if this is text.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// The bytes, if these are bytes.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

/// A row a query returned: its columns' names and values, in order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    /// The columns' names.
    pub columns: Vec<String>,
    /// The values, one per column.
    pub values: Vec<Value>,
}

impl Row {
    /// The value of the column `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        let i = self.columns.iter().position(|c| c == name)?;
        self.values.get(i)
    }
}

/// Run a statement that returns no rows (`INSERT`, `UPDATE`, `CREATE`,
/// ...) with `params` bound to its placeholders. Returns how many rows it
/// changed. Performed on the database, a connection or a transaction.
#[derive(Debug, Clone, Copy)]
pub struct Execute<'a> {
    /// The SQL.
    pub sql: &'a str,
    /// Its parameters.
    pub params: &'a [Value],
}

/// Run a query and collect every row it returns. Performed on the
/// database, a connection or a transaction.
#[derive(Debug, Clone, Copy)]
pub struct Fetch<'a> {
    /// The SQL.
    pub sql: &'a str,
    /// Its parameters.
    pub params: &'a [Value],
}

impl Operation for Execute<'_> {
    type Outcome = io::Result<u64>;
}

impl Operation for Fetch<'_> {
    type Outcome = io::Result<Vec<Row>>;
}

/// Begin a transaction, lending it, as a handle of type `T`, for a scope:
/// closing it commits; dropping it (a failed, panicking or cancelled
/// scope) rolls it back.
pub struct Begin<T> {
    tx: core::marker::PhantomData<fn() -> T>,
}

/// Take a connection from the pool for a scope, as a handle of type `C`:
/// for statements that must share a session (temporary tables, session
/// settings). Closing it returns it to the pool.
pub struct Acquire<C> {
    conn: core::marker::PhantomData<fn() -> C>,
}

macro_rules! scopes {
    ($($ty:ident),*) => {$(
        impl<T> $ty<T> {
            /// Describes the scope.
            pub const fn new() -> Self {
                Self { tx: core::marker::PhantomData }
            }
        }
    )*};
}
scopes!(Begin);

impl<C> Acquire<C> {
    /// Describes the scope.
    pub const fn new() -> Self {
        Self {
            conn: core::marker::PhantomData,
        }
    }
}

impl<T> Default for Begin<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> Default for Acquire<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> core::fmt::Debug for Begin<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Begin")
    }
}

impl<C> core::fmt::Debug for Acquire<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Acquire")
    }
}

impl<T> Acquisition for Begin<T> {
    type Resource = T;
    type Error = io::Error;
}

impl<C> Acquisition for Acquire<C> {
    type Resource = C;
    type Error = io::Error;
}

/// The keys that route this crate's effects in a root.
pub mod effects {
    /// Routes [`Execute`](crate::Execute).
    #[derive(Debug, Clone, Copy)]
    pub struct Execute;
    /// Routes [`Fetch`](crate::Fetch).
    #[derive(Debug, Clone, Copy)]
    pub struct Fetch;
    /// Routes [`Begin`](crate::Begin).
    #[derive(Debug, Clone, Copy)]
    pub struct Begin;
    /// Routes [`Acquire`](crate::Acquire).
    #[derive(Debug, Clone, Copy)]
    pub struct Acquire;
}

impl Effect for Execute<'_> {
    type Key = effects::Execute;
}
impl Effect for Fetch<'_> {
    type Key = effects::Fetch;
}
impl<T> Effect for Begin<T> {
    type Key = effects::Begin;
}
impl<C> Effect for Acquire<C> {
    type Key = effects::Acquire;
}

/// A sqlx error as an `io::Error`. Its message is built at run time, so
/// it travels as a `std::io::Error`.
fn error(e: sqlx::Error) -> io::Error {
    let kind = match &e {
        sqlx::Error::Io(e) => e.kind(),
        sqlx::Error::PoolTimedOut => std::io::ErrorKind::TimedOut,
        sqlx::Error::RowNotFound => std::io::ErrorKind::NotFound,
        sqlx::Error::Configuration(_) => std::io::ErrorKind::InvalidInput,
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => {
            std::io::ErrorKind::InvalidData
        }
        _ => std::io::ErrorKind::Other,
    };
    io::Error::from(std::io::Error::new(kind, e))
}

/// Performs queries and transactions on a `sqlx::AnyPool`.
#[derive(Debug, Clone)]
pub struct Sqlx(pub sqlx::AnyPool);

impl Sqlx {
    /// Connects to the database at `url` (`sqlite://..`, `postgres://..`,
    /// `mysql://..`, with the matching feature). Runs on Tokio inside a
    /// Tokio runtime, and on smol (feature `smol`) anywhere else.
    pub async fn connect(url: &str) -> io::Result<Self> {
        sqlx::any::install_default_drivers();
        sqlx::AnyPool::connect(url).await.map(Self).map_err(error)
    }

    /// A new, empty, in-memory SQLite database: one connection, so every
    /// statement sees the same data, and gone when the last clone is.
    #[cfg(feature = "sqlite")]
    pub async fn memory() -> io::Result<Self> {
        sqlx::any::install_default_drivers();
        sqlx::any::AnyPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect("sqlite::memory:")
            .await
            .map(Self)
            .map_err(error)
    }
}

/// A transaction from [`Begin`] on [`Sqlx`].
#[derive(Debug)]
pub struct Transaction(sqlx::Transaction<'static, sqlx::Any>);

/// A connection from [`Acquire`] on [`Sqlx`].
#[derive(Debug)]
pub struct Connection(sqlx::pool::PoolConnection<sqlx::Any>);

/// The query, with `params` bound.
///
/// sqlx accepts runtime SQL only through `AssertSqlSafe`, to keep values
/// out of SQL text. Here the SQL is the core's own and every value is a
/// bound parameter, never spliced in, which is what that asks.
fn bind<'q>(
    sql: &str,
    params: &[Value],
) -> sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments> {
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for param in params {
        query = match param {
            Value::Null => query.bind(None::<i64>),
            Value::Bool(b) => query.bind(*b),
            Value::Int(n) => query.bind(*n),
            Value::Float(x) => query.bind(*x),
            Value::Text(s) => query.bind(s.clone()),
            Value::Bytes(b) => query.bind(b.clone()),
        };
    }
    query
}

/// A row from `Any`, as portable values.
fn row(r: &sqlx::any::AnyRow) -> io::Result<Row> {
    use sqlx::any::AnyTypeInfoKind as K;
    use sqlx::{Column as _, Row as _, ValueRef as _};
    let mut out = Row::default();
    for (i, column) in r.columns().iter().enumerate() {
        out.columns.push(column.name().into());
        let raw = r.try_get_raw(i).map_err(error)?;
        if raw.is_null() {
            out.values.push(Value::Null);
            continue;
        }
        let kind = raw.type_info().kind();
        let value = match kind {
            K::Null => Value::Null,
            K::Bool => Value::Bool(r.try_get(i).map_err(error)?),
            K::SmallInt => Value::Int(r.try_get::<i16, _>(i).map_err(error)?.into()),
            K::Integer => Value::Int(r.try_get::<i32, _>(i).map_err(error)?.into()),
            K::BigInt => Value::Int(r.try_get(i).map_err(error)?),
            K::Real => Value::Float(r.try_get::<f32, _>(i).map_err(error)?.into()),
            K::Double => Value::Float(r.try_get(i).map_err(error)?),
            K::Text => Value::Text(r.try_get(i).map_err(error)?),
            K::Blob => Value::Bytes(r.try_get(i).map_err(error)?),
        };
        out.values.push(value);
    }
    Ok(out)
}

/// Implements `Execute` and `Fetch` on a type whose `$exec` expression,
/// given `s: &mut Self`, is a sqlx executor.
macro_rules! queries {
    ($ty:ty, |$s:ident| $exec:expr) => {
        impl<'a> seam_core::future::AsyncPerform<Execute<'a>> for $ty {
            async fn perform_async(&mut self, op: Execute<'a>) -> io::Result<u64> {
                let $s = self;
                let done = bind(op.sql, op.params)
                    .execute($exec)
                    .await
                    .map_err(error)?;
                Ok(done.rows_affected())
            }
        }

        impl<'a> seam_core::future::SendPerform<Execute<'a>> for $ty {
            async fn perform_send(&mut self, op: Execute<'a>) -> io::Result<u64> {
                let $s = self;
                let done = bind(op.sql, op.params)
                    .execute($exec)
                    .await
                    .map_err(error)?;
                Ok(done.rows_affected())
            }
        }

        impl<'a> seam_core::future::AsyncPerform<Fetch<'a>> for $ty {
            async fn perform_async(&mut self, op: Fetch<'a>) -> io::Result<Vec<Row>> {
                let $s = self;
                let rows = bind(op.sql, op.params)
                    .fetch_all($exec)
                    .await
                    .map_err(error)?;
                rows.iter().map(row).collect()
            }
        }

        impl<'a> seam_core::future::SendPerform<Fetch<'a>> for $ty {
            async fn perform_send(&mut self, op: Fetch<'a>) -> io::Result<Vec<Row>> {
                let $s = self;
                let rows = bind(op.sql, op.params)
                    .fetch_all($exec)
                    .await
                    .map_err(error)?;
                rows.iter().map(row).collect()
            }
        }
    };
}

queries!(Sqlx, |s| &s.0);
queries!(Transaction, |s| &mut *s.0);
queries!(Connection, |s| &mut *s.0);

/// Implements an async scope from its opening and closing bodies.
macro_rules! scope {
    ($ty:ty, $acq:ty, |$s:ident| $open:expr, |$r:ident| $close:expr) => {
        impl seam_core::future::AsyncPerform<seam_core::Opening<$acq>> for $ty {
            async fn perform_async(
                &mut self,
                _: seam_core::Opening<$acq>,
            ) -> io::Result<<$acq as Acquisition>::Resource> {
                let $s = self;
                $open
            }
        }

        impl seam_core::future::SendPerform<seam_core::Opening<$acq>> for $ty {
            async fn perform_send(
                &mut self,
                _: seam_core::Opening<$acq>,
            ) -> io::Result<<$acq as Acquisition>::Resource> {
                let $s = self;
                $open
            }
        }

        impl seam_core::future::AsyncPerform<Closing<$acq>> for $ty {
            async fn perform_async(&mut self, Closing($r, _): Closing<$acq>) -> io::Result<()> {
                $close
            }
        }

        impl seam_core::future::SendPerform<Closing<$acq>> for $ty {
            async fn perform_send(&mut self, Closing($r, _): Closing<$acq>) -> io::Result<()> {
                $close
            }
        }
    };
}

scope!(
    Sqlx,
    Begin<Transaction>,
    |s| s.0.begin().await.map(Transaction).map_err(error),
    |tx| tx.0.commit().await.map_err(error)
);
scope!(
    Sqlx,
    Acquire<Connection>,
    |s| s.0.acquire().await.map(Connection).map_err(error),
    |conn| {
        drop(conn);
        Ok(())
    }
);

/// Writes each helper in its async and `Send` forms, from one body: sqlx
/// is async, so there is no sync form.
macro_rules! two_ways {
    ($(
        $(#[$m:meta])*
        pub async fn $aname:ident, $sname:ident $([$($g:ident),*])?
        ($($args:tt)*) -> $ret:ty
        where [$($bound:tt)*] $(and [$($extra:tt)*])?
        $body:block
    )*) => {$(
        $(#[$m])*
        pub async fn $aname<$($($g,)*)? C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::AsyncPerform<$($bound)*> + ?Sized,
            $($($extra)*)?
        {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::AsyncPerform::perform_async($c, $op).await };
            }
            $body
        }

        #[doc = concat!("`", stringify!($aname), "`, with a `Send` future: for multithreaded runtimes.")]
        pub async fn $sname<$($($g,)*)? C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::SendPerform<$($bound)*> + Send + ?Sized,
            $($($extra)*)?
        {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::SendPerform::perform_send($c, $op).await };
            }
            $body
        }
    )*};
}

two_ways! {
    /// Runs `sql` with `params`: [`Execute`]. Returns how many rows it
    /// changed.
    pub async fn execute_async, execute_send
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<u64>
    where [Execute<'a>]
    {
        run!(c, Execute { sql, params })
    }

    /// Every row `sql` returns: [`Fetch`].
    pub async fn fetch_all_async, fetch_all_send
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<Vec<Row>>
    where [Fetch<'a>]
    {
        run!(c, Fetch { sql, params })
    }

    /// The first row `sql` returns, or `None` if it returns none.
    pub async fn fetch_optional_async, fetch_optional_send
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<Option<Row>>
    where [Fetch<'a>]
    {
        Ok(run!(c, Fetch { sql, params })?.into_iter().next())
    }

    /// The first row `sql` returns. Fails with `NotFound` if it returns
    /// none.
    pub async fn fetch_one_async, fetch_one_send
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<Row>
    where [Fetch<'a>]
    {
        run!(c, Fetch { sql, params })?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the query returned no rows"))
    }

    /// Every row `sql` returns, each as a `T`: a tuple of columns by
    /// position, or a type of the core's own with `TryFrom<&Row>`.
    pub async fn fetch_all_as_async, fetch_all_as_send [T]
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<Vec<T>>
    where [Fetch<'a>] and [T: for<'r> TryFrom<&'r Row, Error = io::Error>,]
    {
        run!(c, Fetch { sql, params })?.iter().map(T::try_from).collect()
    }

    /// The first row `sql` returns as a `T`, or `None` if it returns none.
    pub async fn fetch_optional_as_async, fetch_optional_as_send [T]
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<Option<T>>
    where [Fetch<'a>] and [T: for<'r> TryFrom<&'r Row, Error = io::Error>,]
    {
        run!(c, Fetch { sql, params })?.first().map(T::try_from).transpose()
    }

    /// The first row `sql` returns, as a `T`. Fails with `NotFound` if it
    /// returns none.
    pub async fn fetch_one_as_async, fetch_one_as_send [T]
    (c: &mut C, sql: &str, params: &[Value]) -> io::Result<T>
    where [Fetch<'a>] and [T: for<'r> TryFrom<&'r Row, Error = io::Error>,]
    {
        run!(c, Fetch { sql, params })?
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the query returned no rows"))
            .and_then(T::try_from)
    }
}
