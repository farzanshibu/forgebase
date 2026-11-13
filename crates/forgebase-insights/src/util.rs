//! Small helpers: row access, time ranges and number rounding.

use forgebase_db::Value;

use crate::error::{InsightsError, Result};

pub(crate) fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or_default()
}

pub(crate) fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn opt_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// RFC 3339 text of Unix milliseconds.
pub fn iso(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map(forgebase_core::format_rfc3339)
        .unwrap_or_default()
}

/// Unix milliseconds of an RFC 3339 time, or of a `YYYY-MM-DD` date (UTC midnight).
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(t) = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339) {
        return i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok();
    }
    let d = parse_date(s)?;
    Some(d.midnight().assume_utc().unix_timestamp() * 1000)
}

/// A `YYYY-MM-DD` date.
pub fn parse_date(s: &str) -> Option<time::Date> {
    let mut it = s.trim().split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let month = time::Month::try_from(m.parse::<u8>().ok()?).ok()?;
    time::Date::from_calendar_date(y.parse().ok()?, month, d.parse().ok()?).ok()
}

/// `YYYY-MM-DD` of Unix milliseconds (UTC).
pub fn day_of(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000)).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

/// A half-open time range in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// At or after.
    pub from_ms: i64,
    /// Before.
    pub to_ms: i64,
}

impl Range {
    /// The range ending now and starting `default_secs` earlier, or the given bounds.
    ///
    /// # Errors
    /// `invalid_range` when `to` is not after `from`, or the range is longer than `max_days`.
    pub fn resolve(from: Option<i64>, to: Option<i64>, default_secs: i64, max_days: i64) -> Result<Self> {
        let to_ms = to.unwrap_or_else(|| now_ms() + 1);
        let from_ms = from.unwrap_or(to_ms - default_secs * 1000);
        if to_ms <= from_ms {
            return Err(InsightsError::invalid("invalid_range", "`to` must be after `from`"));
        }
        if to_ms - from_ms > max_days * 86_400_000 {
            return Err(InsightsError::invalid(
                "invalid_range",
                format!("the range is longer than {max_days} days"),
            ));
        }
        Ok(Self { from_ms, to_ms })
    }
}

/// `x` rounded to 3 decimals.
pub(crate) fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Microseconds as milliseconds, 3 decimals.
pub(crate) fn ms(us: u64) -> f64 {
    r3(us as f64 / 1000.0)
}

/// `LIKE` pattern matching `s` anywhere (case-insensitive when compared with `lower(col)`).
pub(crate) fn contains_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('%');
    for c in s.to_lowercase().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}
