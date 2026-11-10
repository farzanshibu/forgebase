//! Converting [`Value`]s to and from PostgreSQL types (ADR-0028 §7.3).
//!
//! `Value` carries no SQL type, but PostgreSQL is strictly typed, so binding
//! is driven by the parameter types the server reports for the prepared
//! statement: each value is encoded in the binary format of exactly that type.
//! Lossless conversions are accepted (`Integer` into `float8` or `numeric`,
//! integral text into `int8`, 0/1 into `bool`, a number into `text`); anything
//! else is a [`DbError::Statement`], never a silent cast.
//!
//! Decoding maps every result column back onto the five `Value` kinds, so
//! callers see the same `Rows` as on SQLite.

use std::fmt::Write as _;

use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo, PgTypeKind, PgValueFormat, PgValueRef};
use sqlx::{Encode, Postgres, Type, TypeInfo, ValueRef};

use crate::DbError;
use crate::backend::Value;

/// Type OIDs from `pg_type.dat` (stable across PostgreSQL versions).
pub(crate) mod oid {
    pub const BOOL: u32 = 16;
    pub const BYTEA: u32 = 17;
    pub const CHAR: u32 = 18;
    pub const NAME: u32 = 19;
    pub const INT8: u32 = 20;
    pub const INT2: u32 = 21;
    pub const INT4: u32 = 23;
    pub const REGPROC: u32 = 24;
    pub const TEXT: u32 = 25;
    pub const OID: u32 = 26;
    pub const XID: u32 = 28;
    pub const JSON: u32 = 114;
    pub const XML: u32 = 142;
    pub const FLOAT4: u32 = 700;
    pub const FLOAT8: u32 = 701;
    pub const UNKNOWN: u32 = 705;
    pub const BPCHAR: u32 = 1042;
    pub const VARCHAR: u32 = 1043;
    pub const DATE: u32 = 1082;
    pub const TIME: u32 = 1083;
    pub const TIMESTAMP: u32 = 1114;
    pub const TIMESTAMPTZ: u32 = 1184;
    pub const INTERVAL: u32 = 1186;
    pub const NUMERIC: u32 = 1700;
    pub const REGCLASS: u32 = 2205;
    pub const UUID: u32 = 2950;
    pub const JSONB: u32 = 3802;
    pub const XID8: u32 = 5069;
}

/// Microseconds between 1970-01-01 and PostgreSQL's epoch 2000-01-01.
const PG_EPOCH_UNIX_US: i64 = 946_684_800_000_000;
/// Days between 1970-01-01 and 2000-01-01.
const PG_EPOCH_UNIX_DAYS: i64 = 10_957;

/// What a value is encoded as, resolved from the parameter's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Bool,
    Int2,
    Int4,
    Int8,
    Oid,
    Float4,
    Float8,
    Numeric,
    /// Any type whose binary input is its UTF-8 text (text, varchar, citext, enums, …).
    Text,
    Json,
    Jsonb,
    Bytea,
    Timestamptz,
    Timestamp,
    Date,
    Uuid,
}

fn target_of(ty: &PgTypeInfo) -> Option<Target> {
    match ty.kind() {
        PgTypeKind::Domain(base) => return target_of(base),
        PgTypeKind::Enum(_) => return Some(Target::Text),
        _ => {}
    }
    let Oid(o) = ty.oid()?;
    Some(match o {
        oid::BOOL => Target::Bool,
        oid::INT2 => Target::Int2,
        oid::INT4 => Target::Int4,
        oid::INT8 => Target::Int8,
        oid::OID | oid::REGPROC | oid::REGCLASS | oid::XID => Target::Oid,
        oid::FLOAT4 => Target::Float4,
        oid::FLOAT8 => Target::Float8,
        oid::NUMERIC => Target::Numeric,
        oid::TEXT | oid::VARCHAR | oid::BPCHAR | oid::NAME | oid::UNKNOWN | oid::XML | oid::CHAR => Target::Text,
        oid::JSON => Target::Json,
        oid::JSONB => Target::Jsonb,
        oid::BYTEA => Target::Bytea,
        oid::TIMESTAMPTZ => Target::Timestamptz,
        oid::TIMESTAMP => Target::Timestamp,
        oid::DATE => Target::Date,
        oid::UUID => Target::Uuid,
        // Extension types whose binary input is text.
        _ if matches!(ty.name(), "citext" | "ltree" | "lquery") => Target::Text,
        _ => return None,
    })
}

/// One parameter, already converted for its declared type.
#[derive(Debug)]
pub(crate) struct PgParam {
    ty: PgTypeInfo,
    /// Binary encoding; `None` is SQL NULL.
    bytes: Option<Vec<u8>>,
}

impl PgParam {
    /// Converts `value` for a parameter of type `ty` (parameter `index`, 1-based, for messages).
    pub(crate) fn new(value: &Value, ty: &PgTypeInfo, index: usize) -> Result<Self, DbError> {
        let bytes = if value.is_null() {
            None
        } else if let PgTypeKind::Array(elem) = ty.kind() {
            Some(encode_array(value, elem, ty, index)?)
        } else {
            let target = target_of(ty).ok_or_else(|| mismatch(value, ty, index, "this type cannot be bound"))?;
            Some(encode_scalar(value, target).map_err(|why| mismatch(value, ty, index, &why))?)
        };
        Ok(Self { ty: ty.clone(), bytes })
    }
}

fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Integer(_) => "an integer",
        Value::Real(_) => "a real number",
        Value::Text(_) => "text",
        Value::Blob(_) => "a blob",
    }
}

fn mismatch(value: &Value, ty: &PgTypeInfo, index: usize, why: &str) -> DbError {
    DbError::Statement(format!(
        "parameter ${index} has type {} and cannot take {}: {why}",
        ty.name(),
        kind_name(value)
    ))
}

impl Type<Postgres> for PgParam {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(oid::UNKNOWN))
    }

    fn compatible(_ty: &PgTypeInfo) -> bool {
        true
    }
}

impl Encode<'_, Postgres> for PgParam {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        match &self.bytes {
            Some(b) => {
                buf.extend_from_slice(b);
                Ok(IsNull::No)
            }
            None => Ok(IsNull::Yes),
        }
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.ty.clone())
    }

    fn size_hint(&self) -> usize {
        self.bytes.as_ref().map_or(0, Vec::len)
    }
}

fn int_of(v: &Value) -> Result<i64, String> {
    match v {
        Value::Integer(i) => Ok(*i),
        Value::Real(f) if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.0e15 => Ok(*f as i64),
        Value::Text(s) => s.trim().parse().map_err(|_| format!("{s:?} is not an integer")),
        _ => Err("not an integer".into()),
    }
}

fn float_of(v: &Value) -> Result<f64, String> {
    match v {
        Value::Integer(i) => Ok(*i as f64),
        Value::Real(f) => Ok(*f),
        Value::Text(s) => s.trim().parse().map_err(|_| format!("{s:?} is not a number")),
        _ => Err("not a number".into()),
    }
}

fn text_of(v: &Value) -> Result<String, String> {
    match v {
        Value::Text(s) => Ok(s.clone()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Real(f) => Ok(real_to_text(*f)),
        Value::Blob(b) => String::from_utf8(b.clone()).map_err(|_| "the blob is not UTF-8 text".into()),
        Value::Null => Err("null".into()),
    }
}

/// SQLite's text form of a real: integral values keep a `.0`.
fn real_to_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

fn encode_scalar(v: &Value, target: Target) -> Result<Vec<u8>, String> {
    Ok(match target {
        Target::Bool => match v {
            Value::Integer(0) => vec![0],
            Value::Integer(1) => vec![1],
            Value::Text(s) => match s.trim().to_ascii_lowercase().as_str() {
                "0" | "f" | "false" => vec![0],
                "1" | "t" | "true" => vec![1],
                _ => return Err(format!("{s:?} is not a boolean")),
            },
            _ => return Err("booleans take 0 or 1".into()),
        },
        Target::Int2 => i16::try_from(int_of(v)?)
            .map_err(|_| "out of range for int2".to_owned())?
            .to_be_bytes()
            .to_vec(),
        Target::Int4 => i32::try_from(int_of(v)?)
            .map_err(|_| "out of range for int4".to_owned())?
            .to_be_bytes()
            .to_vec(),
        Target::Int8 => int_of(v)?.to_be_bytes().to_vec(),
        Target::Oid => u32::try_from(int_of(v)?)
            .map_err(|_| "out of range for oid".to_owned())?
            .to_be_bytes()
            .to_vec(),
        Target::Float4 => (float_of(v)? as f32).to_be_bytes().to_vec(),
        Target::Float8 => float_of(v)?.to_be_bytes().to_vec(),
        Target::Numeric => {
            let text = match v {
                Value::Integer(i) => i.to_string(),
                Value::Real(f) if f.is_finite() => format!("{f}"),
                Value::Real(_) => "NaN".to_owned(),
                Value::Text(s) => s.trim().to_owned(),
                _ => return Err("not a number".into()),
            };
            numeric_from_decimal(&text).ok_or_else(|| format!("{text:?} is not a decimal number"))?
        }
        Target::Text => match v {
            Value::Blob(_) => return Err("bind blobs to bytea".into()),
            other => text_of(other)?.into_bytes(),
        },
        Target::Json | Target::Jsonb => {
            let text = match v {
                Value::Text(s) => s.clone(),
                Value::Integer(i) => i.to_string(),
                Value::Real(f) if f.is_finite() => real_to_text(*f),
                _ => return Err("JSON parameters take JSON text".into()),
            };
            let mut out = Vec::with_capacity(text.len() + 1);
            if target == Target::Jsonb {
                out.push(1); // jsonb binary format version
            }
            out.extend_from_slice(text.as_bytes());
            out
        }
        Target::Bytea => match v {
            Value::Blob(b) => b.clone(),
            Value::Text(s) => s.as_bytes().to_vec(),
            _ => return Err("bytea takes a blob".into()),
        },
        Target::Timestamptz | Target::Timestamp => {
            let Value::Text(s) = v else {
                return Err("timestamps take RFC 3339 text".into());
            };
            let us = parse_timestamp_us(s).ok_or_else(|| format!("{s:?} is not an RFC 3339 timestamp"))?;
            (us - PG_EPOCH_UNIX_US).to_be_bytes().to_vec()
        }
        Target::Date => {
            let Value::Text(s) = v else {
                return Err("dates take YYYY-MM-DD text".into());
            };
            let days = parse_date_days(s).ok_or_else(|| format!("{s:?} is not a YYYY-MM-DD date"))?;
            i32::try_from(days - PG_EPOCH_UNIX_DAYS)
                .map_err(|_| "date out of range".to_owned())?
                .to_be_bytes()
                .to_vec()
        }
        Target::Uuid => {
            let Value::Text(s) = v else {
                return Err("uuid takes text".into());
            };
            uuid::Uuid::parse_str(s.trim())
                .map_err(|_| format!("{s:?} is not a UUID"))?
                .as_bytes()
                .to_vec()
        }
    })
}

/// A one-dimensional array parameter from a JSON array in `Value::Text`
/// (e.g. `= ANY (?1)` with `["a","b"]`).
fn encode_array(value: &Value, elem: &PgTypeInfo, ty: &PgTypeInfo, index: usize) -> Result<Vec<u8>, DbError> {
    let Value::Text(json) = value else {
        return Err(mismatch(value, ty, index, "array parameters take a JSON array"));
    };
    let items: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(|_| mismatch(value, ty, index, "array parameters take a JSON array"))?;
    let target = target_of(elem).ok_or_else(|| mismatch(value, ty, index, "unsupported element type"))?;
    let Some(Oid(elem_oid)) = elem.oid() else {
        return Err(mismatch(value, ty, index, "unsupported element type"));
    };
    let len = i32::try_from(items.len()).map_err(|_| mismatch(value, ty, index, "array too long"))?;
    let mut out = Vec::new();
    out.extend_from_slice(&1i32.to_be_bytes()); // dimensions
    let has_null = items.iter().any(serde_json::Value::is_null);
    out.extend_from_slice(&i32::from(has_null).to_be_bytes());
    out.extend_from_slice(&elem_oid.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&1i32.to_be_bytes()); // lower bound
    for item in &items {
        let v = match item {
            serde_json::Value::Null => {
                out.extend_from_slice(&(-1i32).to_be_bytes());
                continue;
            }
            serde_json::Value::Bool(b) => Value::Integer(i64::from(*b)),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Integer(i),
                None => Value::Real(n.as_f64().unwrap_or(f64::NAN)),
            },
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        };
        let bytes = encode_scalar(&v, target).map_err(|why| mismatch(value, ty, index, &why))?;
        let n = i32::try_from(bytes.len()).map_err(|_| mismatch(value, ty, index, "element too long"))?;
        out.extend_from_slice(&n.to_be_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// PostgreSQL's binary `numeric` from decimal text (`-12.50`, `1e3`, `NaN`).
fn numeric_from_decimal(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if text.eq_ignore_ascii_case("nan") {
        for v in [0u16, 0, 0xC000, 0] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        return Some(out);
    }
    let (neg, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(p) => (&body[..p], body[p + 1..].parse::<i32>().ok()?),
        None => (body, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().chain(frac_part.bytes()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    // All digits with the decimal point after `point` of them.
    let digits: String = format!("{int_part}{frac_part}");
    let point = i32::try_from(int_part.len()).ok()? + exp;
    let dscale = (i32::try_from(digits.len()).ok()? - point).max(0);
    // Pad so the point falls on a base-10000 group boundary.
    let lead_pad = (4 - point.rem_euclid(4)) % 4;
    let mut padded = "0".repeat(usize::try_from(lead_pad).ok()?);
    padded.push_str(&digits);
    while padded.len() % 4 != 0 {
        padded.push('0');
    }
    let mut groups: Vec<i16> = padded
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).ok().and_then(|s| s.parse().ok()).unwrap_or(0))
        .collect();
    let mut weight = (point + lead_pad) / 4 - 1;
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        weight = 0;
    }
    let ndigits = i16::try_from(groups.len()).ok()?;
    out.extend_from_slice(&ndigits.to_be_bytes());
    out.extend_from_slice(&i16::try_from(weight).ok()?.to_be_bytes());
    out.extend_from_slice(&(if neg && !groups.is_empty() { 0x4000u16 } else { 0 }).to_be_bytes());
    out.extend_from_slice(&u16::try_from(dscale).ok()?.to_be_bytes());
    for g in groups {
        out.extend_from_slice(&g.to_be_bytes());
    }
    Some(out)
}

/// Decimal text of a binary `numeric`.
fn numeric_to_decimal(b: &[u8]) -> Option<String> {
    let rd = |i: usize| -> Option<u16> { Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?])) };
    let ndigits = usize::from(rd(0)?);
    let weight = i32::from(rd(2)? as i16);
    let sign = rd(4)?;
    let dscale = usize::from(rd(6)?);
    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let groups: Vec<u16> = (0..ndigits).map(|i| rd(8 + 2 * i)).collect::<Option<_>>()?;
    let mut int = String::new();
    for w in 0..=weight.max(-1) {
        let g = usize::try_from(w)
            .ok()
            .and_then(|w| groups.get(w))
            .copied()
            .unwrap_or(0);
        if int.is_empty() {
            if g != 0 {
                int = g.to_string();
            }
        } else {
            let _ = write!(int, "{g:04}");
        }
    }
    if int.is_empty() {
        int.push('0');
    }
    let mut frac = String::new();
    let mut w = weight + 1;
    while frac.len() < dscale {
        let g = if w < 0 {
            0
        } else {
            usize::try_from(w)
                .ok()
                .and_then(|w| groups.get(w))
                .copied()
                .unwrap_or(0)
        };
        let _ = write!(frac, "{g:04}");
        w += 1;
    }
    frac.truncate(dscale);
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    s.push_str(&int);
    if !frac.is_empty() {
        s.push('.');
        s.push_str(&frac);
    }
    Some(s)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

fn parse_date_days(s: &str) -> Option<i64> {
    let s = s.trim();
    let (y, rest) = s.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.get(..2)?.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
}

/// Microseconds since the Unix epoch of `YYYY-MM-DD[T ]HH:MM:SS[.f][Z|±HH[:MM]]`
/// (no offset means UTC).
pub(crate) fn parse_timestamp_us(s: &str) -> Option<i64> {
    let s = s.trim();
    let days = parse_date_days(s.get(..10)?)?;
    let rest = s.get(10..)?;
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (h, mi, sec) = (
        rest.get(0..2)?.parse::<i64>().ok()?,
        rest.get(3..5)?.parse::<i64>().ok()?,
        rest.get(6..8)?.parse::<i64>().ok()?,
    );
    if rest.get(2..3)? != ":" || rest.get(5..6)? != ":" || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut tail = rest.get(8..)?;
    let mut micros = 0i64;
    if let Some(f) = tail.strip_prefix('.') {
        let n = f.bytes().take_while(u8::is_ascii_digit).count();
        let digits = &f[..n];
        let mut padded: String = digits.chars().take(6).collect();
        while padded.len() < 6 {
            padded.push('0');
        }
        micros = padded.parse().ok()?;
        tail = &f[n..];
    }
    let offset_s = match tail {
        "" | "Z" | "z" => 0,
        t => {
            let sign = match t.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let t = &t[1..];
            let (oh, om) = match t.split_once(':') {
                Some((a, b)) => (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?),
                None if t.len() == 4 => (t[..2].parse().ok()?, t[2..].parse().ok()?),
                None => (t.parse().ok()?, 0),
            };
            sign * (oh * 3600 + om * 60)
        }
    };
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset_s) * 1_000_000) + micros)
}

/// ForgeBase's fixed-width UTC timestamp, `YYYY-MM-DDTHH:MM:SS.mmmZ`
/// (`forgebase_schema::format_timestamp`).
pub(crate) fn format_timestamp_us(unix_us: i64, zone: bool) -> String {
    let secs = unix_us.div_euclid(1_000_000);
    let ms = unix_us.rem_euclid(1_000_000) / 1000;
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}{}",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60,
        if zone { "Z" } else { "" }
    )
}

/// Removes insignificant whitespace from JSON text without reordering keys
/// or touching numbers (`jsonb` output has a space after `:` and `,`).
pub(crate) fn compact_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut esc) = (false, false);
    for c in s.chars() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

fn be_i64(b: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

fn be_i32(b: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

fn bad(ty: &str) -> DbError {
    DbError::Statement(format!("could not decode a value of type {ty}"))
}

/// Decodes one result value.
pub(crate) fn decode(value: PgValueRef<'_>) -> Result<Value, DbError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let ty = value.type_info().into_owned();
    let bytes = value.as_bytes().map_err(|_| bad(ty.name()))?;
    match value.format() {
        PgValueFormat::Binary => decode_binary(&ty, bytes),
        PgValueFormat::Text => decode_text(&ty, bytes),
    }
}

fn base_oid(ty: &PgTypeInfo) -> u32 {
    match ty.kind() {
        PgTypeKind::Domain(base) => base_oid(base),
        PgTypeKind::Enum(_) => oid::TEXT,
        _ => match ty.oid() {
            Some(Oid(o)) => o,
            None => 0,
        },
    }
}

fn decode_binary(ty: &PgTypeInfo, b: &[u8]) -> Result<Value, DbError> {
    let name = ty.name();
    let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| bad(name));
    Ok(match base_oid(ty) {
        oid::BOOL => Value::Integer(i64::from(*b.first().ok_or_else(|| bad(name))? != 0)),
        oid::INT2 => Value::Integer(i64::from(i16::from_be_bytes(
            b.get(..2).and_then(|s| s.try_into().ok()).ok_or_else(|| bad(name))?,
        ))),
        oid::INT4 => Value::Integer(i64::from(be_i32(b).ok_or_else(|| bad(name))?)),
        oid::INT8 => Value::Integer(be_i64(b).ok_or_else(|| bad(name))?),
        oid::OID | oid::REGPROC | oid::REGCLASS | oid::XID => Value::Integer(i64::from(u32::from_be_bytes(
            b.get(..4).and_then(|s| s.try_into().ok()).ok_or_else(|| bad(name))?,
        ))),
        oid::XID8 => Value::Integer(be_i64(b).ok_or_else(|| bad(name))?),
        oid::FLOAT4 => Value::Real(f64::from(f32::from_be_bytes(
            b.get(..4).and_then(|s| s.try_into().ok()).ok_or_else(|| bad(name))?,
        ))),
        oid::FLOAT8 => Value::Real(f64::from_be_bytes(
            b.get(..8).and_then(|s| s.try_into().ok()).ok_or_else(|| bad(name))?,
        )),
        oid::NUMERIC => {
            let s = numeric_to_decimal(b).ok_or_else(|| bad(name))?;
            Value::Real(s.parse().unwrap_or(f64::NAN))
        }
        oid::JSON => Value::Text(compact_json(&text(b)?)),
        oid::JSONB => Value::Text(compact_json(&text(b.get(1..).ok_or_else(|| bad(name))?)?)),
        oid::BYTEA => Value::Blob(b.to_vec()),
        oid::TIMESTAMPTZ => Value::Text(format_timestamp_us(
            be_i64(b).ok_or_else(|| bad(name))? + PG_EPOCH_UNIX_US,
            true,
        )),
        oid::TIMESTAMP => Value::Text(format_timestamp_us(
            be_i64(b).ok_or_else(|| bad(name))? + PG_EPOCH_UNIX_US,
            false,
        )),
        oid::DATE => {
            let (y, m, d) = civil_from_days(i64::from(be_i32(b).ok_or_else(|| bad(name))?) + PG_EPOCH_UNIX_DAYS);
            Value::Text(format!("{y:04}-{m:02}-{d:02}"))
        }
        oid::TIME => {
            let us = be_i64(b).ok_or_else(|| bad(name))?;
            let s = us / 1_000_000;
            let frac = us % 1_000_000;
            let mut out = format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60);
            if frac != 0 {
                let f = format!("{frac:06}");
                let _ = write!(out, ".{}", f.trim_end_matches('0'));
            }
            Value::Text(out)
        }
        oid::INTERVAL => {
            let us = be_i64(b).ok_or_else(|| bad(name))?;
            let days = be_i32(b.get(8..).ok_or_else(|| bad(name))?).ok_or_else(|| bad(name))?;
            let months = be_i32(b.get(12..).ok_or_else(|| bad(name))?).ok_or_else(|| bad(name))?;
            Value::Text(format!("P{months}M{days}DT{}S", us as f64 / 1e6))
        }
        oid::UUID => Value::Text(
            uuid::Uuid::from_slice(b.get(..16).ok_or_else(|| bad(name))?)
                .map_err(|_| bad(name))?
                .hyphenated()
                .to_string(),
        ),
        _ => match ty.kind() {
            PgTypeKind::Array(elem) => Value::Text(decode_array(elem, b).ok_or_else(|| bad(name))?.to_string()),
            // Text-like types (text, varchar, citext, enums, …) send their UTF-8 text.
            _ => match String::from_utf8(b.to_vec()) {
                Ok(s) => Value::Text(s),
                Err(_) => Value::Blob(b.to_vec()),
            },
        },
    })
}

/// A one-dimensional binary array as JSON (numbers stay numbers, NULL is null).
fn decode_array(elem: &PgTypeInfo, b: &[u8]) -> Option<serde_json::Value> {
    let ndim = be_i32(b)?;
    if ndim == 0 {
        return Some(serde_json::Value::Array(Vec::new()));
    }
    if ndim != 1 {
        return None;
    }
    let len = usize::try_from(be_i32(b.get(12..)?)?).ok()?;
    let mut pos = 20;
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        let n = be_i32(b.get(pos..)?)?;
        pos += 4;
        if n < 0 {
            items.push(serde_json::Value::Null);
            continue;
        }
        let n = usize::try_from(n).ok()?;
        let v = decode_binary(elem, b.get(pos..pos + n)?).ok()?;
        pos += n;
        items.push(match v {
            Value::Null => serde_json::Value::Null,
            Value::Integer(i) => serde_json::Value::from(i),
            Value::Real(f) => serde_json::Value::from(f),
            Value::Text(s) if matches!(base_oid(elem), oid::JSON | oid::JSONB) => {
                serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
            }
            Value::Text(s) => serde_json::Value::String(s),
            Value::Blob(b) => serde_json::Value::String(hex(&b)),
        });
    }
    Some(serde_json::Value::Array(items))
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

/// Text-format values (simple-protocol results: DDL and one-off statements).
/// Sessions run with `DateStyle=ISO` and `TimeZone=UTC`.
fn decode_text(ty: &PgTypeInfo, b: &[u8]) -> Result<Value, DbError> {
    let name = ty.name();
    let s = std::str::from_utf8(b).map_err(|_| bad(name))?;
    Ok(match base_oid(ty) {
        oid::BOOL => Value::Integer(i64::from(s == "t")),
        oid::INT2 | oid::INT4 | oid::INT8 | oid::OID | oid::XID | oid::XID8 => {
            Value::Integer(s.parse().map_err(|_| bad(name))?)
        }
        oid::FLOAT4 | oid::FLOAT8 | oid::NUMERIC => Value::Real(match s {
            "Infinity" => f64::INFINITY,
            "-Infinity" => f64::NEG_INFINITY,
            other => other.parse().unwrap_or(f64::NAN),
        }),
        oid::JSON | oid::JSONB => Value::Text(compact_json(s)),
        oid::BYTEA => {
            let h = s.strip_prefix("\\x").ok_or_else(|| bad(name))?;
            let bytes = (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| bad(name))?;
            Value::Blob(bytes)
        }
        oid::TIMESTAMPTZ => Value::Text(format_timestamp_us(
            parse_timestamp_us(s).ok_or_else(|| bad(name))?,
            true,
        )),
        oid::TIMESTAMP => Value::Text(format_timestamp_us(
            parse_timestamp_us(s).ok_or_else(|| bad(name))?,
            false,
        )),
        _ => Value::Text(s.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_numeric(s: &str) -> String {
        numeric_to_decimal(&numeric_from_decimal(s).unwrap()).unwrap()
    }

    #[test]
    fn numeric_binary_format() {
        assert_eq!(roundtrip_numeric("0"), "0");
        assert_eq!(roundtrip_numeric("12345"), "12345");
        assert_eq!(roundtrip_numeric("-12.50"), "-12.50");
        assert_eq!(roundtrip_numeric("0.0001"), "0.0001");
        assert_eq!(roundtrip_numeric("100000000"), "100000000");
        assert_eq!(roundtrip_numeric("1e3"), "1000");
        assert_eq!(roundtrip_numeric("NaN"), "NaN");
        assert!(numeric_from_decimal("12a").is_none());
        assert!(numeric_from_decimal("").is_none());
        // 12345.678 = groups [1, 2345, 6780], weight 1, dscale 3.
        let b = numeric_from_decimal("12345.678").unwrap();
        assert_eq!(&b[..8], &[0, 3, 0, 1, 0, 0, 0, 3]);
    }

    #[test]
    fn timestamps_and_dates() {
        let us = parse_timestamp_us("2026-11-11T01:15:00.123Z").unwrap();
        assert_eq!(format_timestamp_us(us, true), "2026-11-11T01:15:00.123Z");
        assert_eq!(parse_timestamp_us("2026-11-11 03:15:00.123+02").unwrap(), us);
        assert_eq!(
            parse_timestamp_us("2026-11-11T01:15:00.123456+00:00").unwrap(),
            us + 456
        );
        assert_eq!(parse_timestamp_us("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(format_timestamp_us(-1000, true), "1969-12-31T23:59:59.999Z");
        assert!(parse_timestamp_us("yesterday").is_none());
        assert_eq!(parse_date_days("2000-01-01"), Some(PG_EPOCH_UNIX_DAYS));
        assert_eq!(civil_from_days(PG_EPOCH_UNIX_DAYS), (2000, 1, 1));
    }

    #[test]
    fn json_is_compacted_without_reordering() {
        assert_eq!(
            compact_json(r#"{"b": 1, "a": [1, 2], "s": "x y, z"}"#),
            r#"{"b":1,"a":[1,2],"s":"x y, z"}"#
        );
        assert_eq!(
            compact_json(r#"{"q": "say \"hi\" , ok"}"#),
            r#"{"q":"say \"hi\" , ok"}"#
        );
    }

    #[test]
    fn scalar_conversions_are_lossless_or_refused() {
        assert_eq!(encode_scalar(&Value::Integer(1), Target::Bool).unwrap(), vec![1]);
        assert!(encode_scalar(&Value::Integer(2), Target::Bool).is_err());
        assert!(encode_scalar(&Value::Integer(70_000), Target::Int2).is_err());
        assert_eq!(
            encode_scalar(&Value::Text("42".into()), Target::Int8).unwrap(),
            42i64.to_be_bytes()
        );
        assert!(encode_scalar(&Value::Text("4x".into()), Target::Int8).is_err());
        assert!(encode_scalar(&Value::Real(1.5), Target::Int8).is_err());
        assert_eq!(encode_scalar(&Value::Real(3.0), Target::Text).unwrap(), b"3.0");
        assert!(encode_scalar(&Value::Blob(vec![1]), Target::Text).is_err());
        assert_eq!(
            encode_scalar(&Value::Text("{}".into()), Target::Jsonb).unwrap(),
            b"\x01{}"
        );
        assert!(encode_scalar(&Value::Text("nope".into()), Target::Uuid).is_err());
    }
}
