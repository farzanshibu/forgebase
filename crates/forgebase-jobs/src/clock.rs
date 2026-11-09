//! The time source of the job queue.
//!
//! Every timestamp the queue writes or compares (run-at times, leases,
//! heartbeats, backoff, cron firings) comes from a [`Clock`], so tests can
//! drive time by hand with [`ManualClock`] instead of sleeping.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A source of the current time in Unix epoch milliseconds.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Current time in Unix epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// A clock that only moves when told to. Cheap to clone; clones share the time.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<AtomicI64>,
}

impl ManualClock {
    /// A manual clock showing `start_ms`.
    pub fn new(start_ms: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(start_ms)),
        }
    }

    /// A manual clock starting at the current wall-clock time.
    pub fn from_system() -> Self {
        Self::new(SystemClock.now_ms())
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        self.now
            .fetch_add(i64::try_from(by.as_millis()).unwrap_or(i64::MAX), Ordering::SeqCst);
    }

    /// Sets the clock to an absolute time.
    pub fn set(&self, ms: i64) {
        self.now.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}
