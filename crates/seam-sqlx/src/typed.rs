//! Typed reads: [`Value`]s and [`Row`]s into Rust types, through std's
//! `TryFrom`, so a row type is an ordinary `TryFrom<&Row>` impl.

use std::format;
use std::string::String;
use std::vec::Vec;

use seam_core::io;

use crate::{Row, Value};

/// The value's kind, for error messages.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "NULL",
        Value::Bool(_) => "a boolean",
        Value::Int(_) => "an integer",
        Value::Float(_) => "a float",
        Value::Text(_) => "text",
        Value::Bytes(_) => "bytes",
    }
}

/// An `InvalidData` error with a message built at run time.
fn invalid(message: String) -> io::Error {
    io::Error::from(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

fn mismatch(want: &str, v: &Value) -> io::Error {
    invalid(format!("expected {want}, found {}", kind(v)))
}

/// `TryFrom<&Value>` for `$t`, and for `Option<$t>` (`NULL` is `None`).
macro_rules! decode {
    ($($t:ty => |$v:ident| $e:expr;)*) => {$(
        impl TryFrom<&Value> for $t {
            type Error = io::Error;
            fn try_from($v: &Value) -> io::Result<Self> {
                $e
            }
        }

        impl TryFrom<&Value> for Option<$t> {
            type Error = io::Error;
            fn try_from(v: &Value) -> io::Result<Self> {
                match v {
                    Value::Null => Ok(None),
                    v => <$t>::try_from(v).map(Some),
                }
            }
        }
    )*};
}

/// Integers from `Int`, failing if the value is out of the type's range.
macro_rules! decode_ints {
    ($($t:ty),*) => {
        decode! {$(
            $t => |v| match v {
                Value::Int(n) => <$t>::try_from(*n).map_err(|_| {
                    invalid(format!("{n} is out of range for {}", stringify!($t)))
                }),
                v => Err(mismatch("an integer", v)),
            };
        )*}
    };
}

decode_ints!(i8, i16, i32, i64, u8, u16, u32, u64);

decode! {
    // SQLite has no boolean type: it stores them as 0 and 1.
    bool => |v| match v {
        Value::Bool(b) => Ok(*b),
        Value::Int(0) => Ok(false),
        Value::Int(1) => Ok(true),
        v => Err(mismatch("a boolean", v)),
    };
    // Integers too, as aggregates such as `SUM` give them.
    f64 => |v| match v {
        Value::Float(x) => Ok(*x),
        Value::Int(n) => Ok(*n as f64),
        v => Err(mismatch("a number", v)),
    };
    f32 => |v| f64::try_from(v).map(|x| x as f32);
    String => |v| match v {
        Value::Text(s) => Ok(s.clone()),
        v => Err(mismatch("text", v)),
    };
    Vec<u8> => |v| match v {
        Value::Bytes(b) => Ok(b.clone()),
        v => Err(mismatch("bytes", v)),
    };
}

#[cfg(feature = "uuid")]
decode! {
    // The `Any` driver has no UUID type: they travel as text (or 16
    // bytes, read back).
    uuid::Uuid => |v| match v {
        Value::Text(s) => uuid::Uuid::try_parse(s).map_err(|e| invalid(format!("{e}"))),
        Value::Bytes(b) => uuid::Uuid::from_slice(b).map_err(|e| invalid(format!("{e}"))),
        v => Err(mismatch("a UUID, as text or bytes", v)),
    };
}

#[cfg(feature = "json")]
decode! {
    // The `Any` driver has no JSON type: it travels as text.
    serde_json::Value => |v| match v {
        Value::Text(s) => serde_json::from_str(s).map_err(|e| invalid(format!("{e}"))),
        v => Err(mismatch("JSON text", v)),
    };
}

impl Row {
    /// The column `name`, as a `T`. Fails with `NotFound` if there is no
    /// such column, and `InvalidData` if its value is not a `T`.
    ///
    /// ```
    /// use seam_sqlx::{Row, Value};
    ///
    /// let row = Row {
    ///     columns: vec!["id".into(), "body".into()],
    ///     values: vec![Value::Int(7), Value::Null],
    /// };
    /// assert_eq!(row.try_get::<i64>("id").unwrap(), 7);
    /// assert_eq!(row.try_get::<Option<String>>("body").unwrap(), None);
    /// assert!(row.try_get::<String>("body").is_err());
    /// ```
    pub fn try_get<T>(&self, name: &str) -> io::Result<T>
    where
        T: for<'v> TryFrom<&'v Value, Error = io::Error>,
    {
        let value = self.get(name).ok_or_else(|| {
            io::Error::from(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no column `{name}`"),
            ))
        })?;
        T::try_from(value).map_err(|e| invalid(format!("column `{name}`: {e}")))
    }

    /// The column at `index` (from 0), as a `T`. Fails with `NotFound` if
    /// there is no such column, and `InvalidData` if its value is not a
    /// `T`.
    pub fn try_get_at<T>(&self, index: usize) -> io::Result<T>
    where
        T: for<'v> TryFrom<&'v Value, Error = io::Error>,
    {
        let value = self.values.get(index).ok_or_else(|| {
            io::Error::from(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no column {index}"),
            ))
        })?;
        T::try_from(value).map_err(|e| invalid(format!("column {index}: {e}")))
    }
}

/// `TryFrom<&Row>` for tuples, by position: the row must have exactly as
/// many columns.
macro_rules! tuples {
    ($(($($t:ident $i:tt),+))*) => {$(
        impl<$($t),+> TryFrom<&Row> for ($($t,)+)
        where
            $($t: for<'v> TryFrom<&'v Value, Error = io::Error>,)+
        {
            type Error = io::Error;
            fn try_from(row: &Row) -> io::Result<Self> {
                const N: usize = [$($i),+].len();
                if row.values.len() != N {
                    return Err(invalid(format!(
                        "expected {N} columns, found {}",
                        row.values.len()
                    )));
                }
                Ok(($(row.try_get_at::<$t>($i)?,)+))
            }
        }
    )*};
}

tuples! {
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11)
}
