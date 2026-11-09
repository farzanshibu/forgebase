//! The time source of the auth service.
//!
//! Token, session, link and lockout expiry all read a [`Clock`], so tests can
//! move time by hand with [`ManualClock`] instead of sleeping.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A source of the current time in Unix epoch seconds.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Current time in Unix epoch seconds.
    fn now(&self) -> i64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }
}

/// A clock that only moves when told to. Clones share the time.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<AtomicI64>,
}

impl ManualClock {
    /// A manual clock starting at the current wall-clock time.
    pub fn from_system() -> Self {
        Self {
            now: Arc::new(AtomicI64::new(SystemClock.now())),
        }
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        self.now
            .fetch_add(i64::try_from(by.as_secs()).unwrap_or(i64::MAX), Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}
