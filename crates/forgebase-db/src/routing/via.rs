//! [`ReadVia`]: a [`Backend`] view whose plain reads go through
//! [`Backend::fetch_routed`] (ADR-0029 §2.5, FB-169).
//!
//! The vector and graph services pass a `&dyn Backend` to many helpers that
//! run one read each. Wrapping the backend once at a call site of the §2.5
//! table sends all of that call's reads through the router, without touching
//! every helper. Everything but `fetch_all` is the inner backend's:
//! transactions, `fetch_all_as`, `explain` and `maintenance` stay on the
//! primary.

use std::time::Duration;

use super::ReadIntent;
use crate::backend::{Backend, BoxFuture, PlanStep, Rows, RunAs, SlowQuery, Statement, TxMode, WriteTx};
use crate::dialect::Dialect;
use crate::DbError;

/// A read-only view of `inner` that routes `fetch_all` by `intent`.
#[derive(Debug)]
pub struct ReadVia<'a> {
    inner: &'a dyn Backend,
    intent: ReadIntent,
}

impl<'a> ReadVia<'a> {
    /// Routes reads as [`ReadIntent::replica`]`(what)`.
    pub fn replica(inner: &'a dyn Backend, what: &'static str) -> Self {
        Self {
            inner,
            intent: ReadIntent::replica(what),
        }
    }

    /// Routes reads by `intent`.
    pub fn new(inner: &'a dyn Backend, intent: ReadIntent) -> Self {
        Self { inner, intent }
    }
}

impl Backend for ReadVia<'_> {
    fn dialect(&self) -> &dyn Dialect {
        self.inner.dialect()
    }

    fn feature_active(&self, feature: crate::Feature) -> bool {
        self.inner.feature_active(feature)
    }

    fn fetch_all<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.inner.fetch_routed(stmt, &self.intent)
    }

    fn fetch_routed<'a>(&'a self, stmt: &'a Statement, intent: &'a ReadIntent) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.inner.fetch_routed(stmt, intent)
    }

    fn begin(&self, mode: TxMode) -> BoxFuture<'_, Result<Box<dyn WriteTx>, DbError>> {
        self.inner.begin(mode)
    }

    fn begin_as<'a>(&'a self, mode: TxMode, run_as: &'a RunAs) -> BoxFuture<'a, Result<Box<dyn WriteTx>, DbError>> {
        self.inner.begin_as(mode, run_as)
    }

    fn fetch_all_as<'a>(&'a self, stmt: &'a Statement, run_as: &'a RunAs) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.inner.fetch_all_as(stmt, run_as)
    }

    fn explain<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<Vec<PlanStep>, DbError>> {
        self.inner.explain(stmt)
    }

    fn statement_writes<'a>(&'a self, stmt: &'a Statement) -> BoxFuture<'a, Result<bool, DbError>> {
        self.inner.statement_writes(stmt)
    }

    fn slow_queries(&self) -> Vec<SlowQuery> {
        self.inner.slow_queries()
    }

    fn clear_slow_queries(&self) {
        self.inner.clear_slow_queries();
    }

    fn slow_query_threshold(&self) -> Duration {
        self.inner.slow_query_threshold()
    }

    fn set_slow_query_threshold(&self, threshold: Duration) {
        self.inner.set_slow_query_threshold(threshold);
    }

    fn maintenance<'a>(&'a self, statements: &'a [String]) -> BoxFuture<'a, Result<(), DbError>> {
        self.inner.maintenance(statements)
    }
}
