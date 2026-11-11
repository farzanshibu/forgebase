//! The table plan of a move: every table holding the tenant's rows, how its
//! rows are found, and how they reach the target (ADR-0029 §3.2, §3.8).
//!
//! * **Published** tables go through the move's publication with a row
//!   filter. A filter may only read the row itself, so it is either the
//!   tenant column (`tenant = '<t>'`, KV: `tenant = 't:<t>'`, collections:
//!   `_tenant = '<t>'`) or a constant list of *container* keys (vector
//!   collections, queue topics and subscriptions, storage buckets):
//!   containers are schema objects, so the list is complete; a container
//!   created during the move is noticed at the switch and its rows are
//!   copied in the freeze.
//! * **Staged** tables find their tenant through a parent row that is data
//!   (job attempts, events and dependencies through the job; upload parts
//!   through the upload; keyword rows through the vector item; record
//!   versions through the record; snapshot items through the snapshot): no
//!   constant filter exists, so the engine copies them into staging tables
//!   on the target and inserts them at the switch ([`super::sync`]).
//! * **Whole** tables belong to the tenant entirely (its pgmq queues).
//!
//! Every table also gets a *predicate* that selects the tenant's rows on
//! either set with subqueries, used to verify, abort and clean up.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::backend::{Backend, Statement, Value};
use crate::placement::{self, PlacementClass, TenantKey};
use crate::{Database, DbError};

use super::pgsql::{self, Column, ident, lit, qualified};

/// System schema on PostgreSQL.
pub const SYSTEM_SCHEMA: &str = "forgebase";

/// How a table's rows reach the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feed {
    /// Publication with a row filter; the subscription copies and streams.
    Publish,
    /// Engine copy into a staging table, inserted at the switch.
    Stage,
    /// The whole table, published without a filter (pgmq queues).
    Whole,
}

/// An identity column given the target's sequence at the switch, in
/// source order (order-defining tables, ADR-0029 §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renumber {
    /// Column.
    pub column: String,
    /// The column's sequence on the target (`pg_get_serial_sequence`).
    pub sequence: String,
}

/// One table of a move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TablePlan {
    /// Schema.
    pub schema: String,
    /// Table.
    pub table: String,
    /// How rows travel.
    pub feed: Feed,
    /// Publication row filter (constant; `None` for whole tables).
    pub filter: Option<String>,
    /// Selects the tenant's rows on either set (subqueries allowed).
    pub predicate: String,
    /// Column of the filter, and the container table and key it lists
    /// (`Publish` children of containers).
    pub container: Option<ContainerRef>,
    /// Primary-key columns.
    pub key: Vec<String>,
    /// Insertable columns, in table order (generated ones left out).
    pub columns: Vec<String>,
    /// Column types, aligned with `columns`.
    pub types: Vec<String>,
    /// Renumbered at the switch.
    pub renumber: Option<Renumber>,
    /// Columns left out of verification (renumbered or remapped).
    pub unhashed: Vec<String>,
    /// Distance from the tenant column (0: has it); deletes go deepest first.
    pub depth: u32,
    /// Whether the source needs `REPLICA IDENTITY FULL` while published (a
    /// filter column outside the replica identity).
    pub identity_full: bool,
    /// Staging table on the target (`Stage`).
    pub staging: Option<String>,
}

/// The container a published child filters by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerRef {
    /// Column of the child.
    pub column: String,
    /// Container table.
    pub table: String,
    /// Its key column.
    pub key: String,
    /// The container keys in the filter, as SQL literals.
    pub listed: Vec<String>,
}

impl TablePlan {
    /// `"schema"."table"`.
    pub fn qualified(&self) -> String {
        qualified(&self.schema, &self.table)
    }

    /// `schema.table` for messages and maps.
    pub fn name(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }

    /// Columns verified (all minus `unhashed`).
    pub fn hashed(&self) -> Vec<String> {
        self.columns
            .iter()
            .filter(|c| !self.unhashed.contains(c))
            .cloned()
            .collect()
    }
}

/// Everything a move touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePlan {
    /// The tenant.
    pub tenant: String,
    /// Tables, parents before children.
    pub tables: Vec<TablePlan>,
    /// The tenant's pgmq queues (`fbs_…`), created on the target.
    pub queues: Vec<String>,
}

impl MovePlan {
    /// A table by `schema.table`.
    pub fn table(&self, name: &str) -> Option<&TablePlan> {
        self.tables.iter().find(|t| t.name() == name)
    }

    /// Tables of one feed.
    pub fn of(&self, feed: Feed) -> impl Iterator<Item = &TablePlan> {
        self.tables.iter().filter(move |t| t.feed == feed)
    }

    /// `CREATE PUBLICATION` / `ALTER PUBLICATION … SET TABLE` list: every
    /// published table with its filter, and the barrier.
    pub fn publication_tables(&self, move_id: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .tables
            .iter()
            .filter(|t| t.feed != Feed::Stage)
            .filter(|t| t.feed == Feed::Whole || t.filter.is_some())
            .map(|t| match &t.filter {
                Some(f) => format!("{} WHERE ({f})", t.qualified()),
                None => t.qualified(),
            })
            .collect();
        out.push(format!(
            "{}.barrier WHERE (move_id = {})",
            super::MOVE_SCHEMA,
            lit(move_id)
        ));
        out
    }
}

/// Containers: tables whose keys are schema objects of the tenant, so a
/// constant list of them is a complete row filter for their children.
const CONTAINERS: &[&str] = &[
    "_forgebase_vector_collections",
    "_forgebase_queue_topics",
    "_forgebase_queue_subscriptions",
    "_forgebase_storage_buckets",
];

/// Order-defining tables renumbered at the switch, by column.
const RENUMBERED: &[(&str, &str)] = &[
    ("_forgebase_queue_messages", "seq"),
    ("_forgebase_job_events", "id"),
    ("_forgebase_job_attempts", "id"),
];

/// Columns remapped (or reset) at the switch: left out of verification.
const REMAPPED: &[(&str, &str)] = &[
    ("_forgebase_queue_deliveries", "seq"),
    ("_forgebase_queue_subscriptions", "cursor_seq"),
    ("_forgebase_queue_snapshots", "cursor_seq"),
    ("_forgebase_queue_snapshot_items", "seq"),
    ("_forgebase_queue_topics", "source_after_seq"),
];

/// How a system table finds its tenant in a move. The registry is the
/// default; queue subscriptions and snapshots follow their topic and
/// subscription (FB-172: a tenant's subscription to an instance-wide topic
/// stays on control), and KV stores the tenant as `t:<env>`.
fn key_of(table: &str, registry: TenantKey) -> TenantKey {
    match table {
        "_forgebase_queue_subscriptions" => TenantKey::Parent {
            column: "topic",
            table: "_forgebase_queue_topics",
            parent_column: "name",
        },
        "_forgebase_queue_snapshots" => TenantKey::Parent {
            column: "subscription",
            table: "_forgebase_queue_subscriptions",
            parent_column: "name",
        },
        _ => registry,
    }
}

/// The value a table's tenant column holds for `tenant`.
fn tenant_value(table: &str, tenant: &str) -> String {
    if table == "_forgebase_kv_entries" {
        format!("t:{tenant}")
    } else {
        tenant.to_owned()
    }
}

struct Builder<'a> {
    source: &'a Database,
    target: &'a Database,
    tenant: &'a str,
    move_id: &'a str,
    /// Predicates of system tables by name (for children).
    predicates: BTreeMap<String, (String, u32)>,
    tables: Vec<TablePlan>,
}

/// Builds the plan of moving `tenant` from `source` to `target`. `control`
/// holds the collection catalog (record versions follow the records of
/// tenant-scoped collections).
pub async fn build(
    control: &dyn Backend,
    source: &Database,
    target: &Database,
    tenant: &str,
    move_id: &str,
) -> Result<MovePlan, DbError> {
    let mut b = Builder {
        source,
        target,
        tenant,
        move_id,
        predicates: BTreeMap::new(),
        tables: Vec::new(),
    };
    // System tables: the registry is ordered so that parents come first;
    // resolve in passes until every parent is known.
    let mut pending: Vec<&placement::TablePlacement> = placement::TABLES
        .iter()
        .filter(|t| t.class == PlacementClass::Tenant)
        .filter(|t| !matches!(t.tenant_key, TenantKey::Record { .. }))
        .collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = Vec::new();
        for t in pending {
            if !b.system_table(t).await? {
                rest.push(t);
            }
        }
        if rest.len() == before {
            // Parents missing on this database (older schema): skip them.
            break;
        }
        pending = rest;
    }
    let collections = b.collection_tables().await?;
    for (schema, table) in &collections {
        b.collection_table(schema, table).await?;
    }
    b.record_versions(control, &collections).await?;
    let queues = b.pgmq_queues().await?;
    Ok(MovePlan {
        tenant: tenant.to_owned(),
        tables: b.tables,
        queues,
    })
}

impl Builder<'_> {
    async fn both_have(&self, schema: &str, table: &str) -> Result<bool, DbError> {
        Ok(pgsql::table_exists(self.source, schema, table).await?
            && pgsql::table_exists(self.target, schema, table).await?)
    }

    fn staging_name(&self) -> String {
        format!(
            "{}_{}",
            super::object_name(self.move_id).trim_start_matches("fb_move_"),
            self.tables.len()
        )
    }

    /// Returns `false` when the table's parent is not resolved yet.
    async fn system_table(&mut self, t: &placement::TablePlacement) -> Result<bool, DbError> {
        if !self.both_have(SYSTEM_SCHEMA, t.table).await? {
            return Ok(true);
        }
        let tenant = self.tenant;
        let (predicate, depth, feed, filter, container, filter_column) = match key_of(t.table, t.tenant_key) {
            TenantKey::Column { column } => {
                let value = lit(&tenant_value(t.table, tenant));
                let p = format!("{} = {value}", ident(column));
                (p.clone(), 0, Feed::Publish, Some(p), None, Some(column.to_owned()))
            }
            TenantKey::Parent {
                column,
                table: parent,
                parent_column,
            } => {
                let Some((parent_pred, parent_depth)) = self.predicates.get(parent).cloned() else {
                    // The parent is not in this database's schema at all, or not yet resolved.
                    return Ok(!pgsql::table_exists(self.source, SYSTEM_SCHEMA, parent).await?);
                };
                let sub = format!(
                    "SELECT {} FROM {} WHERE {parent_pred}",
                    ident(parent_column),
                    qualified(SYSTEM_SCHEMA, parent)
                );
                let predicate = format!("{} IN ({sub})", ident(column));
                if CONTAINERS.contains(&parent) {
                    let listed = self.list_keys(&sub).await?;
                    let filter = (!listed.is_empty()).then(|| format!("{} IN ({})", ident(column), listed.join(", ")));
                    let container = ContainerRef {
                        column: column.to_owned(),
                        table: parent.to_owned(),
                        key: parent_column.to_owned(),
                        listed,
                    };
                    (
                        predicate,
                        parent_depth + 1,
                        Feed::Publish,
                        filter,
                        Some(container),
                        Some(column.to_owned()),
                    )
                } else {
                    (predicate, parent_depth + 1, Feed::Stage, None, None, None)
                }
            }
            TenantKey::None | TenantKey::Record { .. } => return Ok(true),
        };
        self.predicates
            .insert(t.table.to_owned(), (predicate.clone(), depth));
        let mut plan = self.table_plan(SYSTEM_SCHEMA, t.table, feed, filter, predicate, depth).await?;
        plan.container = container;
        if let Some(col) = filter_column {
            plan.identity_full = !self.identity_covers(SYSTEM_SCHEMA, t.table, &col).await?;
        }
        self.tables.push(plan);
        Ok(true)
    }

    async fn list_keys(&self, sub: &str) -> Result<Vec<String>, DbError> {
        let rows = self
            .source
            .fetch_all(&Statement::new(
                format!("SELECT CAST(k AS text), pg_typeof(k)::text FROM ({sub}) AS x(k) ORDER BY 1"),
                vec![],
            ))
            .await?;
        Ok(rows
            .rows
            .iter()
            .filter_map(|r| {
                let v = r.first().and_then(Value::as_str)?;
                let ty = r.get(1).and_then(Value::as_str).unwrap_or("text");
                Some(if matches!(ty, "bigint" | "integer" | "smallint") {
                    v.to_owned()
                } else {
                    lit(v)
                })
            })
            .collect())
    }

    async fn identity_covers(&self, schema: &str, table: &str, column: &str) -> Result<bool, DbError> {
        Ok(match pgsql::replica_identity(self.source, schema, table).await?.as_str() {
            "f" => true,
            "d" => pgsql::primary_key(self.source, schema, table)
                .await?
                .iter()
                .any(|c| c == column),
            _ => false,
        })
    }

    async fn table_plan(
        &self,
        schema: &str,
        table: &str,
        feed: Feed,
        filter: Option<String>,
        predicate: String,
        depth: u32,
    ) -> Result<TablePlan, DbError> {
        let cols: Vec<Column> = pgsql::columns(self.source, schema, table)
            .await?
            .into_iter()
            .filter(|c| !c.generated)
            .collect();
        let key = pgsql::primary_key(self.source, schema, table).await?;
        let renumber = match RENUMBERED.iter().find(|(t, _)| schema == SYSTEM_SCHEMA && *t == table) {
            Some((_, column)) => {
                let rows = self
                    .target
                    .fetch_all(&Statement::new(
                        "SELECT pg_catalog.pg_get_serial_sequence(CAST(?1 AS text), CAST(?2 AS text))",
                        vec![qualified(schema, table).into(), (*column).into()],
                    ))
                    .await?;
                rows.scalar().and_then(Value::as_str).map(|s| Renumber {
                    column: (*column).to_owned(),
                    sequence: s.to_owned(),
                })
            }
            None => None,
        };
        let mut unhashed: Vec<String> = REMAPPED
            .iter()
            .filter(|(t, _)| schema == SYSTEM_SCHEMA && *t == table)
            .map(|(_, c)| (*c).to_owned())
            .collect();
        if let Some(r) = &renumber {
            unhashed.push(r.column.clone());
        }
        Ok(TablePlan {
            schema: schema.to_owned(),
            table: table.to_owned(),
            feed,
            filter,
            predicate,
            container: None,
            key,
            columns: cols.iter().map(|c| c.name.clone()).collect(),
            types: cols.iter().map(|c| c.ty.clone()).collect(),
            renumber,
            unhashed,
            depth,
            identity_full: false,
            staging: (feed == Feed::Stage).then(|| self.staging_name()),
        })
    }

    /// User collection tables with a `_tenant` column (tenant-scoped
    /// collections), present on both sets.
    async fn collection_tables(&self) -> Result<Vec<(String, String)>, DbError> {
        let rows = self
            .source
            .fetch_all(&Statement::raw(
                "SELECT c.table_schema::text, c.table_name::text FROM information_schema.columns c \
                 JOIN information_schema.tables t ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
                 WHERE c.column_name = '_tenant' AND t.table_type = 'BASE TABLE' \
                   AND c.table_schema NOT IN ('pg_catalog', 'information_schema', 'pgmq', 'ag_catalog', 'cron', 'fb_move') \
                 ORDER BY 1, 2",
            ))
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            let (Some(schema), Some(table)) = (r.first().and_then(Value::as_str), r.get(1).and_then(Value::as_str)) else {
                continue;
            };
            if schema == SYSTEM_SCHEMA && placement::table(table).is_some() {
                continue;
            }
            if pgsql::table_exists(self.target, schema, table).await? {
                out.push((schema.to_owned(), table.to_owned()));
            }
        }
        Ok(out)
    }

    async fn collection_table(&mut self, schema: &str, table: &str) -> Result<(), DbError> {
        let p = format!("{} = {}", ident(placement::COLLECTION_TENANT_COLUMN), lit(self.tenant));
        let mut plan = self
            .table_plan(schema, table, Feed::Publish, Some(p.clone()), p, 0)
            .await?;
        plan.identity_full = !self
            .identity_covers(schema, table, placement::COLLECTION_TENANT_COLUMN)
            .await?;
        self.tables.push(plan);
        Ok(())
    }

    /// `_forgebase_record_versions`: versions of the tenant's records in
    /// tenant-scoped collections (deleted records: by an earlier version's
    /// `_tenant`).
    async fn record_versions(&mut self, control: &dyn Backend, collections: &[(String, String)]) -> Result<(), DbError> {
        let table = "_forgebase_record_versions";
        if !self.both_have(SYSTEM_SCHEMA, table).await? || collections.is_empty() {
            return Ok(());
        }
        let rows = control
            .fetch_all(&Statement::raw(
                "SELECT id, CAST(name AS text) FROM _forgebase_collections WHERE type <> 'view' \
                 AND definition -> 'options' ->> 'tenant_scoped' = 'true' ORDER BY id",
            ))
            .await?;
        let mut parts = Vec::new();
        let mut ids = Vec::new();
        for r in &rows.rows {
            let (Some(id), Some(name)) = (r.first().and_then(Value::as_str), r.get(1).and_then(Value::as_str)) else {
                continue;
            };
            let Some((schema, physical)) = collections.iter().find(|(_, t)| t.eq_ignore_ascii_case(name)) else {
                continue;
            };
            ids.push(lit(id));
            parts.push(format!(
                "(collection_id = {} AND record_id IN (SELECT CAST(id AS text) FROM {} WHERE _tenant = {}))",
                lit(id),
                qualified(schema, physical),
                lit(self.tenant)
            ));
        }
        if parts.is_empty() {
            return Ok(());
        }
        parts.push(format!(
            "(collection_id IN ({ids}) AND record_id IN (SELECT v.record_id FROM {t} v WHERE v.collection_id IN ({ids}) \
             AND v.data ->> '_tenant' = {tenant}))",
            ids = ids.join(", "),
            t = qualified(SYSTEM_SCHEMA, table),
            tenant = lit(self.tenant)
        ));
        let predicate = format!("({})", parts.join(" OR "));
        let plan = self
            .table_plan(SYSTEM_SCHEMA, table, Feed::Stage, None, predicate, 1)
            .await?;
        self.tables.push(plan);
        Ok(())
    }

    /// The pgmq queues of the moved subscriptions (FB-164: `fbs_` and 128
    /// bits of the subscription name's SHA-256), with their queue and
    /// archive tables as whole tables.
    async fn pgmq_queues(&mut self) -> Result<Vec<String>, DbError> {
        let Some((subs, _)) = self.predicates.get("_forgebase_queue_subscriptions").cloned() else {
            return Ok(Vec::new());
        };
        if !pgsql::table_exists(self.source, "pgmq", "meta").await? {
            return Ok(Vec::new());
        }
        let rows = self
            .source
            .fetch_all(&Statement::raw(format!(
                "SELECT 'fbs_' || left(encode(sha256(convert_to(name, 'UTF8')), 'hex'), 32) FROM {} WHERE {subs} ORDER BY 1",
                qualified(SYSTEM_SCHEMA, "_forgebase_queue_subscriptions")
            )))
            .await?;
        let mut queues = Vec::new();
        for q in pgsql::texts(&rows) {
            let main = format!("q_{q}");
            if !pgsql::table_exists(self.source, "pgmq", &main).await? {
                continue;
            }
            queues.push(q.clone());
            for t in [main, format!("a_{q}")] {
                if !pgsql::table_exists(self.source, "pgmq", &t).await? {
                    continue;
                }
                let mut plan = self.table_plan("pgmq", &t, Feed::Whole, None, "true".into(), 2).await?;
                // The message's `seq` points into the renumbered messages.
                plan.unhashed.push("message".into());
                self.tables.push(plan);
            }
        }
        Ok(queues)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(table: &str, feed: Feed, filter: Option<&str>) -> TablePlan {
        TablePlan {
            schema: SYSTEM_SCHEMA.into(),
            table: table.into(),
            feed,
            filter: filter.map(str::to_owned),
            predicate: "true".into(),
            container: None,
            key: vec!["id".into()],
            columns: vec!["id".into(), "a".into(), "seq".into()],
            types: vec!["text".into(), "text".into(), "bigint".into()],
            renumber: None,
            unhashed: vec!["seq".into()],
            depth: 0,
            identity_full: false,
            staging: None,
        }
    }

    #[test]
    fn publication_lists_published_tables_with_filters_and_the_barrier() {
        let p = MovePlan {
            tenant: "t".into(),
            tables: vec![
                plan("a", Feed::Publish, Some("tenant = 't'")),
                plan("b", Feed::Stage, None),
                plan("c", Feed::Publish, None),
                plan("d", Feed::Whole, None),
            ],
            queues: vec![],
        };
        let list = p.publication_tables("mv_1");
        assert_eq!(
            list,
            vec![
                "\"forgebase\".\"a\" WHERE (tenant = 't')".to_owned(),
                "\"forgebase\".\"d\"".to_owned(),
                "fb_move.barrier WHERE (move_id = 'mv_1')".to_owned(),
            ]
        );
        assert_eq!(p.tables[0].hashed(), vec!["id".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn tenant_keys_follow_topics_and_kv_scopes() {
        assert_eq!(tenant_value("_forgebase_kv_entries", "e1"), "t:e1");
        assert_eq!(tenant_value("_forgebase_jobs", "e1"), "e1");
        assert!(matches!(
            key_of("_forgebase_queue_subscriptions", TenantKey::None),
            TenantKey::Parent { table: "_forgebase_queue_topics", .. }
        ));
        // Every container and renumbered table is a tenant table of the registry.
        for t in CONTAINERS.iter().chain(RENUMBERED.iter().map(|(t, _)| t)) {
            assert_eq!(placement::table(t).map(|p| p.class), Some(PlacementClass::Tenant), "{t}");
        }
        for (t, _) in REMAPPED {
            assert!(placement::table(t).is_some(), "{t}");
        }
    }
}
