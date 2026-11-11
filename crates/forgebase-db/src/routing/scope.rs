//! The read-your-writes state of one request (ADR-0029 §2.6).
//!
//! The API's consistency layer runs each request inside
//! [`scoped`] with a [`RequestScope`] that holds:
//!
//! * the request's consistency (`X-ForgeBase-Consistency`);
//! * the commits it must see (`X-ForgeBase-Read-After` plus the caller's
//!   recent commits the server remembers);
//! * the commits it makes: every commit through a routed set
//!   ([`crate::SetHandle`] with replicas) records its token here, the layer
//!   answers it as `X-ForgeBase-Commit`, and later reads of the same request
//!   see the write.
//!
//! The scope is a Tokio task-local: work spawned onto other tasks does not
//! see it (its reads use the intent alone, its commits are not answered).

use std::future::Future;
use std::sync::{Arc, Mutex};

use super::{Consistency, Lsn, ReadAfter};

tokio::task_local! {
    static SCOPE: Arc<RequestScope>;
}

/// Read-your-writes state of one request. Shared by reference.
#[derive(Debug, Default)]
pub struct RequestScope {
    consistency: Consistency,
    read_after: Mutex<ReadAfter>,
    commits: Mutex<ReadAfter>,
}

impl RequestScope {
    /// A scope with `consistency`, needing `read_after`.
    pub fn new(consistency: Consistency, read_after: ReadAfter) -> Arc<Self> {
        Arc::new(Self {
            consistency,
            read_after: Mutex::new(read_after),
            commits: Mutex::new(ReadAfter::new()),
        })
    }

    /// The request's consistency.
    pub fn consistency(&self) -> Consistency {
        self.consistency
    }

    /// Commits the request's reads must see (its own included).
    pub fn read_after(&self) -> ReadAfter {
        self.read_after.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The position the request's reads of `set` must see.
    pub fn read_after_of(&self, set: &str) -> Option<Lsn> {
        self.read_after.lock().unwrap_or_else(|p| p.into_inner()).get(set)
    }

    /// Commits made during the request (highest per set).
    pub fn commits(&self) -> ReadAfter {
        self.commits.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Records a commit on `set` at `lsn`: answered to the client, and seen
    /// by the request's later reads.
    pub fn record_commit(&self, set: &str, lsn: Lsn) {
        self.commits.lock().unwrap_or_else(|p| p.into_inner()).note(set, lsn);
        self.read_after.lock().unwrap_or_else(|p| p.into_inner()).note(set, lsn);
    }
}

/// Runs `f` with `scope` as the current request scope.
pub async fn scoped<F: Future>(scope: Arc<RequestScope>, f: F) -> F::Output {
    SCOPE.scope(scope, f).await
}

/// The current request scope, if any.
pub fn current() -> Option<Arc<RequestScope>> {
    SCOPE.try_with(Arc::clone).ok()
}

/// Records a commit in the current request scope (no-op outside one).
pub fn record_commit(set: &str, lsn: Lsn) {
    let _ = SCOPE.try_with(|s| s.record_commit(set, lsn));
}

/// Runs `f` with every routed read on the primary (`Strong`), for reads
/// that must see a row another request committed moments ago and that no
/// token covers: identity lookups right after a signup, a password reset,
/// a verification (FIX-15). Commits made inside are still recorded in the
/// enclosing scope, so the response keeps its `X-ForgeBase-Commit`.
pub async fn on_primary<F: Future>(f: F) -> F::Output {
    let outer = current();
    let read_after = outer.as_ref().map(|s| s.read_after()).unwrap_or_default();
    let inner = RequestScope::new(Consistency::Strong, read_after);
    let out = scoped(Arc::clone(&inner), f).await;
    if let Some(outer) = outer {
        let commits = inner.commits();
        outer.commits.lock().unwrap_or_else(|p| p.into_inner()).merge(&commits);
        outer.read_after.lock().unwrap_or_else(|p| p.into_inner()).merge(&commits);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn commits_are_seen_inside_the_scope_only() {
        record_commit("control", Lsn(5));
        assert!(current().is_none());
        let scope = RequestScope::new(Consistency::Strong, ReadAfter::parse_header("control/0/3"));
        let seen = scoped(scope.clone(), async {
            record_commit("control", Lsn(9));
            record_commit("s1", Lsn(2));
            current().map(|s| (s.consistency(), s.read_after_of("control")))
        })
        .await;
        assert_eq!(seen, Some((Consistency::Strong, Some(Lsn(9)))));
        assert_eq!(scope.commits().to_header().as_deref(), Some("control/0/9, s1/0/2"));
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn on_primary_reads_strong_and_keeps_the_commits() {
        let scope = RequestScope::new(Consistency::Session, ReadAfter::parse_header("control/0/3"));
        let seen = scoped(scope.clone(), async {
            on_primary(async {
                record_commit("control", Lsn(7));
                current().map(|s| (s.consistency(), s.read_after_of("control")))
            })
            .await
        })
        .await;
        assert_eq!(seen, Some((Consistency::Strong, Some(Lsn(7)))));
        assert_eq!(scope.commits().to_header().as_deref(), Some("control/0/7"));
        assert_eq!(scope.read_after_of("control"), Some(Lsn(7)));
        // Outside any scope it still runs (no commits to keep).
        assert_eq!(on_primary(async { current().map(|s| s.consistency()) }).await, Some(Consistency::Strong));
    }
}
