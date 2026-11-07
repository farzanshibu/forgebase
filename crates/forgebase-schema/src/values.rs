//! Record value validation and normalisation.
//!
//! [`normalize_value`] turns a client-supplied JSON value into the canonical
//! JSON form of a field (dates in UTC, integers as integers, geo points with
//! sorted keys, ...) or explains what is wrong with it. `null` always
//! normalises to `null`; whether that is allowed (required fields) is the
//! caller's decision, see [`is_blank`].

use std::collections::{BTreeMap, HashSet};

use forgebase_core::{ErrorCode, ErrorKind};
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::model::{ArrayItems, Field, FieldKind};

/// Default maximum text length in characters.
pub const DEFAULT_MAX_TEXT: u32 = 1_000_000;
/// Default maximum JSON size in bytes.
pub const DEFAULT_MAX_JSON: u32 = 1 << 20;

/// Why one value was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    /// Stable code (`required`, `invalid_type`, `too_long`, ...).
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

impl FieldError {
    /// A new field error.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Field-level validation failures of one record, keyed by field name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("validation failed: {}", summary(.fields))]
pub struct ValidationErrors {
    /// Problems by field name.
    pub fields: BTreeMap<String, FieldError>,
}

fn summary(fields: &BTreeMap<String, FieldError>) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}: {}", v.message))
        .collect::<Vec<_>>()
        .join("; ")
}

impl ValidationErrors {
    /// Records a problem.
    pub fn add(&mut self, field: impl Into<String>, error: FieldError) {
        self.fields.entry(field.into()).or_insert(error);
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// A single-field failure.
    pub fn single(field: impl Into<String>, error: FieldError) -> Self {
        let mut e = Self::default();
        e.add(field, error);
        e
    }
}

impl ErrorCode for ValidationErrors {
    fn code(&self) -> &'static str {
        "validation_failed"
    }
    fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}

/// Whether a normalised value counts as missing for `required` (null, empty
/// string, empty array).
pub fn is_blank(value: &Json) -> bool {
    match value {
        Json::Null => true,
        Json::String(s) => s.is_empty(),
        Json::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn type_err(expected: &str) -> FieldError {
    FieldError::new("invalid_type", format!("expected {expected}"))
}

/// Validates `value` for `field` and returns its canonical form.
pub fn normalize_value(field: &Field, value: &Json) -> Result<Json, FieldError> {
    if value.is_null() {
        return Ok(Json::Null);
    }
    match &field.kind {
        FieldKind::Text { min_length, max_length } => {
            let s = value.as_str().ok_or_else(|| type_err("a string"))?;
            check_len(s, *min_length, Some(max_length.unwrap_or(DEFAULT_MAX_TEXT)))?;
            Ok(Json::String(s.to_owned()))
        }
        FieldKind::Password { min_length } => {
            let s = value.as_str().ok_or_else(|| type_err("a string"))?;
            check_len(s, Some(*min_length), Some(256))?;
            Ok(Json::String(s.to_owned()))
        }
        FieldKind::Number { min, max, integer } => {
            let n = value
                .as_f64()
                .filter(|f| f.is_finite())
                .ok_or_else(|| type_err("a finite number"))?;
            if *integer && n.fract() != 0.0 {
                return Err(FieldError::new("not_integer", "expected an integer"));
            }
            if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                return Err(FieldError::new(
                    "out_of_range",
                    format!("must be between {} and {}", fmt_bound(*min), fmt_bound(*max)),
                ));
            }
            if *integer {
                if n.abs() > 9_007_199_254_740_992.0 {
                    return Err(FieldError::new("out_of_range", "integer is too large"));
                }
                Ok(Json::from(value.as_i64().unwrap_or(n as i64)))
            } else {
                Ok(serde_json::Number::from_f64(n).map(Json::Number).unwrap_or(Json::Null))
            }
        }
        FieldKind::Bool => value.as_bool().map(Json::Bool).ok_or_else(|| type_err("true or false")),
        FieldKind::Date => {
            let s = value.as_str().ok_or_else(|| type_err("a date string"))?;
            normalize_date(s)
                .map(Json::String)
                .ok_or_else(|| FieldError::new("invalid_date", "expected an RFC 3339 date-time or YYYY-MM-DD"))
        }
        FieldKind::Email => {
            let s = value.as_str().ok_or_else(|| type_err("a string"))?;
            if !is_email(s) {
                return Err(FieldError::new("invalid_email", "not a valid e-mail address"));
            }
            Ok(Json::String(s.to_owned()))
        }
        FieldKind::Url => {
            let s = value.as_str().ok_or_else(|| type_err("a string"))?;
            match url::Url::parse(s) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() && s.len() <= 2048 => {
                    Ok(Json::String(s.to_owned()))
                }
                _ => Err(FieldError::new("invalid_url", "expected an http or https URL")),
            }
        }
        FieldKind::Select { values, max_select } => {
            let check = |v: &Json| -> Result<String, FieldError> {
                let s = v.as_str().ok_or_else(|| type_err("a string"))?;
                if values.iter().any(|allowed| allowed == s) {
                    Ok(s.to_owned())
                } else {
                    Err(FieldError::new("invalid_choice", format!("must be one of {values:?}")))
                }
            };
            if *max_select > 1 {
                let items = as_unique_list(value, *max_select)?;
                Ok(Json::Array(
                    items
                        .iter()
                        .map(check)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .map(Json::String)
                        .collect(),
                ))
            } else {
                Ok(Json::String(check(value)?))
            }
        }
        FieldKind::Json { max_size } => {
            let size = serde_json::to_vec(value).map(|v| v.len()).unwrap_or(usize::MAX);
            if size > max_size.unwrap_or(DEFAULT_MAX_JSON) as usize {
                return Err(FieldError::new("too_large", "JSON value is too large"));
            }
            Ok(value.clone())
        }
        FieldKind::Array {
            items,
            min_items,
            max_items,
        } => {
            let arr = value.as_array().ok_or_else(|| type_err("an array"))?;
            if min_items.is_some_and(|m| arr.len() < m as usize) {
                return Err(FieldError::new(
                    "too_short",
                    format!("needs at least {} items", min_items.unwrap_or(0)),
                ));
            }
            if max_items.is_some_and(|m| arr.len() > m as usize) {
                return Err(FieldError::new(
                    "too_many",
                    format!("allows at most {} items", max_items.unwrap_or(0)),
                ));
            }
            for item in arr {
                let ok = match items {
                    ArrayItems::Text => item.is_string(),
                    ArrayItems::Number => item.as_f64().is_some_and(f64::is_finite),
                    ArrayItems::Bool => item.is_boolean(),
                    ArrayItems::Json => true,
                };
                if !ok {
                    return Err(type_err(&format!("an array of {items:?}").to_lowercase()));
                }
            }
            if serde_json::to_vec(value).map(|v| v.len()).unwrap_or(usize::MAX) > DEFAULT_MAX_JSON as usize {
                return Err(FieldError::new("too_large", "array is too large"));
            }
            Ok(value.clone())
        }
        FieldKind::Relation { max_select, .. } => {
            let check = |v: &Json| -> Result<String, FieldError> {
                let s = v.as_str().ok_or_else(|| type_err("a record id"))?;
                if crate::is_valid_id(s) {
                    Ok(s.to_owned())
                } else {
                    Err(FieldError::new("invalid_id", "not a valid record id"))
                }
            };
            if *max_select > 1 {
                let items = as_unique_list(value, *max_select)?;
                Ok(Json::Array(
                    items
                        .iter()
                        .map(check)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .map(Json::String)
                        .collect(),
                ))
            } else {
                Ok(Json::String(check(value)?))
            }
        }
        FieldKind::File { max_select } => {
            let check = |v: &Json| -> Result<String, FieldError> {
                let s = v.as_str().ok_or_else(|| type_err("a file name"))?;
                if s.is_empty() || s.len() > 255 || s.contains(['/', '\\', '\0']) || s == "." || s == ".." {
                    Err(FieldError::new("invalid_file", "not a valid file name"))
                } else {
                    Ok(s.to_owned())
                }
            };
            if *max_select > 1 {
                let items = as_unique_list(value, *max_select)?;
                Ok(Json::Array(
                    items
                        .iter()
                        .map(check)
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .map(Json::String)
                        .collect(),
                ))
            } else {
                Ok(Json::String(check(value)?))
            }
        }
        FieldKind::Geo => {
            let obj = value.as_object().ok_or_else(|| type_err("an object {lon, lat}"))?;
            let lon = obj
                .get("lon")
                .and_then(Json::as_f64)
                .filter(|v| (-180.0..=180.0).contains(v));
            let lat = obj
                .get("lat")
                .and_then(Json::as_f64)
                .filter(|v| (-90.0..=90.0).contains(v));
            match (lon, lat, obj.len()) {
                (Some(lon), Some(lat), 2) => {
                    let mut m = Map::new();
                    m.insert("lat".into(), Json::from(lat));
                    m.insert("lon".into(), Json::from(lon));
                    Ok(Json::Object(m))
                }
                _ => Err(FieldError::new(
                    "invalid_geo",
                    "expected {\"lon\": -180..180, \"lat\": -90..90}",
                )),
            }
        }
        FieldKind::Vector { dimensions } => {
            let arr = value.as_array().ok_or_else(|| type_err("an array of numbers"))?;
            if arr.len() != *dimensions as usize {
                return Err(FieldError::new(
                    "dimension_mismatch",
                    format!("expected {dimensions} dimensions, got {}", arr.len()),
                ));
            }
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                let f = v
                    .as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(|| FieldError::new("not_finite", "vector components must be finite numbers"))?;
                out.push(Json::from(f));
            }
            Ok(Json::Array(out))
        }
    }
}

fn fmt_bound(b: Option<f64>) -> String {
    b.map_or_else(|| "unbounded".to_owned(), |v| v.to_string())
}

fn check_len(s: &str, min: Option<u32>, max: Option<u32>) -> Result<(), FieldError> {
    let n = s.chars().count();
    if let Some(m) = min
        && n < m as usize
    {
        return Err(FieldError::new("too_short", format!("must be at least {m} characters")));
    }
    if let Some(m) = max
        && n > m as usize
    {
        return Err(FieldError::new("too_long", format!("must be at most {m} characters")));
    }
    Ok(())
}

/// Accepts a single value or an array; rejects duplicates and too many items.
fn as_unique_list(value: &Json, max: u32) -> Result<Vec<Json>, FieldError> {
    let items = match value {
        Json::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    if items.len() > max as usize {
        return Err(FieldError::new("too_many", format!("allows at most {max} values")));
    }
    let mut seen = HashSet::new();
    if !items.iter().all(|i| seen.insert(i.to_string())) {
        return Err(FieldError::new("duplicate", "values must be unique"));
    }
    Ok(items)
}

fn is_email(s: &str) -> bool {
    if s.len() > 254 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !local.contains('@')
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_alphanumeric() || c == '-')
        })
}

/// Parses an RFC 3339 date-time, `YYYY-MM-DD HH:MM:SS[.fff]Z` or a plain
/// `YYYY-MM-DD` (midnight UTC) and returns the canonical UTC timestamp.
pub fn normalize_date(s: &str) -> Option<String> {
    use time::format_description::well_known::Rfc3339;
    let s = s.trim();
    if let Ok(t) = time::OffsetDateTime::parse(s, &Rfc3339) {
        return Some(crate::format_timestamp(t));
    }
    if s.len() > 10 && s.as_bytes().get(10) == Some(&b' ') {
        let replaced = format!("{}T{}", &s[..10], &s[11..]);
        if let Ok(t) = time::OffsetDateTime::parse(&replaced, &Rfc3339) {
            return Some(crate::format_timestamp(t));
        }
    }
    let b = s.as_bytes();
    if b.len() == 10 && b[4] == b'-' && b[7] == b'-' {
        let num = |r: std::ops::Range<usize>| {
            s.get(r)
                .filter(|p| p.bytes().all(|c| c.is_ascii_digit()))?
                .parse::<i32>()
                .ok()
        };
        let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
        let month = time::Month::try_from(u8::try_from(m).ok()?).ok()?;
        let date = time::Date::from_calendar_date(y, month, u8::try_from(d).ok()?).ok()?;
        return Some(crate::format_timestamp(date.midnight().assume_utc()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn f(kind: FieldKind) -> Field {
        Field::new("x", kind)
    }

    #[test]
    fn numbers() {
        let int = f(FieldKind::Number {
            min: Some(0.0),
            max: Some(100.0),
            integer: true,
        });
        assert_eq!(normalize_value(&int, &json!(5.0)).unwrap(), json!(5));
        assert_eq!(normalize_value(&int, &json!(5.5)).unwrap_err().code, "not_integer");
        assert_eq!(normalize_value(&int, &json!(101)).unwrap_err().code, "out_of_range");
        assert_eq!(normalize_value(&int, &json!("5")).unwrap_err().code, "invalid_type");
        assert_eq!(normalize_value(&int, &Json::Null).unwrap(), Json::Null);
    }

    #[test]
    fn dates_are_canonical_utc() {
        assert_eq!(
            normalize_date("2026-11-07T10:00:00+05:30").unwrap(),
            "2026-11-07T04:30:00.000Z"
        );
        assert_eq!(
            normalize_date("2026-11-07 10:00:00.5Z").unwrap(),
            "2026-11-07T10:00:00.500Z"
        );
        assert_eq!(normalize_date("2026-11-07").unwrap(), "2026-11-07T00:00:00.000Z");
        assert!(normalize_date("yesterday").is_none());
        assert!(normalize_date("2026-13-01").is_none());
    }

    #[test]
    fn strings_emails_urls() {
        let email = f(FieldKind::Email);
        assert!(normalize_value(&email, &json!("a@example.com")).is_ok());
        for bad in ["a@b", "@x.com", "a b@x.com", "a@-x.com", "x"] {
            assert!(normalize_value(&email, &json!(bad)).is_err(), "{bad}");
        }
        let url = f(FieldKind::Url);
        assert!(normalize_value(&url, &json!("https://example.com/x?y=1")).is_ok());
        assert!(normalize_value(&url, &json!("javascript:alert(1)")).is_err());
        assert!(normalize_value(&url, &json!("file:///etc/passwd")).is_err());
        let text = f(FieldKind::Text {
            min_length: Some(2),
            max_length: Some(3),
        });
        assert_eq!(normalize_value(&text, &json!("a")).unwrap_err().code, "too_short");
        assert_eq!(normalize_value(&text, &json!("abcd")).unwrap_err().code, "too_long");
        assert_eq!(normalize_value(&text, &json!("äöü")).unwrap(), json!("äöü"));
    }

    #[test]
    fn selects_relations_files() {
        let one = f(FieldKind::Select {
            values: vec!["a".into(), "b".into(), "c".into()],
            max_select: 1,
        });
        assert_eq!(normalize_value(&one, &json!("a")).unwrap(), json!("a"));
        assert_eq!(normalize_value(&one, &json!("z")).unwrap_err().code, "invalid_choice");
        let many = f(FieldKind::Select {
            values: vec!["a".into(), "b".into(), "c".into()],
            max_select: 2,
        });
        assert_eq!(normalize_value(&many, &json!("a")).unwrap(), json!(["a"]));
        assert_eq!(
            normalize_value(&many, &json!(["a", "a"])).unwrap_err().code,
            "duplicate"
        );
        assert_eq!(
            normalize_value(&many, &json!(["a", "b", "c"])).unwrap_err().code,
            "too_many"
        );
        let rel = f(FieldKind::Relation {
            collection_id: "c".into(),
            max_select: 1,
            cascade_delete: false,
        });
        assert_eq!(
            normalize_value(&rel, &json!("x' OR 1=1")).unwrap_err().code,
            "invalid_id"
        );
        let file = f(FieldKind::File { max_select: 1 });
        assert!(normalize_value(&file, &json!("../etc/passwd")).is_err());
    }

    #[test]
    fn geo_vector_array() {
        let geo = f(FieldKind::Geo);
        assert_eq!(
            normalize_value(&geo, &json!({"lon": 10, "lat": 20})).unwrap(),
            json!({"lat": 20.0, "lon": 10.0})
        );
        assert!(normalize_value(&geo, &json!({"lon": 200, "lat": 0})).is_err());
        let vec3 = f(FieldKind::Vector { dimensions: 3 });
        assert!(normalize_value(&vec3, &json!([1, 2, 3])).is_ok());
        assert_eq!(
            normalize_value(&vec3, &json!([1, 2])).unwrap_err().code,
            "dimension_mismatch"
        );
        let arr = f(FieldKind::Array {
            items: ArrayItems::Number,
            min_items: None,
            max_items: Some(2),
        });
        assert!(normalize_value(&arr, &json!([1, 2])).is_ok());
        assert!(normalize_value(&arr, &json!([1, "x"])).is_err());
        assert!(normalize_value(&arr, &json!([1, 2, 3])).is_err());
    }
}
