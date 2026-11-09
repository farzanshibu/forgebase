//! Quota periods and their windows (UTC).
//!
//! * `hour`, `day`: fixed UTC windows.
//! * `month`: calendar months, or billing cycles when the subscription has a
//!   cycle anchor (the window starts on the anchor's day of month and time of
//!   day; in shorter months on the last day).
//! * `lifetime`: one window for ever.
//! * `rolling`: the last `window_secs` seconds (sliding).
//! * `none`: no window (booleans, gauges, concurrency).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Milliseconds per hour.
pub const HOUR_MS: i64 = 3_600_000;
/// Milliseconds per day.
pub const DAY_MS: i64 = 86_400_000;

/// How usage is counted over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    /// No window.
    #[default]
    None,
    /// Clock hours.
    Hour,
    /// UTC days.
    Day,
    /// Calendar months or billing cycles.
    Month,
    /// For ever.
    Lifetime,
    /// Sliding window of `window_secs`.
    Rolling,
}

impl Period {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Period::None => "none",
            Period::Hour => "hour",
            Period::Day => "day",
            Period::Month => "month",
            Period::Lifetime => "lifetime",
            Period::Rolling => "rolling",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Period::None,
            "hour" => Period::Hour,
            "day" => Period::Day,
            "month" => Period::Month,
            "lifetime" => Period::Lifetime,
            "rolling" => Period::Rolling,
            _ => return None,
        })
    }

    /// Calendar periods have a previous window to roll unused units over from.
    pub fn is_calendar(self) -> bool {
        matches!(self, Period::Hour | Period::Day | Period::Month)
    }
}

/// A half-open time window `[start, end)` in epoch milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// First millisecond.
    pub start: i64,
    /// First millisecond after the window.
    pub end: i64,
}

impl Default for Window {
    fn default() -> Self {
        Window { start: 0, end: i64::MAX }
    }
}

/// The window containing `now`. `rolling` gives `(now - window, now + 1)`;
/// `none` and `lifetime` give `[0, i64::MAX)`.
pub fn window(period: Period, window_secs: Option<i64>, anchor: Option<i64>, now: i64) -> Window {
    match period {
        Period::None | Period::Lifetime => Window { start: 0, end: i64::MAX },
        Period::Hour => fixed(HOUR_MS, now),
        Period::Day => fixed(DAY_MS, now),
        Period::Month => month(anchor, now),
        Period::Rolling => {
            let w = window_secs.unwrap_or(3600).max(1).saturating_mul(1000);
            Window {
                start: now.saturating_sub(w),
                end: now.saturating_add(1),
            }
        }
    }
}

/// The calendar window just before `w` (same period and anchor).
pub fn previous(period: Period, anchor: Option<i64>, w: Window) -> Option<Window> {
    if !period.is_calendar() {
        return None;
    }
    Some(window(period, None, anchor, w.start - 1))
}

fn fixed(len: i64, now: i64) -> Window {
    let start = now.div_euclid(len) * len;
    Window { start, end: start + len }
}

fn month(anchor: Option<i64>, now: i64) -> Window {
    let (day, tod) = match anchor {
        Some(a) => {
            let (_, _, d) = civil_from_days(a.div_euclid(DAY_MS));
            (d, a.rem_euclid(DAY_MS))
        }
        None => (1, 0),
    };
    let boundary = |y: i64, m: u32| -> i64 {
        let d = day.min(days_in_month(y, m));
        days_from_civil(y, m, d) * DAY_MS + tod
    };
    let (y, m, _) = civil_from_days(now.div_euclid(DAY_MS));
    let here = boundary(y, m);
    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    if now < here {
        Window {
            start: boundary(py, pm),
            end: here,
        }
    } else {
        Window {
            start: here,
            end: boundary(ny, nm),
        }
    }
}

/// Days in a month of the proleptic Gregorian calendar.
pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date of a day count since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Epoch milliseconds of a UTC date and time (tests and fixtures).
pub fn utc(y: i64, m: u32, d: u32, h: i64, min: i64) -> i64 {
    days_from_civil(y, m, d) * DAY_MS + h * HOUR_MS + min * 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for z in [-800_000, -1, 0, 1, 59, 60, 10_957, 20_000, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn fixed_windows() {
        let t = utc(2026, 11, 9, 13, 45);
        assert_eq!(
            window(Period::Hour, None, None, t),
            Window {
                start: utc(2026, 11, 9, 13, 0),
                end: utc(2026, 11, 9, 14, 0)
            }
        );
        assert_eq!(
            window(Period::Day, None, None, t),
            Window {
                start: utc(2026, 11, 9, 0, 0),
                end: utc(2026, 11, 10, 0, 0)
            }
        );
        assert_eq!(window(Period::Lifetime, None, None, t), Window { start: 0, end: i64::MAX });
        let r = window(Period::Rolling, Some(60), None, t);
        assert_eq!((r.start, r.end), (t - 60_000, t + 1));
    }

    #[test]
    fn calendar_and_anchored_months() {
        let t = utc(2026, 12, 31, 23, 59);
        assert_eq!(
            window(Period::Month, None, None, t),
            Window {
                start: utc(2026, 12, 1, 0, 0),
                end: utc(2027, 1, 1, 0, 0)
            }
        );
        // Billing cycle anchored on the 31st at 10:00: February clamps to the 28th.
        let anchor = Some(utc(2026, 1, 31, 10, 0));
        assert_eq!(
            window(Period::Month, None, anchor, utc(2027, 3, 1, 0, 0)),
            Window {
                start: utc(2027, 2, 28, 10, 0),
                end: utc(2027, 3, 31, 10, 0)
            }
        );
        assert_eq!(
            window(Period::Month, None, anchor, utc(2027, 2, 28, 9, 59)),
            Window {
                start: utc(2027, 1, 31, 10, 0),
                end: utc(2027, 2, 28, 10, 0)
            }
        );
        let w = window(Period::Month, None, anchor, utc(2028, 2, 29, 12, 0));
        assert_eq!(w.start, utc(2028, 2, 29, 10, 0), "leap year");
        let prev = previous(Period::Month, anchor, w).unwrap();
        assert_eq!(prev.end, w.start);
        assert_eq!(prev.start, utc(2028, 1, 31, 10, 0));
        assert!(previous(Period::Rolling, None, w).is_none());
    }
}
