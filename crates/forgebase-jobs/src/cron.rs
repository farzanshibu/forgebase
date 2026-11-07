//! Cron expressions (UTC).
//!
//! Five fields: `minute hour day-of-month month day-of-week`. A sixth field in
//! front adds seconds (`second minute hour dom month dow`). Each field takes
//! `*`, `a`, `a-b`, `*/n`, `a/n`, `a-b/n` and comma lists; months accept
//! `JAN`..`DEC`, weekdays `SUN`..`SAT` (`0` and `7` are Sunday). When both
//! day-of-month and day-of-week are restricted a day matches if either does
//! (classic cron). Everything is UTC.

use std::fmt;

use time::{Date, Duration, Month, OffsetDateTime, Time};

/// A parsed cron expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    seconds: u64,
    minutes: u64,
    hours: u64,
    dom: u64,
    months: u64,
    dow: u64,
    dom_any: bool,
    dow_any: bool,
}

/// A cron expression that does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronError(pub String);

impl fmt::Display for CronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid cron expression: {}", self.0)
    }
}

impl std::error::Error for CronError {}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

fn parse_value(token: &str, names: &[&str], name_base: u32) -> Result<u32, CronError> {
    if let Ok(n) = token.parse::<u32>() {
        return Ok(n);
    }
    let upper = token.to_ascii_uppercase();
    names
        .iter()
        .position(|n| *n == upper)
        .map(|i| i as u32 + name_base)
        .ok_or_else(|| CronError(format!("{token:?} is not a number or name")))
}

/// Parses one field into a bit set over `min..=max`.
fn parse_field(text: &str, min: u32, max: u32, names: &[&str], name_base: u32) -> Result<(u64, bool), CronError> {
    let mut bits = 0u64;
    let mut any = false;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| CronError(format!("bad step {s:?} in {part:?}")))?;
                if step == 0 {
                    return Err(CronError(format!("step is 0 in {part:?}")));
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" || range == "?" {
            if step == 1 && part.len() == 1 {
                any = true;
            }
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (parse_value(a, names, name_base)?, parse_value(b, names, name_base)?)
        } else {
            let v = parse_value(range, names, name_base)?;
            // `a/n` means "from a to the end, every n".
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return Err(CronError(format!("{part:?} is outside {min}-{max}")));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    /// Parses a 5- or 6-field expression.
    pub fn parse(text: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let (sec, rest) = match fields.len() {
            5 => ("0", &fields[..]),
            6 => (fields[0], &fields[1..]),
            n => return Err(CronError(format!("expected 5 or 6 fields, found {n}"))),
        };
        let (seconds, _) = parse_field(sec, 0, 59, &[], 0)?;
        let (minutes, _) = parse_field(rest[0], 0, 59, &[], 0)?;
        let (hours, _) = parse_field(rest[1], 0, 23, &[], 0)?;
        let (dom, dom_any) = parse_field(rest[2], 1, 31, &[], 0)?;
        let (months, _) = parse_field(rest[3], 1, 12, &MONTHS, 1)?;
        let (mut dow, dow_any) = parse_field(rest[4], 0, 7, &DAYS, 0)?;
        // 7 is Sunday too.
        if dow & (1 << 7) != 0 {
            dow = (dow & !(1 << 7)) | 1;
        }
        Ok(Self {
            seconds,
            minutes,
            hours,
            dom,
            months,
            dow,
            dom_any,
            dow_any,
        })
    }

    fn day_matches(&self, date: Date) -> bool {
        let dom = self.dom & (1 << date.day()) != 0;
        let dow = self.dow & (1 << date.weekday().number_days_from_sunday()) != 0;
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }

    /// The first matching instant strictly after `after_ms` (Unix epoch
    /// milliseconds), in epoch milliseconds. `None` if nothing matches within
    /// eight years (e.g. February 30th).
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        let start = OffsetDateTime::from_unix_timestamp(after_ms.div_euclid(1000) + 1).ok()?;
        let limit = start + Duration::days(366 * 8);
        let mut t = start;
        while t <= limit {
            let midnight = |d: Date| d.with_time(Time::MIDNIGHT).assume_utc();
            if self.months & (1 << u8::from(t.month())) == 0 {
                let (y, m) = if t.month() == Month::December {
                    (t.year() + 1, Month::January)
                } else {
                    (t.year(), t.month().next())
                };
                t = midnight(Date::from_calendar_date(y, m, 1).ok()?);
            } else if !self.day_matches(t.date()) {
                t = midnight(t.date().next_day()?);
            } else if self.hours & (1 << t.hour()) == 0 {
                t = t.replace_time(Time::from_hms(t.hour(), 0, 0).ok()?) + Duration::hours(1);
            } else if self.minutes & (1 << t.minute()) == 0 {
                t = t.replace_time(Time::from_hms(t.hour(), t.minute(), 0).ok()?) + Duration::minutes(1);
            } else if self.seconds & (1 << t.second()) == 0 {
                t += Duration::seconds(1);
            } else {
                return Some(t.unix_timestamp() * 1000);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(rfc: &str) -> i64 {
        (OffsetDateTime::parse(rfc, &time::format_description::well_known::Rfc3339)
            .unwrap()
            .unix_timestamp())
            * 1000
    }

    fn next(expr: &str, after: &str) -> String {
        let t = Cron::parse(expr).unwrap().next_after(ms(after)).unwrap();
        crate::model::iso(t)
    }

    #[test]
    fn common_expressions() {
        assert_eq!(next("* * * * *", "2026-11-07T10:00:00Z"), "2026-11-07T10:01:00.000Z");
        assert_eq!(next("*/15 * * * *", "2026-11-07T10:07:30Z"), "2026-11-07T10:15:00.000Z");
        assert_eq!(next("0 9 * * MON-FRI", "2026-11-07T10:00:00Z"), "2026-11-09T09:00:00.000Z");
        assert_eq!(next("30 2 1 JAN *", "2026-11-07T10:00:00Z"), "2027-01-01T02:30:00.000Z");
        assert_eq!(next("0 0 29 2 *", "2026-11-07T00:00:00Z"), "2028-02-29T00:00:00.000Z");
        assert_eq!(next("*/10 * * * * *", "2026-11-07T10:00:05Z"), "2026-11-07T10:00:10.000Z");
        assert_eq!(next("0 0 * * 7", "2026-11-07T10:00:00Z"), "2026-11-08T00:00:00.000Z");
        // Strictly after: an exact match does not return itself.
        assert_eq!(next("0 10 * * *", "2026-11-07T10:00:00Z"), "2026-11-08T10:00:00.000Z");
    }

    #[test]
    fn day_of_month_or_day_of_week() {
        // The 13th, or any Friday.
        assert_eq!(next("0 0 13 * FRI", "2026-11-07T10:00:00Z"), "2026-11-13T00:00:00.000Z");
        assert_eq!(next("0 0 13 * FRI", "2026-11-13T10:00:00Z"), "2026-11-20T00:00:00.000Z");
    }

    #[test]
    fn rejects_bad_expressions() {
        for bad in ["", "* * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "*/0 * * * *", "a b c d e", "5-1 * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad:?} should not parse");
        }
        assert_eq!(Cron::parse("0 0 30 2 *").unwrap().next_after(0), None);
    }
}
