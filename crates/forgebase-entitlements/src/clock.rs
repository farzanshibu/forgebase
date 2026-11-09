//! Injectable time (tests drive period resets without sleeping).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// A source of "now" in Unix epoch milliseconds.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A clock that only moves when told to (tests).
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    now: Arc<AtomicI64>,
}

impl ManualClock {
    /// A clock showing `ms`.
    pub fn at(ms: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(ms)),
        }
    }

    /// Sets the time.
    pub fn set(&self, ms: i64) {
        self.now.store(ms, Ordering::SeqCst);
    }

    /// Moves the time forward by `ms`.
    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}
