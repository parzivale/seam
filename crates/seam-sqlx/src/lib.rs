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
//! A transaction is a scope, as everything that must be closed is in seam:
//! closing it commits, and a scope that fails, panics or is cancelled
//! rolls it back.
//!
//! ```
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
//! ```
//!
//! Queries are runtime SQL with `?` placeholders (`$1` on Postgres). Pass
//! every value as a parameter, never formatted into the SQL: this crate
//! hands the SQL to sqlx as trusted (`AssertSqlSafe`). sqlx's `query!` macros check SQL against a live database at compile
//! time, which a seam core cannot route; use them in the edge if wanted.
#![forbid(unsafe_code)]

use std::string::String;
use std::vec::Vec;

use seam_core::io;
use seam_core::{Acquisition, Closing, Effect, Operation};

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
    i32 => |v| Value::Int(v.into());
    i64 => |v| Value::Int(v);
    u32 => |v| Value::Int(v.into());
    f64 => |v| Value::Float(v);
    &str => |v| Value::Text(v.into());
    String => |v| Value::Text(v);
    &[u8] => |v| Value::Bytes(v.into());
    Vec<u8> => |v| Value::Bytes(v);
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

impl Value {
    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
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
        pub async fn $aname:ident, $sname:ident
        ($($args:tt)*) -> $ret:ty
        where [$($bound:tt)*]
        $body:block
    )*) => {$(
        $(#[$m])*
        pub async fn $aname<C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::AsyncPerform<$($bound)*> + ?Sized,
        {
            #[allow(unused_macros)]
            macro_rules! run {
                ($c:expr, $op:expr) => { seam_core::future::AsyncPerform::perform_async($c, $op).await };
            }
            $body
        }

        #[doc = concat!("`", stringify!($aname), "`, with a `Send` future: for multithreaded runtimes.")]
        pub async fn $sname<C>($($args)*) -> $ret
        where
            C: for<'a> seam_core::future::SendPerform<$($bound)*> + Send + ?Sized,
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
}
