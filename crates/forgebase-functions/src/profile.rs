//! Function profiling (ADR-0036 §3).
//!
//! Each execution carries a [`Profile`] in its store: the host-call boundary
//! (`call` for WebAssembly functions, `js_call` for JavaScript ones) reads the
//! clock before and after the operation and adds the duration to a per-op
//! counter and, for the first [`MAX_TIMELINE`] calls, to a timeline. When the
//! execution ends, [`FnProfiler::record`] folds the result into an in-memory
//! aggregate per `(function, version)` (invocations, errors, timeouts, cold
//! starts, a duration sketch, fuel and memory against their limits, the
//! host-call breakdown) and a per-tenant counter. `forgebase-insights` drains
//! the aggregates every flush interval into `_forgebase_fn_stats` and the
//! daily tenant table. The same profile, as JSON, is stored in the `profile`
//! column of the execution's log row, so it lives as long as the log row.
//!
//! Cost per host call: two `Instant::now()` readings and an insertion into a
//! small map keyed by a static string; per execution: one short mutex hold.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use forgebase_core::sketch::Sketch;
use serde::Serialize;
use serde_json::{Value as Json, json};

/// Host calls kept in an execution's timeline.
pub const MAX_TIMELINE: usize = 64;

/// The operation names a profile uses; anything else is `other`, so a
/// function cannot grow the key set.
const OPS: &[&str] = &[
    "db.get",
    "db.list",
    "db.create",
    "db.update",
    "db.delete",
    "kv",
    "vector.search",
    "vector.get",
    "vector.upsert",
    "vector.delete",
    "storage.get",
    "storage.list",
    "storage.put",
    "storage.delete",
    "storage.sign",
    "queues.publish",
    "jobs.enqueue",
    "realtime.broadcast",
    "auth.verify",
    "env.get",
    "secret.get",
    "crypto.digest",
    "crypto.hmac",
    "fetch",
];

/// The static name of a host operation (`other` when unknown).
pub fn canonical_op(op: &str) -> &'static str {
    OPS.iter().copied().find(|o| *o == op).unwrap_or("other")
}

/// Count and time of one host operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HostStat {
    /// Calls.
    pub count: u64,
    /// Total wall time in microseconds.
    pub total_us: u64,
    /// Longest call in microseconds.
    pub max_us: u64,
    /// Calls that returned an error to the function.
    pub errors: u64,
}

impl HostStat {
    fn add(&mut self, other: &HostStat) {
        self.count += other.count;
        self.total_us += other.total_us;
        self.max_us = self.max_us.max(other.max_us);
        self.errors += other.errors;
    }
}

/// One host call on an execution's timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallEvent {
    /// The operation.
    pub op: &'static str,
    /// Microseconds from the start of the execution to the call.
    pub at_us: u64,
    /// Duration in microseconds.
    pub dur_us: u64,
    /// Whether the call succeeded.
    pub ok: bool,
}

/// What one execution measured.
#[derive(Debug, Default)]
pub struct Profile {
    enabled: bool,
    started: Option<Instant>,
    /// Per operation.
    pub ops: BTreeMap<&'static str, HostStat>,
    /// The first [`MAX_TIMELINE`] calls.
    pub timeline: Vec<CallEvent>,
    /// Calls beyond the timeline.
    pub timeline_dropped: u32,
    /// Largest linear memory the execution had, in bytes.
    pub peak_memory: u64,
}

impl Profile {
    /// A profile that records when `enabled`.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: enabled.then(Instant::now),
            ..Self::default()
        }
    }

    /// Reads the clock for a host call about to start (`None` when off).
    #[inline]
    pub fn begin(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    /// Records a host call that started at `t` (from [`Profile::begin`]).
    #[inline]
    pub fn end(&mut self, t: Option<Instant>, op: &'static str, ok: bool) {
        let Some(t) = t else { return };
        let dur_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
        let s = self.ops.entry(op).or_default();
        s.count += 1;
        s.total_us += dur_us;
        s.max_us = s.max_us.max(dur_us);
        if !ok {
            s.errors += 1;
        }
        if self.timeline.len() < MAX_TIMELINE {
            let at_us = self
                .started
                .map_or(0, |s| u64::try_from(t.duration_since(s).as_micros()).unwrap_or(0));
            self.timeline.push(CallEvent { op, at_us, dur_us, ok });
        } else {
            self.timeline_dropped = self.timeline_dropped.saturating_add(1);
        }
    }

    /// Notes the memory the execution had (the maximum is kept).
    #[inline]
    pub fn memory(&mut self, bytes: usize) {
        self.peak_memory = self.peak_memory.max(bytes as u64);
    }

    /// Total host calls.
    pub fn host_calls(&self) -> u64 {
        self.ops.values().map(|s| s.count).sum()
    }
}

/// How an execution ended and what it used: input of [`FnProfiler::record`].
#[derive(Debug)]
pub struct Finished<'a> {
    /// Function name.
    pub function: &'a str,
    /// Version that ran.
    pub version: i64,
    /// Outcome (`ok`, `failed`, `timeout`, ...).
    pub status: &'a str,
    /// Wall time of the execution in microseconds.
    pub duration_us: u64,
    /// Fuel consumed.
    pub fuel_used: u64,
    /// The version's fuel limit.
    pub fuel_limit: u64,
    /// The version's memory limit in bytes.
    pub memory_limit: u64,
    /// The module or script was not cached: this execution paid for loading it.
    pub cold: bool,
    /// Microseconds spent loading the module or script.
    pub prepare_us: u64,
    /// The tenant of the caller, when it had one.
    pub tenant: Option<&'a str>,
    /// What the host-call boundary measured.
    pub profile: &'a Profile,
}

impl Finished<'_> {
    /// Whether the status is a failure (anything but `ok`).
    pub fn is_error(&self) -> bool {
        self.status != "ok"
    }

    /// The per-invocation detail stored with the log row and returned by the API.
    pub fn detail_json(&self) -> Json {
        let host: BTreeMap<&str, &HostStat> = self.profile.ops.iter().map(|(k, v)| (*k, v)).collect();
        json!({
            "cold_start": self.cold,
            "prepare_us": self.prepare_us,
            "duration_us": self.duration_us,
            "fuel_used": self.fuel_used,
            "fuel_limit": self.fuel_limit,
            "memory_peak_bytes": self.profile.peak_memory,
            "memory_limit_bytes": self.memory_limit,
            "tenant": self.tenant,
            "host_calls": self.profile.host_calls(),
            "host": host,
            "timeline": self.profile.timeline,
            "timeline_dropped": self.profile.timeline_dropped,
        })
    }
}

/// Aggregated executions of one `(function, version)` since the last drain.
#[derive(Debug, Clone, Default)]
pub struct FnAggregate {
    /// Function name.
    pub function: String,
    /// Version.
    pub version: i64,
    /// Executions.
    pub invocations: u64,
    /// Executions that did not end `ok`.
    pub errors: u64,
    /// Executions that timed out.
    pub timeouts: u64,
    /// Executions that loaded their module or script.
    pub cold_starts: u64,
    /// Total wall time in microseconds.
    pub total_us: u64,
    /// Longest execution in microseconds.
    pub max_us: u64,
    /// Total fuel.
    pub fuel_total: u64,
    /// Most fuel one execution used.
    pub fuel_max: u64,
    /// The fuel limit.
    pub fuel_limit: u64,
    /// Peak memory of any execution in bytes.
    pub memory_max: u64,
    /// The memory limit in bytes.
    pub memory_limit: u64,
    /// Host calls.
    pub host_calls: u64,
    /// Duration distribution in microseconds.
    pub sketch: Sketch,
    /// Per host operation.
    pub host: BTreeMap<String, HostStat>,
}

impl FnAggregate {
    /// Adds the numbers of `other` (the same function and version).
    pub fn merge(&mut self, other: &FnAggregate) {
        self.invocations += other.invocations;
        self.errors += other.errors;
        self.timeouts += other.timeouts;
        self.cold_starts += other.cold_starts;
        self.total_us += other.total_us;
        self.max_us = self.max_us.max(other.max_us);
        self.fuel_total += other.fuel_total;
        self.fuel_max = self.fuel_max.max(other.fuel_max);
        self.fuel_limit = self.fuel_limit.max(other.fuel_limit);
        self.memory_max = self.memory_max.max(other.memory_max);
        self.memory_limit = self.memory_limit.max(other.memory_limit);
        self.host_calls += other.host_calls;
        self.sketch.merge(&other.sketch);
        for (k, v) in &other.host {
            self.host.entry(k.clone()).or_default().add(v);
        }
    }
}

/// Work attributed to one tenant since the last drain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenantFnUsage {
    /// Environment id.
    pub tenant: String,
    /// Executions.
    pub invocations: u64,
    /// Wall time in milliseconds, summed.
    pub compute_ms: u64,
    /// Fuel used.
    pub fuel: u64,
}

/// Everything [`FnProfiler::drain`] took.
#[derive(Debug, Default)]
pub struct FnDrained {
    /// Per function and version.
    pub functions: Vec<FnAggregate>,
    /// Per tenant.
    pub tenants: Vec<TenantFnUsage>,
}

/// The in-memory aggregates of every function.
#[derive(Debug, Default)]
pub struct FnProfiler {
    off: AtomicBool,
    inner: Mutex<ProfilerInner>,
}

#[derive(Debug, Default)]
struct ProfilerInner {
    functions: HashMap<(String, i64), FnAggregate>,
    tenants: HashMap<String, TenantFnUsage>,
}

impl FnProfiler {
    /// Turns profiling on or off (`[observability] function_profiling`).
    pub fn set_enabled(&self, on: bool) {
        self.off.store(!on, Ordering::Relaxed);
    }

    /// Whether executions are profiled.
    pub fn enabled(&self) -> bool {
        !self.off.load(Ordering::Relaxed)
    }

    /// Folds one finished execution into the aggregates.
    pub fn record(&self, f: &Finished<'_>) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let a = g.functions.entry((f.function.to_owned(), f.version)).or_insert_with(|| FnAggregate {
            function: f.function.to_owned(),
            version: f.version,
            ..FnAggregate::default()
        });
        a.invocations += 1;
        if f.is_error() {
            a.errors += 1;
        }
        if f.status == "timeout" {
            a.timeouts += 1;
        }
        if f.cold {
            a.cold_starts += 1;
        }
        a.total_us += f.duration_us;
        a.max_us = a.max_us.max(f.duration_us);
        a.fuel_total += f.fuel_used;
        a.fuel_max = a.fuel_max.max(f.fuel_used);
        a.fuel_limit = f.fuel_limit;
        a.memory_max = a.memory_max.max(f.profile.peak_memory);
        a.memory_limit = f.memory_limit;
        a.host_calls += f.profile.host_calls();
        a.sketch.record(f.duration_us);
        for (op, s) in &f.profile.ops {
            a.host.entry((*op).to_owned()).or_default().add(s);
        }
        if let Some(t) = f.tenant.filter(|t| !t.is_empty()) {
            // Bounded: past 10 000 tenants in one interval the rest is not attributed.
            if g.tenants.len() < 10_000 || g.tenants.contains_key(t) {
                let u = g.tenants.entry(t.to_owned()).or_insert_with(|| TenantFnUsage {
                    tenant: t.to_owned(),
                    ..TenantFnUsage::default()
                });
                u.invocations += 1;
                u.compute_ms += f.duration_us / 1000;
                u.fuel += f.fuel_used;
            }
        }
    }

    /// Takes the aggregates out of memory.
    pub fn drain(&self) -> FnDrained {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut functions: Vec<FnAggregate> = g.functions.drain().map(|(_, v)| v).collect();
        functions.sort_by(|a, b| (&a.function, a.version).cmp(&(&b.function, b.version)));
        let mut tenants: Vec<TenantFnUsage> = g.tenants.drain().map(|(_, v)| v).collect();
        tenants.sort_by(|a, b| a.tenant.cmp(&b.tenant));
        FnDrained { functions, tenants }
    }

    /// The aggregates in memory, without clearing them.
    pub fn peek(&self) -> FnDrained {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        FnDrained {
            functions: g.functions.values().cloned().collect(),
            tenants: g.tenants.values().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished<'a>(p: &'a Profile, status: &'a str, cold: bool, tenant: Option<&'a str>) -> Finished<'a> {
        Finished {
            function: "f",
            version: 2,
            status,
            duration_us: 5_000,
            fuel_used: 100,
            fuel_limit: 1_000,
            memory_limit: 1 << 20,
            cold,
            prepare_us: if cold { 900 } else { 0 },
            tenant,
            profile: p,
        }
    }

    #[test]
    fn host_calls_are_counted_timed_and_bounded() {
        let mut p = Profile::new(true);
        for i in 0..(MAX_TIMELINE + 5) {
            let t = p.begin();
            p.end(t, canonical_op(if i % 2 == 0 { "db.get" } else { "fetch" }), i % 10 != 0);
        }
        p.memory(4096);
        p.memory(1024);
        assert_eq!(p.host_calls(), (MAX_TIMELINE + 5) as u64);
        assert_eq!(p.timeline.len(), MAX_TIMELINE);
        assert_eq!(p.timeline_dropped, 5);
        assert_eq!(p.peak_memory, 4096);
        assert!(p.ops["db.get"].count > 0 && p.ops["fetch"].errors > 0);
        assert_eq!(canonical_op("evil.op.name"), "other");
        let off = Profile::new(false);
        assert!(off.begin().is_none());
    }

    #[test]
    fn aggregates_per_function_version_and_tenant() {
        let prof = FnProfiler::default();
        let mut p = Profile::new(true);
        let t = p.begin();
        p.end(t, "kv", true);
        prof.record(&finished(&p, "ok", true, Some("env1")));
        prof.record(&finished(&p, "timeout", false, Some("env1")));
        prof.record(&finished(&p, "ok", false, None));
        let live = prof.peek();
        assert_eq!(live.functions.len(), 1);
        let d = prof.drain();
        let a = &d.functions[0];
        assert_eq!((a.invocations, a.errors, a.timeouts, a.cold_starts), (3, 1, 1, 1));
        assert_eq!(a.host["kv"].count, 3);
        assert_eq!((a.fuel_total, a.fuel_limit, a.fuel_max), (300, 1_000, 100));
        assert_eq!(a.sketch.count(), 3);
        assert_eq!(d.tenants, vec![TenantFnUsage { tenant: "env1".into(), invocations: 2, compute_ms: 10, fuel: 200 }]);
        assert!(prof.drain().functions.is_empty());
        let j = finished(&p, "ok", true, None).detail_json();
        assert_eq!(j["cold_start"], true);
        assert_eq!(j["host"]["kv"]["count"], 1);
        assert_eq!(j["timeline"][0]["op"], "kv");
    }
}
