//! Plan-based API rate limits, enforced in the HTTP layer as a
//! [`RequestGate`] on `/api/v1/*`.
//!
//! A feature of kind `rate` with a limit and a period of `hour` or `day` in
//! the subject's grants caps the subject's requests in that fixed UTC
//! window (several rate features: all apply). Subjects whose plan does not
//! grant the feature are not limited by it. Privileged and anonymous callers
//! are not limited here (FB-20 rate-limits authentication per IP). Counters
//! live in memory: they are a rate limit, not billed usage, and restart
//! with the process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderName, HeaderValue, StatusCode};
use forgebase_api::{ApiError, GateDecision, RequestGate};
use forgebase_data::{AccessContext, Principal};
use serde_json::json;

use crate::model::FeatureKind;
use crate::period::{self, Period};
use crate::service::{EntitlementService, Shared, subject_key};

const MAX_KEYS: usize = 100_000;

/// Request counts per (feature, subject) in the current window.
#[derive(Debug, Default)]
pub(crate) struct RateCounters {
    map: Mutex<HashMap<(String, String), (i64, i64)>>,
}

impl RateCounters {
    /// Counts one request unless it would pass `limit`; returns the count
    /// after it, or `None` when the limit is reached.
    fn hit(&self, feature: &str, subject: &str, window_start: i64, limit: i64) -> Option<i64> {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= MAX_KEYS {
            map.retain(|_, (start, _)| *start >= window_start);
            if map.len() >= MAX_KEYS {
                map.clear();
            }
        }
        let e = map
            .entry((feature.to_owned(), subject.to_owned()))
            .or_insert((window_start, 0));
        if e.0 != window_start {
            *e = (window_start, 0);
        }
        if e.1 >= limit {
            return None;
        }
        e.1 += 1;
        Some(e.1)
    }

    fn undo(&self, feature: &str, subject: &str, window_start: i64) {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(e) = map.get_mut(&(feature.to_owned(), subject.to_owned()))
            && e.0 == window_start
            && e.1 > 0
        {
            e.1 -= 1;
        }
    }
}

/// The gate.
#[derive(Debug, Clone)]
pub struct RateGate {
    shared: Arc<Shared>,
}

impl EntitlementService {
    /// The plan-based rate-limit gate for `RouterOptions::gates`.
    pub fn rate_gate(&self) -> Arc<dyn RequestGate> {
        Arc::new(RateGate {
            shared: self.shared().clone(),
        })
    }
}

fn header(name: &'static str, v: i64) -> Option<(HeaderName, HeaderValue)> {
    HeaderValue::from_str(&v.to_string())
        .ok()
        .map(|v| (HeaderName::from_static(name), v))
}

impl RequestGate for RateGate {
    fn admit(&self, ctx: &AccessContext, path: &str) -> GateDecision {
        if !path.starts_with("/api/v1/")
            || ctx.is_privileged()
            || matches!(ctx.principal, Principal::Anonymous | Principal::System)
        {
            return GateDecision::Pass(Vec::new());
        }
        let cat = self.shared.catalog();
        if cat.rate_features.is_empty() {
            return GateDecision::Pass(Vec::new());
        }
        let now = self.shared.now();
        let subject = subject_key(&ctx.subject());
        let member = ctx.principal_key();
        let r = cat.resolve(&subject, Some(&member), now);
        // Most restrictive window for the headers: (limit, remaining, reset).
        let mut shown: Option<(i64, i64, i64)> = None;
        let mut counted: Vec<(String, i64)> = Vec::new();
        for f in &cat.rate_features {
            let Some(g) = r.grant(f).filter(|g| g.enabled && g.kind == FeatureKind::Rate) else {
                continue;
            };
            let Some(limit) = g.limit else { continue };
            let p = if g.period == Period::Day {
                Period::Day
            } else {
                Period::Hour
            };
            let w = period::window(p, None, None, now);
            let reset_secs = ((w.end - now) + 999) / 1000;
            match self.shared.rates.hit(f, &subject, w.start, limit) {
                Some(n) => {
                    counted.push((f.clone(), w.start));
                    let remaining = limit - n;
                    if shown.is_none_or(|(_, rem, _)| remaining < rem) {
                        shown = Some((limit, remaining, reset_secs));
                    }
                }
                None => {
                    // Requests refused by one window do not count in the others.
                    for (cf, start) in &counted {
                        self.shared.rates.undo(cf, &subject, *start);
                    }
                    let err = ApiError::new(
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate_limited",
                        format!("the plan allows {limit} API requests per {}", p.as_str()),
                    )
                    .with_details(json!({"feature": f, "limit": limit, "period": p, "retry_after": reset_secs}));
                    let headers = [
                        Some((
                            axum::http::header::RETRY_AFTER,
                            HeaderValue::from_str(&reset_secs.to_string()).unwrap_or(HeaderValue::from_static("1")),
                        )),
                        header("x-ratelimit-limit", limit),
                        header("x-ratelimit-remaining", 0),
                        header("x-ratelimit-reset", reset_secs),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    return GateDecision::Reject(err, headers);
                }
            }
        }
        let headers = match shown {
            Some((limit, remaining, reset)) => [
                header("x-ratelimit-limit", limit),
                header("x-ratelimit-remaining", remaining),
                header("x-ratelimit-reset", reset),
            ]
            .into_iter()
            .flatten()
            .collect(),
            None => Vec::new(),
        };
        GateDecision::Pass(headers)
    }
}
