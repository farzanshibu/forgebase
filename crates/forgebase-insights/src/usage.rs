//! Per-tenant usage and cost estimates (ADR-0036 §2).
//!
//! Sources, all reused from what ForgeBase already keeps:
//!
//! * **metering events** (`_forgebase_usage_events`, ADR-0010): units per
//!   subject (`org:<id>` or a principal) and feature over the period, read
//!   straight from the events (indexed by time, so no rollup is kept);
//! * **daily rollups** (`_forgebase_tenant_usage_daily`): resource snapshots
//!   per environment (`storage.bytes`, `kv.keys`, ...) and counters of work
//!   done (`queue.messages`, `jobs.created`, `functions.invocations`, ...),
//!   written by [`InsightsService::rollup_day`] every 15 minutes for today and
//!   yesterday, and by the function profiler's flush.
//!
//! Tenants follow the tenancy tree (ADR-0008 §3): realm → organization →
//! project → environment. Resource numbers belong to environments and roll up
//! to their project, organization and realm; metering events belong to the
//! subject that was billed (an organization, or a principal), so they appear
//! at organization level and above, and, when listing environments, as rows of
//! their own marked `attributed: false`.
//!
//! The cost is an **estimate**: the quantity of each counter priced through
//! the metered price of the same feature in the billing catalog (per unit,
//! graduated or volume, in the requested currency) as if the period were one
//! billing period, plus the credits the credit rates would charge. Gauges are
//! not priced.

use std::collections::{BTreeMap, HashMap, HashSet};

use forgebase_billing::money::{self, Tier};
use forgebase_billing::{Price, PriceType};
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement, Value};
use serde::Serialize;
use utoipa::ToSchema;

use crate::InsightsService;
use crate::error::{InsightsError, Result};
use crate::util::{Range, day_of, int, iso, now_ms, opt_text, text};

/// Resource snapshots: the value of the last day in a period is shown, with the peak.
const GAUGES: &[&str] = &["storage.bytes", "storage.objects", "kv.keys", "kv.bytes", "vector.items"];

/// What a built-in metric is.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MetricInfo {
    /// Key, as used in `metric` and `sort`.
    pub key: String,
    /// `counter` (work done: summed over the period) or `gauge` (a resource
    /// held: last value of the period and its peak).
    pub kind: &'static str,
    /// Unit.
    pub unit: &'static str,
    /// What it counts.
    pub description: &'static str,
    /// Where it comes from.
    pub source: &'static str,
}

/// The built-in metrics; `usage.<feature>` metrics come from metering events.
pub fn builtin_metrics() -> Vec<MetricInfo> {
    let m = |key: &str, kind, unit, description, source| MetricInfo {
        key: key.to_owned(),
        kind,
        unit,
        description,
        source,
    };
    vec![
        m("storage.bytes", "gauge", "bytes", "Bytes of current objects in the environment's buckets", "daily rollup"),
        m("storage.objects", "gauge", "objects", "Current objects in the environment's buckets", "daily rollup"),
        m("kv.keys", "gauge", "keys", "Live key-value entries", "daily rollup"),
        m("kv.bytes", "gauge", "bytes", "Bytes of live key-value data (JSON text)", "daily rollup"),
        m("vector.items", "gauge", "items", "Vector items in the environment's collections", "daily rollup"),
        m("queue.messages", "counter", "messages", "Messages published to the environment's topics (still retained)", "daily rollup"),
        m("queue.bytes", "counter", "bytes", "Bytes of those messages", "daily rollup"),
        m("jobs.created", "counter", "jobs", "Background jobs created for the environment", "daily rollup"),
        m("functions.invocations", "counter", "invocations", "Function executions on behalf of the environment", "function profiler"),
        m("functions.compute_ms", "counter", "ms", "Wall time of those executions", "function profiler"),
        m("functions.fuel", "counter", "fuel", "WebAssembly fuel those executions used", "function profiler"),
        m("usage.<feature>", "counter", "feature unit", "Units metered for a feature (ADR-0010) for an organization or principal", "metering events"),
    ]
}

fn is_gauge(metric: &str) -> bool {
    GAUGES.contains(&metric)
}

/// The level usage is grouped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Environments (tenants), with metering rows of their own.
    Environment,
    /// Projects.
    Project,
    /// Organizations (and principals owning personal projects).
    Org,
    /// Realms.
    Realm,
}

impl Level {
    /// Parses the query value.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "environment" | "env" | "tenant" => Self::Environment,
            "project" => Self::Project,
            "org" | "organization" => Self::Org,
            "realm" => Self::Realm,
            _ => return None,
        })
    }
}

/// One metric of a tenant over the period.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MetricValue {
    /// `counter` or `gauge`.
    pub kind: &'static str,
    /// Counters: the sum over the period. Gauges: the value of the last day
    /// with data in the period.
    pub value: i64,
    /// Gauges: the largest daily value of the period.
    pub peak: Option<i64>,
}

/// One priced line of a cost estimate.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CostLine {
    /// The metric.
    pub metric: String,
    /// Units priced.
    pub quantity: i64,
    /// The price used (its key), when one is set for the feature.
    pub price: Option<String>,
    /// Estimated amount in minor units of the currency.
    pub amount_minor: Option<i64>,
    /// Credits the credit rate of the feature would charge.
    pub credits: Option<i64>,
}

/// A cost estimate. It prices the period's quantities with the catalog as
/// they are now: not an invoice.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CostEstimate {
    /// Always true: this is an estimate.
    pub estimate: bool,
    /// ISO 4217 code.
    pub currency: String,
    /// Sum of the priced lines in minor units.
    pub amount_minor: i64,
    /// The same as a decimal string.
    pub amount: String,
    /// Credits the credit rates would charge, in total.
    pub credits: i64,
    /// What was priced.
    pub lines: Vec<CostLine>,
}

/// Usage of one tenant (or tenant group).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageRow {
    /// The level of this row (`environment`, `project`, `org`, `user`, `realm`, `shared`).
    pub level: String,
    /// Id at that level (`shared` for data that belongs to no tenant).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Realm id, when known.
    pub realm: Option<String>,
    /// Organization id, when known.
    pub org: Option<String>,
    /// Project id, when known.
    pub project: Option<String>,
    /// False for a metering row listed among environments: the events belong
    /// to an organization or principal, not to one environment.
    pub attributed: bool,
    /// The metrics.
    pub metrics: BTreeMap<String, MetricValue>,
    /// The cost estimate, when billing is on and something is priced.
    pub cost: Option<CostEstimate>,
}

/// A page of usage rows.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageReport {
    /// Start of the period (RFC 3339).
    pub from: String,
    /// End of the period, exclusive.
    pub to: String,
    /// Grouping level asked for.
    pub group_by: Level,
    /// Sort key used.
    pub sort: String,
    /// Rows matching before paging.
    pub total: usize,
    /// Page size.
    pub limit: usize,
    /// Rows skipped.
    pub offset: usize,
    /// Currency of the cost estimates, when there are any.
    pub currency: Option<String>,
    /// Whether the billing catalog was available for prices.
    pub cost_available: bool,
    /// Always true.
    pub estimate: bool,
    /// The rows.
    pub items: Vec<UsageRow>,
}

/// Daily values of one scope.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageSeries {
    /// The scope (`env:<id>`).
    pub scope: String,
    /// Per metric, `[day, value]` pairs, oldest first.
    pub metrics: BTreeMap<String, Vec<(String, i64)>>,
}

/// Filters of [`InsightsService::usage`].
#[derive(Debug, Clone)]
pub struct UsageQuery {
    /// Grouping level.
    pub group_by: Level,
    /// Only these metrics (empty: all).
    pub metrics: Vec<String>,
    /// Metric to sort by, or `cost`; default: the first metric with data, by name.
    pub sort: Option<String>,
    /// Only this realm.
    pub realm: Option<String>,
    /// Only this organization.
    pub org: Option<String>,
    /// Only this project.
    pub project: Option<String>,
    /// Currency of the estimates; default: the currency of most metered prices.
    pub currency: Option<String>,
    /// Page size (1 to 500).
    pub limit: usize,
    /// Rows to skip.
    pub offset: usize,
}

impl Default for UsageQuery {
    fn default() -> Self {
        Self {
            group_by: Level::Environment,
            metrics: Vec::new(),
            sort: None,
            realm: None,
            org: None,
            project: None,
            currency: None,
            limit: 50,
            offset: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// The tenancy tree
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Tree {
    realms: HashMap<String, String>,
    orgs: HashMap<String, (String, String)>,
    /// project id → (realm, org, owner user, name)
    projects: HashMap<String, (String, Option<String>, Option<String>, String)>,
    /// environment id → (project id, name)
    envs: HashMap<String, (String, String)>,
}

#[derive(Debug, Clone, Default)]
struct Chain {
    env: Option<(String, String)>,
    project: Option<(String, String)>,
    /// `(id, name)`; for a personal project the owner (`user:<id>`).
    org: Option<(String, String)>,
    realm: Option<(String, String)>,
    /// The scope itself, for the level it stands at.
    own: (String, String, String),
}

impl Tree {
    async fn load(svc: &InsightsService) -> Result<Self> {
        let mut t = Tree::default();
        for r in svc.fetch("SELECT id, name FROM _forgebase_realms", vec![]).await?.rows {
            t.realms.insert(text(&r, 0), text(&r, 1));
        }
        for r in svc.fetch("SELECT id, realm_id, name FROM _forgebase_orgs", vec![]).await?.rows {
            t.orgs.insert(text(&r, 0), (text(&r, 1), text(&r, 2)));
        }
        for r in svc
            .fetch("SELECT id, realm_id, org_id, owner_user, name FROM _forgebase_projects", vec![])
            .await?
            .rows
        {
            t.projects.insert(text(&r, 0), (text(&r, 1), opt_text(&r, 2), opt_text(&r, 3), text(&r, 4)));
        }
        for r in svc.fetch("SELECT id, project_id, name FROM _forgebase_environments", vec![]).await?.rows {
            t.envs.insert(text(&r, 0), (text(&r, 1), text(&r, 2)));
        }
        Ok(t)
    }

    fn realm(&self, id: &str) -> Option<(String, String)> {
        Some((id.to_owned(), self.realms.get(id).cloned().unwrap_or_else(|| id.to_owned())))
    }

    /// The tree position of a scope string.
    fn chain(&self, scope: &str) -> Chain {
        let mut c = Chain::default();
        if let Some(env) = scope.strip_prefix("env:") {
            let name = self.envs.get(env).map_or_else(|| env.to_owned(), |(_, n)| n.clone());
            c.own = ("environment".into(), env.to_owned(), name.clone());
            c.env = Some((env.to_owned(), name));
            if let Some((pid, _)) = self.envs.get(env)
                && let Some((realm, org, owner, pname)) = self.projects.get(pid)
            {
                c.project = Some((pid.clone(), pname.clone()));
                c.realm = self.realm(realm);
                c.org = match (org, owner) {
                    (Some(o), _) => Some((o.clone(), self.orgs.get(o).map_or_else(|| o.clone(), |(_, n)| n.clone()))),
                    (None, Some(u)) => Some((format!("user:{u}"), format!("user:{u}"))),
                    _ => None,
                };
            }
        } else if let Some(org) = scope.strip_prefix("org:") {
            let name = self.orgs.get(org).map_or_else(|| org.to_owned(), |(_, n)| n.clone());
            c.own = ("org".into(), org.to_owned(), name.clone());
            c.org = Some((org.to_owned(), name));
            if let Some((realm, _)) = self.orgs.get(org) {
                c.realm = self.realm(realm);
            }
        } else if scope.is_empty() {
            c.own = ("shared".into(), "shared".into(), "Data without a tenant".into());
        } else {
            c.own = ("user".into(), scope.to_owned(), scope.to_owned());
            c.org = Some((scope.to_owned(), scope.to_owned()));
        }
        c
    }
}

// ---------------------------------------------------------------------------
// Rollup
// ---------------------------------------------------------------------------

impl InsightsService {
    /// Every database that may hold tenant rows: the control database and, on
    /// a sharded server, each shard.
    fn tenant_backends(&self) -> Vec<std::sync::Arc<dyn Backend>> {
        let mut out: Vec<std::sync::Arc<dyn Backend>> = vec![self.inner.deps.db.clone()];
        if let Some(r) = &self.inner.deps.router {
            for s in r.sets() {
                if !s.is_control() {
                    out.push(std::sync::Arc::new(s.database().clone()));
                }
            }
        }
        out
    }

    /// Computes the resource and work numbers of `day` (`YYYY-MM-DD`, UTC) from
    /// the live tables and stores them (replacing the day's values of these
    /// metrics; the function counters are kept). Safe to repeat.
    ///
    /// # Errors
    /// `invalid_parameter` for a bad date; a database error.
    pub async fn rollup_day(&self, day: &str) -> Result<usize> {
        let date = crate::util::parse_date(day)
            .ok_or_else(|| InsightsError::invalid("invalid_parameter", "day must be a date, YYYY-MM-DD"))?;
        let start = date.midnight().assume_utc().unix_timestamp() * 1000;
        let end = start + 86_400_000;
        let now = now_ms();
        let mut values: BTreeMap<(String, &'static str), i64> = BTreeMap::new();
        let scope = |t: Option<String>| t.filter(|t| !t.is_empty()).map_or_else(String::new, |t| format!("env:{t}"));
        for db in self.tenant_backends() {
            let queries: [(&str, Vec<Value>, &[&'static str]); 5] = [
                (
                    "SELECT b.tenant, COALESCE(SUM(o.size), 0), COUNT(*) FROM _forgebase_storage_objects o
                     JOIN _forgebase_storage_buckets b ON b.name = o.bucket
                     WHERE o.is_latest = 1 AND o.delete_marker = 0 GROUP BY b.tenant",
                    vec![],
                    &["storage.bytes", "storage.objects"],
                ),
                (
                    "SELECT tenant, COUNT(*), COALESCE(SUM(length(CAST(data AS TEXT))), 0) FROM _forgebase_kv_entries
                     WHERE expires_at IS NULL OR expires_at > ?1 GROUP BY tenant",
                    vec![now.into()],
                    &["kv.keys", "kv.bytes"],
                ),
                (
                    "SELECT c.tenant, COUNT(*) FROM _forgebase_vector_items i
                     JOIN _forgebase_vector_collections c ON c.id = i.collection_id GROUP BY c.tenant",
                    vec![],
                    &["vector.items"],
                ),
                (
                    "SELECT t.tenant, COUNT(*), COALESCE(SUM(m.bytes), 0) FROM _forgebase_queue_messages m
                     JOIN _forgebase_queue_topics t ON t.name = m.topic
                     WHERE m.published_at >= ?1 AND m.published_at < ?2 GROUP BY t.tenant",
                    vec![start.into(), end.into()],
                    &["queue.messages", "queue.bytes"],
                ),
                (
                    "SELECT tenant, COUNT(*) FROM _forgebase_jobs WHERE created_at >= ?1 AND created_at < ?2 GROUP BY tenant",
                    vec![start.into(), end.into()],
                    &["jobs.created"],
                ),
            ];
            for (sql, params, names) in queries {
                match db.fetch_all(&Statement::new(sql, params)).await {
                    Ok(rows) => {
                        for r in rows.rows {
                            let s = scope(opt_text(&r, 0));
                            for (i, name) in names.iter().enumerate() {
                                *values.entry((s.clone(), name)).or_insert(0) += int(&r, i + 1);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, query = names[0], "tenant usage rollup query failed"),
                }
            }
        }
        let n = values.len();
        let stmts: Vec<Statement> = values
            .into_iter()
            .map(|((scope, metric), v)| {
                Statement::new(
                    "INSERT INTO _forgebase_tenant_usage_daily (day, scope, metric, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (day, scope, metric) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    vec![day.to_owned().into(), scope.into(), metric.into(), v.into(), now.into()],
                )
            })
            .collect();
        if !stmts.is_empty() {
            self.write(stmts).await?;
        }
        Ok(n)
    }

    /// Rolls up today and yesterday.
    pub(crate) async fn rollup_recent(&self, now: i64) -> Result<()> {
        self.rollup_day(&day_of(now)).await?;
        self.rollup_day(&day_of(now - 86_400_000)).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// Prices of metered features: feature → prices, and the default currency.
    async fn price_book(&self) -> (HashMap<String, Vec<Price>>, Option<String>) {
        let Some(b) = &self.inner.deps.billing else {
            return (HashMap::new(), None);
        };
        let prices = match b.prices(&AccessContext::system(), None).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "insights: the price list is not available");
                return (HashMap::new(), None);
            }
        };
        let mut book: HashMap<String, Vec<Price>> = HashMap::new();
        let mut currencies: HashMap<String, usize> = HashMap::new();
        for p in prices {
            if p.price_type == PriceType::Metered
                && p.active
                && let Some(f) = p.feature.clone()
            {
                *currencies.entry(p.currency.clone()).or_default() += 1;
                book.entry(f).or_default().push(p);
            }
        }
        let default = currencies.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0))).map(|c| c.0);
        (book, default)
    }

    async fn rates(&self) -> HashMap<String, (i64, i64)> {
        let Some(c) = &self.inner.deps.credits else {
            return HashMap::new();
        };
        match c.rates(&AccessContext::system()).await {
            Ok(r) => r
                .into_iter()
                .map(|r| (r.feature, (r.credits_per_unit, r.per_units.max(1))))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "insights: the credit rates are not available");
                HashMap::new()
            }
        }
    }

    /// Usage per tenant over a period, with cost estimates.
    ///
    /// # Errors
    /// `invalid_parameter` for an unknown sort metric; a database error.
    pub async fn usage(&self, range: Range, q: &UsageQuery) -> Result<UsageReport> {
        let tree = Tree::load(self).await?;
        let (first_day, last_day) = (day_of(range.from_ms), day_of(range.to_ms - 1));
        // scope → metric → value
        let mut by_scope: BTreeMap<String, BTreeMap<String, MetricValue>> = BTreeMap::new();
        let rows = self
            .fetch(
                "SELECT scope, metric, SUM(value), MAX(value) FROM _forgebase_tenant_usage_daily
                 WHERE day >= ?1 AND day <= ?2 GROUP BY scope, metric",
                vec![first_day.clone().into(), last_day.clone().into()],
            )
            .await?;
        let mut peaks: HashMap<(String, String), i64> = HashMap::new();
        for r in &rows.rows {
            let (scope, metric) = (text(r, 0), text(r, 1));
            peaks.insert((scope.clone(), metric.clone()), int(r, 3));
            if !is_gauge(&metric) {
                by_scope.entry(scope).or_default().insert(
                    metric,
                    MetricValue {
                        kind: "counter",
                        value: int(r, 2),
                        peak: None,
                    },
                );
            }
        }
        let last = self
            .fetch(
                "SELECT d.scope, d.metric, d.value FROM _forgebase_tenant_usage_daily d
                 JOIN (SELECT scope, metric, MAX(day) AS md FROM _forgebase_tenant_usage_daily
                       WHERE day >= ?1 AND day <= ?2 GROUP BY scope, metric) m
                   ON d.scope = m.scope AND d.metric = m.metric AND d.day = m.md",
                vec![first_day.into(), last_day.into()],
            )
            .await?;
        for r in &last.rows {
            let (scope, metric) = (text(r, 0), text(r, 1));
            if is_gauge(&metric) {
                let peak = peaks.get(&(scope.clone(), metric.clone())).copied();
                by_scope.entry(scope).or_default().insert(
                    metric,
                    MetricValue {
                        kind: "gauge",
                        value: int(r, 2),
                        peak,
                    },
                );
            }
        }
        let ev = self
            .fetch(
                "SELECT subject, feature_key, SUM(units) FROM _forgebase_usage_events
                 WHERE at >= ?1 AND at < ?2 AND source <> 'gauge' GROUP BY subject, feature_key",
                vec![range.from_ms.into(), range.to_ms.into()],
            )
            .await?;
        for r in &ev.rows {
            by_scope.entry(text(r, 0)).or_default().insert(
                format!("usage.{}", text(r, 1)),
                MetricValue {
                    kind: "counter",
                    value: int(r, 2),
                    peak: None,
                },
            );
        }

        // Group by level.
        let mut groups: BTreeMap<(String, String), UsageRow> = BTreeMap::new();
        for (scope, metrics) in by_scope {
            let c = tree.chain(&scope);
            let (level, id, name, attributed) = match q.group_by {
                Level::Environment => match &c.env {
                    Some((i, n)) => ("environment".to_owned(), i.clone(), n.clone(), true),
                    None => (c.own.0.clone(), c.own.1.clone(), c.own.2.clone(), false),
                },
                Level::Project => match &c.project {
                    Some((i, n)) => ("project".to_owned(), i.clone(), n.clone(), true),
                    None => (c.own.0.clone(), c.own.1.clone(), c.own.2.clone(), false),
                },
                Level::Org => match &c.org {
                    Some((i, n)) => (
                        if i.starts_with("user:") { "user" } else { "org" }.to_owned(),
                        i.clone(),
                        n.clone(),
                        true,
                    ),
                    None => (c.own.0.clone(), c.own.1.clone(), c.own.2.clone(), false),
                },
                Level::Realm => match &c.realm {
                    Some((i, n)) => ("realm".to_owned(), i.clone(), n.clone(), true),
                    None => (c.own.0.clone(), c.own.1.clone(), c.own.2.clone(), false),
                },
            };
            if q.realm.as_ref().is_some_and(|r| c.realm.as_ref().map(|x| &x.0) != Some(r))
                || q.org.as_ref().is_some_and(|o| c.org.as_ref().map(|x| &x.0) != Some(o))
                || q.project.as_ref().is_some_and(|p| c.project.as_ref().map(|x| &x.0) != Some(p))
            {
                continue;
            }
            let row = groups.entry((level.clone(), id.clone())).or_insert_with(|| UsageRow {
                level,
                id,
                name,
                realm: c.realm.as_ref().map(|r| r.0.clone()),
                org: c.org.as_ref().map(|o| o.0.clone()),
                project: if q.group_by == Level::Environment || q.group_by == Level::Project {
                    c.project.as_ref().map(|p| p.0.clone())
                } else {
                    None
                },
                attributed,
                metrics: BTreeMap::new(),
                cost: None,
            });
            if q.group_by == Level::Realm {
                row.org = None;
            }
            for (k, v) in metrics {
                if !q.metrics.is_empty() && !q.metrics.contains(&k) {
                    continue;
                }
                let e = row.metrics.entry(k).or_insert(MetricValue {
                    kind: v.kind,
                    value: 0,
                    peak: v.peak.map(|_| 0),
                });
                e.value += v.value;
                if let (Some(p), Some(add)) = (e.peak.as_mut(), v.peak) {
                    // Peaks of different environments add up: the group's peak is at most their sum.
                    *p += add;
                }
            }
        }
        let mut rows: Vec<UsageRow> = groups.into_values().filter(|r| !r.metrics.is_empty()).collect();

        // Cost.
        let (book, default_currency) = self.price_book().await;
        let rates = self.rates().await;
        let currency = q
            .currency
            .as_deref()
            .map(str::to_uppercase)
            .or(default_currency)
            .or_else(|| (!rates.is_empty()).then(|| "USD".to_owned()));
        let cost_available = self.inner.deps.billing.is_some() || self.inner.deps.credits.is_some();
        if let Some(cur) = &currency {
            for row in &mut rows {
                row.cost = estimate(row, cur, &book, &rates);
            }
        }

        // Sort and page.
        let sort = q.sort.clone().unwrap_or_else(|| {
            let mut seen: HashSet<&String> = HashSet::new();
            let mut names: Vec<&String> = rows.iter().flat_map(|r| r.metrics.keys()).filter(|k| seen.insert(k)).collect();
            names.sort();
            names.first().map_or_else(|| "cost".to_owned(), |s| (*s).clone())
        });
        if sort != "cost" && !rows.iter().any(|r| r.metrics.contains_key(&sort)) && !rows.is_empty() {
            let known = builtin_metrics().iter().any(|m| m.key == sort) || sort.starts_with("usage.");
            if !known {
                return Err(InsightsError::invalid(
                    "invalid_parameter",
                    format!("sort must be `cost` or a metric key such as storage.bytes; {sort:?} is neither"),
                ));
            }
        }
        let key = |r: &UsageRow| -> i64 {
            if sort == "cost" {
                r.cost.as_ref().map_or(0, |c| c.amount_minor)
            } else {
                r.metrics.get(&sort).map_or(0, |m| m.value)
            }
        };
        rows.sort_by(|a, b| key(b).cmp(&key(a)).then_with(|| a.level.cmp(&b.level)).then_with(|| a.id.cmp(&b.id)));
        let total = rows.len();
        let limit = q.limit.clamp(1, 500);
        let items = rows.into_iter().skip(q.offset).take(limit).collect();
        Ok(UsageReport {
            from: iso(range.from_ms),
            to: iso(range.to_ms),
            group_by: q.group_by,
            sort,
            total,
            limit,
            offset: q.offset,
            currency,
            cost_available,
            estimate: true,
            items,
        })
    }

    /// Daily values of one scope (`env:<id>`) over a range.
    ///
    /// # Errors
    /// A database error.
    pub async fn usage_series(&self, scope: &str, range: Range) -> Result<UsageSeries> {
        let rows = self
            .fetch(
                "SELECT day, metric, value FROM _forgebase_tenant_usage_daily
                 WHERE scope = ?1 AND day >= ?2 AND day <= ?3 ORDER BY day",
                vec![scope.to_owned().into(), day_of(range.from_ms).into(), day_of(range.to_ms - 1).into()],
            )
            .await?;
        let mut metrics: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
        for r in &rows.rows {
            metrics.entry(text(r, 1)).or_default().push((text(r, 0), int(r, 2)));
        }
        Ok(UsageSeries {
            scope: scope.to_owned(),
            metrics,
        })
    }
}

fn estimate(
    row: &UsageRow,
    currency: &str,
    book: &HashMap<String, Vec<Price>>,
    rates: &HashMap<String, (i64, i64)>,
) -> Option<CostEstimate> {
    let (mut total, mut credits_total) = (0i64, 0i64);
    let mut lines = Vec::new();
    for (metric, v) in &row.metrics {
        if v.kind != "counter" || v.value <= 0 {
            continue;
        }
        let feature = metric.strip_prefix("usage.").unwrap_or(metric);
        let price = book
            .get(feature)
            .and_then(|ps| ps.iter().find(|p| p.amounts(currency).is_some()));
        let amount = price.and_then(|p| {
            let (unit, tiers): (i64, Vec<Tier>) = p.amounts(currency)?;
            money::price_amount(p.scheme, unit, &tiers, v.value).ok()
        });
        let credits = rates
            .get(feature)
            .map(|(per, units)| (i128::from(v.value) * i128::from(*per) + i128::from(*units) - 1) / i128::from(*units))
            .map(|c| i64::try_from(c).unwrap_or(i64::MAX));
        if amount.is_none() && credits.is_none() {
            continue;
        }
        total = total.saturating_add(amount.unwrap_or(0));
        credits_total = credits_total.saturating_add(credits.unwrap_or(0));
        lines.push(CostLine {
            metric: metric.clone(),
            quantity: v.value,
            price: price.map(|p| p.key.clone()),
            amount_minor: amount,
            credits,
        });
    }
    if lines.is_empty() {
        return None;
    }
    Some(CostEstimate {
        estimate: true,
        currency: currency.to_owned(),
        amount_minor: total,
        amount: format_minor(total, currency),
        credits: credits_total,
        lines,
    })
}

/// `1234` in a two-digit currency as `12.34`.
pub fn format_minor(amount: i64, currency: &str) -> String {
    let exp = money::exponent(currency) as usize;
    if exp == 0 {
        return amount.to_string();
    }
    let neg = amount < 0;
    let digits = amount.unsigned_abs().to_string();
    let padded = format!("{digits:0>width$}", width = exp + 1);
    let (whole, frac) = padded.split_at(padded.len() - exp);
    format!("{}{whole}.{frac}", if neg { "-" } else { "" })
}
