//! The canonical row encoding and the per-table SHA-256 of ADR-0028 §6.4
//! step 6.
//!
//! Both sides of a conversion hash the same thing: every value is first
//! brought to the [`Kind`] of its column (decided by the PostgreSQL column
//! type, the typed side) and then encoded with a tag and its length, so no
//! two different rows encode alike. The engines store some values in
//! different but equal forms, which the encoding folds:
//! - JSON: `jsonb` reorders object keys, drops whitespace and normalises
//!   numbers (`1e2` reads back as `100`); JSON is hashed with sorted keys,
//!   no whitespace and numbers as integers when integral;
//! - booleans: `smallint`/`boolean` on PostgreSQL, `0`/`1` on SQLite; both
//!   hash as integers;
//! - floating point: hashed by bit pattern (`-0.0` as `0.0`).
//!
//! Rows are hashed in primary-key order (byte order for text on both
//! engines); a table without a primary key hashes the sorted list of its
//! row digests instead.

use std::borrow::Cow;
use std::fmt::Write as _;

use forgebase_db::Value;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

/// How a column's values are compared, from its PostgreSQL type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `smallint`, `integer`, `bigint`.
    Int,
    /// `boolean` (0 or 1 on SQLite).
    Bool,
    /// `double precision`, `real`.
    Float,
    /// `text`, `varchar`, `char`, `citext`, `name`.
    Text,
    /// `jsonb`, `json` (JSON text on SQLite).
    Json,
    /// `bytea` (BLOB on SQLite).
    Bytes,
    /// Anything else: compared and copied as its text form.
    Other,
}

impl Kind {
    /// The kind of a PostgreSQL type, by `pg_type.typname` (of the base type
    /// for a domain).
    pub fn of_pg(typname: &str) -> Kind {
        match typname {
            "int2" | "int4" | "int8" => Kind::Int,
            "bool" => Kind::Bool,
            "float4" | "float8" => Kind::Float,
            "text" | "varchar" | "bpchar" | "citext" | "name" => Kind::Text,
            "json" | "jsonb" => Kind::Json,
            "bytea" => Kind::Bytes,
            _ => Kind::Other,
        }
    }

    /// Whether ordering by this kind needs a byte-order collation.
    pub fn is_textual(self) -> bool {
        matches!(self, Kind::Text | Kind::Other)
    }
}

/// The text of a value for text-like columns (`Text`, `Other`), shared by
/// the copy and the hash so both see the same string.
pub(crate) fn text_of(v: &Value) -> Result<Cow<'_, str>, String> {
    match v {
        Value::Text(s) => Ok(Cow::Borrowed(s)),
        Value::Integer(i) => Ok(Cow::Owned(i.to_string())),
        Value::Real(f) => Ok(Cow::Owned(real_text(*f))),
        Value::Blob(b) => std::str::from_utf8(b)
            .map(Cow::Borrowed)
            .map_err(|_| "a BLOB that is not UTF-8 text in a text column".to_owned()),
        Value::Null => Err("null".into()),
    }
}

/// A float as PostgreSQL's `float8` input reads it back exactly.
pub(crate) fn real_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f == f64::INFINITY {
        "Infinity".into()
    } else if f == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        // `{:?}` is the shortest representation that reads back as the same f64.
        format!("{f:?}")
    }
}

/// An integer value of a column of kind `Int`/`Bool`.
pub(crate) fn int_of(v: &Value) -> Result<i64, String> {
    match v {
        Value::Integer(i) => Ok(*i),
        Value::Real(f) if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.0e15 => Ok(*f as i64),
        Value::Text(s) => match s.trim() {
            "t" | "true" => Ok(1),
            "f" | "false" => Ok(0),
            t => t.parse().map_err(|_| format!("{s:?} is not an integer")),
        },
        _ => Err("not an integer".into()),
    }
}

/// A float value of a column of kind `Float`.
pub(crate) fn float_of(v: &Value) -> Result<f64, String> {
    match v {
        Value::Real(f) => Ok(*f),
        Value::Integer(i) => Ok(*i as f64),
        Value::Text(s) => match s.trim() {
            "NaN" => Ok(f64::NAN),
            "Infinity" | "inf" => Ok(f64::INFINITY),
            "-Infinity" | "-inf" => Ok(f64::NEG_INFINITY),
            t => t.parse().map_err(|_| format!("{s:?} is not a number")),
        },
        _ => Err("not a number".into()),
    }
}

/// The bytes of a value of a column of kind `Bytes`.
pub(crate) fn bytes_of(v: &Value) -> Result<Cow<'_, [u8]>, String> {
    match v {
        Value::Blob(b) => Ok(Cow::Borrowed(b)),
        Value::Text(s) => Ok(Cow::Borrowed(s.as_bytes())),
        _ => Err("not a BLOB".into()),
    }
}

/// JSON text of a value of a column of kind `Json`.
pub(crate) fn json_text_of(v: &Value) -> Result<Cow<'_, str>, String> {
    match v {
        Value::Text(s) => Ok(Cow::Borrowed(s)),
        Value::Integer(i) => Ok(Cow::Owned(i.to_string())),
        Value::Real(f) if f.is_finite() => Ok(Cow::Owned(real_text(*f))),
        Value::Blob(b) => std::str::from_utf8(b)
            .map(Cow::Borrowed)
            .map_err(|_| "a BLOB in a JSON column".to_owned()),
        _ => Err("not JSON".into()),
    }
}

/// Canonical JSON: object keys sorted by bytes, no whitespace, integral
/// numbers as integers, other numbers in exponent form.
///
/// # Errors
/// Text that is not JSON.
pub fn canonical_json(text: &str) -> Result<String, String> {
    let v: Json = serde_json::from_str(text).map_err(|e| format!("invalid JSON ({e})"))?;
    let mut out = String::with_capacity(text.len());
    write_json(&v, &mut out);
    Ok(out)
}

fn write_json(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else {
                match n.as_f64() {
                    Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 => {
                        let _ = write!(out, "{}", f as i64);
                    }
                    Some(f) => {
                        let _ = write!(out, "{f:e}");
                    }
                    None => out.push_str(&n.to_string()),
                }
            }
        }
        Json::String(s) => out.push_str(&Json::String(s.clone()).to_string()),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Json::String(k.clone()).to_string());
                out.push(':');
                if let Some(v) = map.get(k) {
                    write_json(v, out);
                }
            }
            out.push('}');
        }
    }
}

/// JSON text without whitespace outside strings, everything else kept as
/// written (numbers, key order, escapes). Turns `jsonb`'s output
/// (`{"a": [1, 2]}`) into the compact text SQLite stores.
pub fn compact_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// The SHA-256 of one table's rows in the canonical encoding.
#[derive(Debug)]
pub struct TableHash {
    kinds: Vec<Kind>,
    hasher: Sha256,
    /// Row digests of a table without a primary key (sorted at the end).
    unordered: Option<Vec<[u8; 32]>>,
    row: Vec<u8>,
    rows: u64,
}

impl TableHash {
    /// A hash over rows whose columns have `kinds`. `ordered` is false for
    /// tables without a primary key.
    pub fn new(kinds: Vec<Kind>, ordered: bool) -> Self {
        let mut hasher = Sha256::new();
        for k in &kinds {
            hasher.update([*k as u8]);
        }
        Self {
            kinds,
            hasher,
            unordered: (!ordered).then(Vec::new),
            row: Vec::with_capacity(256),
            rows: 0,
        }
    }

    /// Adds one row (values aligned with the kinds).
    ///
    /// # Errors
    /// A value that does not fit its column's kind (the message names the
    /// column by position).
    pub fn push(&mut self, values: &[Value]) -> Result<(), String> {
        self.row.clear();
        for (i, (v, kind)) in values.iter().zip(&self.kinds).enumerate() {
            encode(&mut self.row, *kind, v).map_err(|e| format!("column {}: {e}", i + 1))?;
        }
        match &mut self.unordered {
            Some(digests) => digests.push(Sha256::digest(&self.row).into()),
            None => {
                self.hasher.update((self.row.len() as u64).to_be_bytes());
                self.hasher.update(&self.row);
            }
        }
        self.rows += 1;
        Ok(())
    }

    /// Row count and hex SHA-256.
    pub fn finish(mut self) -> (u64, String) {
        if let Some(mut digests) = self.unordered.take() {
            digests.sort_unstable();
            for d in &digests {
                self.hasher.update(d);
            }
        }
        (self.rows, hex(&self.hasher.finalize()))
    }
}

fn encode(out: &mut Vec<u8>, kind: Kind, v: &Value) -> Result<(), String> {
    let mut put = |tag: u8, bytes: &[u8]| {
        out.push(tag);
        out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        out.extend_from_slice(bytes);
    };
    if v.is_null() {
        put(0, &[]);
        return Ok(());
    }
    match kind {
        Kind::Int | Kind::Bool => put(1, &int_of(v)?.to_be_bytes()),
        Kind::Float => {
            let f = float_of(v)?;
            let f = if f == 0.0 { 0.0 } else { f };
            put(2, &f.to_bits().to_be_bytes());
        }
        Kind::Text | Kind::Other => put(3, text_of(v)?.as_bytes()),
        Kind::Json => put(4, canonical_json(&json_text_of(v)?)?.as_bytes()),
        Kind::Bytes => put(5, &bytes_of(v)?),
    }
    Ok(())
}

/// Lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_forms_of_one_document_hash_alike() {
        let a = canonical_json(r#"{"b":[1,2.0,1e2],"a":{"y":null,"x":"é"}}"#).unwrap();
        let b = canonical_json(r#"{"a": {"x": "é", "y": null}, "b": [1, 2, 100]}"#).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, r#"{"a":{"x":"é","y":null},"b":[1,2,100]}"#);
        assert_eq!(canonical_json("1.5").unwrap(), "1.5e0");
    }

    #[test]
    fn compact_keeps_strings() {
        assert_eq!(
            compact_json(r#"{"a b": [1, "x \" y"], "c": {}}"#),
            r#"{"a b":[1,"x \" y"],"c":{}}"#
        );
    }

    #[test]
    fn engines_forms_hash_alike_and_differences_do_not() {
        let kinds = vec![Kind::Int, Kind::Bool, Kind::Float, Kind::Text, Kind::Json, Kind::Bytes];
        let sqlite = vec![
            Value::Integer(7),
            Value::Integer(1),
            Value::Real(-0.0),
            Value::Text("hé".into()),
            Value::Text(r#"{"b":1,"a":2}"#.into()),
            Value::Blob(vec![0, 1, 255]),
        ];
        let pg = vec![
            Value::Text("7".into()),
            Value::Text("t".into()),
            Value::Real(0.0),
            Value::Text("hé".into()),
            Value::Text(r#"{"a": 2, "b": 1}"#.into()),
            Value::Blob(vec![0, 1, 255]),
        ];
        let mut h1 = TableHash::new(kinds.clone(), true);
        h1.push(&sqlite).unwrap();
        let mut h2 = TableHash::new(kinds.clone(), true);
        h2.push(&pg).unwrap();
        assert_eq!(h1.finish(), h2.finish());

        let mut other = pg.clone();
        other[3] = Value::Text("he".into());
        let mut h3 = TableHash::new(kinds.clone(), true);
        h3.push(&other).unwrap();
        let mut h4 = TableHash::new(kinds, true);
        h4.push(&pg).unwrap();
        assert_ne!(h3.finish(), h4.finish());
    }

    #[test]
    fn null_and_empty_text_differ() {
        let mut a = TableHash::new(vec![Kind::Text], true);
        a.push(&[Value::Null]).unwrap();
        let mut b = TableHash::new(vec![Kind::Text], true);
        b.push(&[Value::Text(String::new())]).unwrap();
        assert_ne!(a.finish().1, b.finish().1);
    }

    #[test]
    fn unordered_tables_ignore_row_order() {
        let rows = [vec![Value::Integer(1)], vec![Value::Integer(2)]];
        let mut a = TableHash::new(vec![Kind::Int], false);
        a.push(&rows[0]).unwrap();
        a.push(&rows[1]).unwrap();
        let mut b = TableHash::new(vec![Kind::Int], false);
        b.push(&rows[1]).unwrap();
        b.push(&rows[0]).unwrap();
        assert_eq!(a.finish(), b.finish());
    }
}
