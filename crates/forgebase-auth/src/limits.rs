//! In-memory rate limiting per client IP.
//!
//! Fixed windows keyed by `(bucket, ip)`: cheap, lock held for a map lookup
//! only, and bounded (expired entries are swept when the map grows). Per-IP
//! limits reset on restart by design; per-account lockout is persisted in the
//! database (see `service::lockout`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SWEEP_AT: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

/// Counts events per key in fixed windows.
#[derive(Debug, Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<(String, String), Window>>,
}

impl RateLimiter {
    /// Empty limiter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts one event for `(bucket, key)`. Returns `Err(retry_after_secs)`
    /// when more than `limit` events happened in the current `window`.
    pub fn hit(&self, bucket: &str, key: &str, limit: u32, window: Duration) -> Result<(), u64> {
        if limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut map = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() >= SWEEP_AT {
            map.retain(|_, w| now.duration_since(w.start) < window.max(Duration::from_secs(3600)));
        }
        let w = map
            .entry((bucket.to_owned(), key.to_owned()))
            .or_insert(Window { start: now, count: 0 });
        if now.duration_since(w.start) >= window {
            *w = Window { start: now, count: 0 };
        }
        if w.count >= limit {
            let left = window.saturating_sub(now.duration_since(w.start));
            return Err(left.as_secs().max(1));
        }
        w.count += 1;
        Ok(())
    }

    /// Whether `(bucket, key)` is at or over `limit` without counting.
    pub fn exceeded(&self, bucket: &str, key: &str, limit: u32, window: Duration) -> Option<u64> {
        if limit == 0 {
            return None;
        }
        let now = Instant::now();
        let map = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let w = map.get(&(bucket.to_owned(), key.to_owned()))?;
        let elapsed = now.duration_since(w.start);
        (elapsed < window && w.count >= limit).then(|| window.saturating_sub(elapsed).as_secs().max(1))
    }

    /// Forgets `(bucket, key)` (e.g. after a successful login).
    pub fn reset(&self, bucket: &str, key: &str) {
        self.windows
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(bucket.to_owned(), key.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key_and_window() {
        let r = RateLimiter::new();
        for _ in 0..3 {
            assert!(r.hit("login", "1.2.3.4", 3, Duration::from_secs(60)).is_ok());
        }
        let retry = r.hit("login", "1.2.3.4", 3, Duration::from_secs(60)).unwrap_err();
        assert!((1..=60).contains(&retry));
        assert!(
            r.hit("login", "5.6.7.8", 3, Duration::from_secs(60)).is_ok(),
            "other ip"
        );
        assert!(
            r.hit("signup", "1.2.3.4", 3, Duration::from_secs(60)).is_ok(),
            "other bucket"
        );
        assert!(r.exceeded("login", "1.2.3.4", 3, Duration::from_secs(60)).is_some());
        r.reset("login", "1.2.3.4");
        assert!(r.hit("login", "1.2.3.4", 3, Duration::from_secs(60)).is_ok());
        // Zero-length window: never blocks for long.
        assert!(r.hit("x", "k", 1, Duration::ZERO).is_ok());
        assert!(r.hit("x", "k", 1, Duration::ZERO).is_ok());
    }
}
