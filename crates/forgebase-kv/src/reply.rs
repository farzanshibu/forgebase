//! The protocol-neutral reply of a command.
//!
//! The shapes match RESP2 so a RESP listener (FB-32) maps them one to one:
//! [`Reply::Nil`] is a null bulk string, [`Reply::Status`] a simple string,
//! [`Reply::Int`] an integer, [`Reply::Bulk`] a bulk string and
//! [`Reply::Array`] an array.

/// A command's answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// No value.
    Nil,
    /// A short status such as `OK` or a type name.
    Status(String),
    /// An integer.
    Int(i64),
    /// Binary-safe bytes.
    Bulk(Vec<u8>),
    /// A list of replies.
    Array(Vec<Reply>),
}

impl Reply {
    /// `OK`.
    pub fn ok() -> Self {
        Reply::Status("OK".into())
    }

    /// A bulk reply from anything byte-like.
    pub fn bulk(b: impl Into<Vec<u8>>) -> Self {
        Reply::Bulk(b.into())
    }

    /// A floating point number rendered like Redis (`inf`, `-inf`, shortest round-trip).
    pub fn double(v: f64) -> Self {
        Reply::Bulk(format_double(v).into_bytes())
    }

    /// An integer from a count.
    pub fn count(n: usize) -> Self {
        Reply::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

/// Redis-style number text.
pub(crate) fn format_double(v: f64) -> String {
    if v == f64::INFINITY {
        "inf".into()
    } else if v == f64::NEG_INFINITY {
        "-inf".into()
    } else if v.fract() == 0.0 && v.abs() < 1e17 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
