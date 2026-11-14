//! Function profiles (ADR-0036 §3): per function and version statistics from
//! the host-call boundary, and the recent invocations with their timelines.

use std::collections::BTreeMap;

use forgebase_core::sketch::Sketch;
use forgebase_db::Statement;
use forgebase_functions::profile::{FnAggregate, HostStat};
use forgebase_functions::{InvocationDetail, InvocationQuery};
use serde::Serialize;
use utoipa::ToSchema;

use crate::InsightsService;
use crate::error::{InsightsError, Result};
use crate::util::{Range, day_of, int, iso, ms, r3, text};

/// Host calls of one operation.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostBreakdown {
    /// The operation (`db.get`, `kv`, `fetch`, `storage.put`, ...).
    pub op: String,
    /// Calls.
    pub count: u64,
    /// Calls that returned an error to the function.
    pub errors: u64,
    /// Total time in milliseconds.
    pub total_ms: f64,
    /// Mean time in milliseconds.
    pub mean_ms: f64,
    /// Longest call in milliseconds.
    pub max_ms: f64,
    /// Share of all the function's host time, 0 to 1.
    pub share: f64,
}

/// Statistics of one function version over a range.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionStat {
    /// Function name.
    pub function: String,
    /// Version.
    pub version: i64,
    /// Executions.
    pub invocations: u64,
    /// Executions that did not end `ok`.
    pub errors: u64,
    /// `errors / invocations`.
    pub error_rate: f64,
    /// Executions that hit their time limit.
    pub timeouts: u64,
    /// Executions that had to load their module or script.
    pub cold_starts: u64,
    /// Mean duration in milliseconds.
    pub mean_ms: f64,
    /// Median duration in milliseconds (within about 11 %).
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
    /// Longest execution in milliseconds.
    pub max_ms: f64,
    /// Mean fuel (WebAssembly instructions, roughly) per execution.
    pub fuel_mean: u64,
    /// Most fuel one execution used.
    pub fuel_max: u64,
    /// The fuel limit of the version.
    pub fuel_limit: u64,
    /// `fuel_max / fuel_limit`: how close the worst execution came.
    pub fuel_utilisation: f64,
    /// Largest memory one execution had, in bytes.
    pub memory_max_bytes: u64,
    /// The memory limit in bytes.
    pub memory_limit_bytes: u64,
    /// Host calls per execution, on average.
    pub host_calls_per_invocation: f64,
    /// Host calls by operation, most time first.
    pub host: Vec<HostBreakdown>,
}

/// Functions with their versions, most executions first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionStatList {
    /// One row per function version.
    pub items: Vec<FunctionStat>,
    /// Start of the range (RFC 3339).
    pub from: String,
    /// End of the range, exclusive.
    pub to: String,
}

/// One time bucket of a function.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionPoint {
    /// Start of the bucket (RFC 3339).
    pub at: String,
    /// Executions.
    pub invocations: u64,
    /// Failures.
    pub errors: u64,
    /// Cold starts.
    pub cold_starts: u64,
    /// Mean duration in milliseconds.
    pub mean_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
}

/// A function over a range: its versions and its time series.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunctionDetail {
    /// Function name.
    pub function: String,
    /// Statistics per version, newest version first.
    pub versions: Vec<FunctionStat>,
    /// Buckets of all versions, oldest first.
    pub series: Vec<FunctionPoint>,
}

/// A page of recorded executions with their profiles, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InvocationPage {
    /// The executions.
    pub items: Vec<InvocationDetail>,
    /// Pass as `cursor` for the next page; absent after the last one.
    pub next_cursor: Option<i64>,
}

fn fn_agg_from_row(r: &[forgebase_db::Value]) -> FnAggregate {
    // function, version, invocations, errors, timeouts, cold_starts, total_us, max_us, fuel_total, fuel_max,
    // fuel_limit, memory_max, memory_limit, host_calls, sketch, host
    let host: BTreeMap<String, HostStat> = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&text(r, 15))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| {
            let g = |f: &str| v.get(f).and_then(serde_json::Value::as_u64).unwrap_or(0);
            (
                k,
                HostStat {
                    count: g("count"),
                    total_us: g("total_us"),
                    max_us: g("max_us"),
                    errors: g("errors"),
                },
            )
        })
        .collect();
    FnAggregate {
        function: text(r, 0),
        version: int(r, 1),
        invocations: int(r, 2) as u64,
        errors: int(r, 3) as u64,
        timeouts: int(r, 4) as u64,
        cold_starts: int(r, 5) as u64,
        total_us: int(r, 6) as u64,
        max_us: int(r, 7) as u64,
        fuel_total: int(r, 8) as u64,
        fuel_max: int(r, 9) as u64,
        fuel_limit: int(r, 10) as u64,
        memory_max: int(r, 11) as u64,
        memory_limit: int(r, 12) as u64,
        host_calls: int(r, 13) as u64,
        sketch: Sketch::from_json(&text(r, 14)),
        host,
    }
}

fn host_json(host: &BTreeMap<String, HostStat>) -> String {
    serde_json::to_string(host).unwrap_or_else(|_| "{}".into())
}

fn stat_of(a: &FnAggregate) -> FunctionStat {
    let n = a.invocations.max(1);
    let q = |p: f64| ms(a.sketch.quantile_within(p, 0, a.max_us).unwrap_or(0));
    let host_total: u64 = a.host.values().map(|h| h.total_us).sum();
    let mut host: Vec<HostBreakdown> = a
        .host
        .iter()
        .map(|(op, h)| HostBreakdown {
            op: op.clone(),
            count: h.count,
            errors: h.errors,
            total_ms: ms(h.total_us),
            mean_ms: ms(h.total_us.checked_div(h.count).unwrap_or(0)),
            max_ms: ms(h.max_us),
            share: r3(if host_total == 0 {
                0.0
            } else {
                h.total_us as f64 / host_total as f64
            }),
        })
        .collect();
    host.sort_by(|x, y| y.total_ms.total_cmp(&x.total_ms).then_with(|| x.op.cmp(&y.op)));
    FunctionStat {
        function: a.function.clone(),
        version: a.version,
        invocations: a.invocations,
        errors: a.errors,
        error_rate: r3(a.errors as f64 / n as f64),
        timeouts: a.timeouts,
        cold_starts: a.cold_starts,
        mean_ms: ms(a.total_us / n),
        p50_ms: q(0.5),
        p95_ms: q(0.95),
        p99_ms: q(0.99),
        max_ms: ms(a.max_us),
        fuel_mean: a.fuel_total / n,
        fuel_max: a.fuel_max,
        fuel_limit: a.fuel_limit,
        fuel_utilisation: r3(if a.fuel_limit == 0 {
            0.0
        } else {
            a.fuel_max as f64 / a.fuel_limit as f64
        }),
        memory_max_bytes: a.memory_max,
        memory_limit_bytes: a.memory_limit,
        host_calls_per_invocation: r3(a.host_calls as f64 / n as f64),
        host,
    }
}

impl InsightsService {
    pub(crate) async fn flush_functions(&self, start_ms: i64, len_ms: i64) -> Result<usize> {
        let Some(f) = &self.inner.deps.functions else {
            return Ok(0);
        };
        let d = f.profiler().drain();
        let n = d.functions.len();
        self.ingest_functions(d, start_ms, len_ms).await?;
        Ok(n)
    }

    /// Writes drained function aggregates as one bucket, and adds the
    /// per-tenant counters to today's usage rows.
    ///
    /// # Errors
    /// A database error.
    pub async fn ingest_functions(
        &self,
        d: forgebase_functions::profile::FnDrained,
        start_ms: i64,
        len_ms: i64,
    ) -> Result<()> {
        let mut stmts = Vec::new();
        for a in &d.functions {
            metrics::counter!("forgebase_function_cold_starts_total", "function" => a.function.clone())
                .increment(a.cold_starts);
            if a.fuel_limit > 0 {
                metrics::gauge!("forgebase_function_fuel_utilisation_ratio", "function" => a.function.clone())
                    .set(a.fuel_max as f64 / a.fuel_limit as f64);
            }
            let i = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
            stmts.push(Statement::new(
                "INSERT INTO _forgebase_fn_stats
                     (bucket_start, bucket_ms, function, version, invocations, errors, timeouts, cold_starts, total_us, max_us,
                      fuel_total, fuel_max, fuel_limit, memory_max, memory_limit, host_calls, sketch, host)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                vec![
                    start_ms.into(),
                    len_ms.into(),
                    a.function.clone().into(),
                    a.version.into(),
                    i(a.invocations).into(),
                    i(a.errors).into(),
                    i(a.timeouts).into(),
                    i(a.cold_starts).into(),
                    i(a.total_us).into(),
                    i(a.max_us).into(),
                    i(a.fuel_total).into(),
                    i(a.fuel_max).into(),
                    i(a.fuel_limit).into(),
                    i(a.memory_max).into(),
                    i(a.memory_limit).into(),
                    i(a.host_calls).into(),
                    a.sketch.to_json().into(),
                    host_json(&a.host).into(),
                ],
            ));
        }
        let day = day_of(start_ms + len_ms);
        for t in &d.tenants {
            for (metric, v) in [
                ("functions.invocations", t.invocations),
                ("functions.compute_ms", t.compute_ms),
                ("functions.fuel", t.fuel),
            ] {
                stmts.push(Statement::new(
                    "INSERT INTO _forgebase_tenant_usage_daily (day, scope, metric, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (day, scope, metric) DO UPDATE SET
                         value = _forgebase_tenant_usage_daily.value + excluded.value, updated_at = excluded.updated_at",
                    vec![
                        day.clone().into(),
                        format!("env:{}", t.tenant).into(),
                        metric.into(),
                        i64::try_from(v).unwrap_or(i64::MAX).into(),
                        (start_ms + len_ms).into(),
                    ],
                ));
            }
        }
        if !stmts.is_empty() {
            self.write(stmts).await?;
        }
        Ok(())
    }

    async fn function_buckets(
        &self,
        range: Range,
        function: Option<&str>,
        version: Option<i64>,
    ) -> Result<Vec<(i64, FnAggregate)>> {
        let rows = self
            .fetch(
                "SELECT function, version, invocations, errors, timeouts, cold_starts, total_us, max_us, fuel_total, fuel_max,
                        fuel_limit, memory_max, memory_limit, host_calls, sketch, host, bucket_start
                 FROM _forgebase_fn_stats
                 WHERE bucket_start >= ?1 AND bucket_start < ?2
                   AND (CAST(?3 AS TEXT) IS NULL OR function = ?3) AND (CAST(?4 AS BIGINT) IS NULL OR version = ?4)
                 ORDER BY bucket_start LIMIT 200000",
                vec![range.from_ms.into(), range.to_ms.into(), function.map(str::to_owned).into(), version.into()],
            )
            .await?;
        let mut out: Vec<(i64, FnAggregate)> = rows.rows.iter().map(|r| (int(r, 16), fn_agg_from_row(r))).collect();
        if let Some(f) = &self.inner.deps.functions {
            let live_start = self.inner.last_flush_ms.load(std::sync::atomic::Ordering::Relaxed);
            if live_start < range.to_ms && crate::util::now_ms() >= range.from_ms {
                for a in f.profiler().peek().functions {
                    if function.is_none_or(|x| x == a.function) && version.is_none_or(|v| v == a.version) {
                        out.push((live_start, a));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Statistics per function version over a range, most executions first.
    ///
    /// # Errors
    /// A database error.
    pub async fn function_stats(
        &self,
        range: Range,
        function: Option<&str>,
        version: Option<i64>,
    ) -> Result<FunctionStatList> {
        let mut by: BTreeMap<(String, i64), FnAggregate> = BTreeMap::new();
        for (_, a) in self.function_buckets(range, function, version).await? {
            let e = by
                .entry((a.function.clone(), a.version))
                .or_insert_with(|| FnAggregate {
                    function: a.function.clone(),
                    version: a.version,
                    ..FnAggregate::default()
                });
            e.merge(&a);
        }
        let mut items: Vec<FunctionStat> = by.values().map(stat_of).collect();
        items.sort_by(|a, b| {
            b.invocations
                .cmp(&a.invocations)
                .then_with(|| a.function.cmp(&b.function))
                .then_with(|| b.version.cmp(&a.version))
        });
        Ok(FunctionStatList {
            items,
            from: iso(range.from_ms),
            to: iso(range.to_ms),
        })
    }

    /// One function: its versions and a time series.
    ///
    /// # Errors
    /// `not_found` when the range holds nothing of it.
    pub async fn function_detail(&self, name: &str, range: Range) -> Result<FunctionDetail> {
        let list = self.function_stats(range, Some(name), None).await?;
        if list.items.is_empty() {
            return Err(InsightsError::NotFound(format!("function {name}")));
        }
        let mut versions = list.items;
        versions.sort_by_key(|v| std::cmp::Reverse(v.version));
        let mut points: BTreeMap<i64, FnAggregate> = BTreeMap::new();
        for (start, a) in self.function_buckets(range, Some(name), None).await? {
            let e = points.entry(start).or_insert_with(|| FnAggregate {
                function: a.function.clone(),
                ..FnAggregate::default()
            });
            e.merge(&a);
        }
        let series = points
            .into_iter()
            .map(|(start, a)| {
                let s = stat_of(&a);
                FunctionPoint {
                    at: iso(start),
                    invocations: a.invocations,
                    errors: a.errors,
                    cold_starts: a.cold_starts,
                    mean_ms: s.mean_ms,
                    p95_ms: s.p95_ms,
                }
            })
            .collect();
        Ok(FunctionDetail {
            function: name.to_owned(),
            versions,
            series,
        })
    }

    /// Recorded executions with their profiles, newest first.
    ///
    /// # Errors
    /// `unavailable` when functions are switched off.
    pub async fn invocations(&self, mut q: InvocationQuery) -> Result<InvocationPage> {
        let Some(f) = &self.inner.deps.functions else {
            return Err(InsightsError::Unavailable(
                "Functions are disabled on this server".into(),
            ));
        };
        let limit = q.limit.clamp(1, 500);
        q.limit = limit + 1;
        let mut items = f
            .invocations(q)
            .await
            .map_err(|e| InsightsError::Internal(e.to_string()))?;
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = if more { items.last().map(|i| i.log.id) } else { None };
        Ok(InvocationPage { items, next_cursor })
    }

    /// One recorded execution.
    ///
    /// # Errors
    /// `not_found`, or `unavailable` when functions are switched off.
    pub async fn invocation(&self, id: i64) -> Result<InvocationDetail> {
        let page = self
            .invocations(InvocationQuery {
                before_id: Some(id + 1),
                limit: 1,
                ..InvocationQuery::default()
            })
            .await?;
        page.items
            .into_iter()
            .find(|i| i.log.id == id)
            .ok_or_else(|| InsightsError::NotFound(format!("invocation {id}")))
    }
}
