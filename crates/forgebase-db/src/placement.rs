//! The placement registry: which set holds the rows of every system table
//! (ADR-0029 §3.2).
//!
//! Every node has the full system schema; the placement class of a table
//! decides which node's copy holds rows:
//!
//! | Class | Rows live on |
//! |---|---|
//! | [`PlacementClass::Control`] | control only |
//! | [`PlacementClass::Tenant`] | the tenant's set; rows whose tenant is `NULL` or `''` on control |
//! | [`PlacementClass::PerSet`] | every set keeps its own (cursors, bookkeeping) |
//! | [`PlacementClass::ControlReplicated`] | control, with read-only copies on every shard |
//!
//! A tenant table finds its tenant through a [`TenantKey`]: its own column
//! (`tenant`, or `_tenant` on collection tables), its parent row
//! (`collection_id → _forgebase_vector_collections.id`), or for record
//! versions the collection record they belong to. The [`MoveStrategy`] says
//! what an online move (FB-174) does with the table's rows.
//!
//! [`problems`] checks the registry against a schema read by
//! [`system_tables`]: every table classified, nothing stale, every key
//! column present, and no table with a tenant column left as `control`
//! by accident. The tests run it on both backends, the same way the
//! schema-parity test keeps the two schemas honest.
//!
//! User collection tables are not in the registry: [`collection_placement`]
//! classifies them from their `tenant_scoped` option.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::backend::{Backend, Statement, Value};
use crate::{BackendKind, Database, DbError};

/// Where a table's rows live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementClass {
    /// Control only: identity, policy, tenancy tree, catalog, functions,
    /// billing, credits, entitlements, customize, topology.
    Control,
    /// The tenant's set (rows without a tenant: control).
    Tenant,
    /// Each set keeps its own rows; its replicas copy them.
    PerSet,
    /// Control, with read-only copies on every shard (logical replication,
    /// publication `fb_control`), for native RLS functions on shards.
    ControlReplicated,
}

impl PlacementClass {
    /// Snake-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            PlacementClass::Control => "control",
            PlacementClass::Tenant => "tenant",
            PlacementClass::PerSet => "per_set",
            PlacementClass::ControlReplicated => "control_replicated",
        }
    }
}

/// How a tenant table's rows find their tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TenantKey {
    /// Not a tenant table.
    None,
    /// The table's own column holds the tenant (`NULL` or `''`: control).
    Column {
        /// Column name.
        column: &'static str,
    },
    /// The parent row's tenant: `column` references `table.parent_column`,
    /// and `table` is a tenant table itself.
    Parent {
        /// Column of this table.
        column: &'static str,
        /// Parent table.
        table: &'static str,
        /// Referenced column of the parent.
        parent_column: &'static str,
    },
    /// The tenant of a collection record: the row names the collection
    /// (`collection_column`, a `_forgebase_collections.id`) and the record
    /// (`record_column`, the `id` of that collection's table, whose
    /// `_tenant` decides).
    Record {
        /// Column holding the collection id.
        collection_column: &'static str,
        /// Column holding the record id.
        record_column: &'static str,
    },
}

/// What an online tenant move does with a table (ADR-0029 §3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveStrategy {
    /// Not moved (control, per-set and replicated tables).
    Stay,
    /// The tenant's rows are copied with their ids (identity ranges never
    /// collide, §3.3).
    Copy,
    /// Order-defining: rows are staged on the target, then inserted in source
    /// order with the target's sequence; references to them are remapped.
    Renumber,
    /// Copied, but holds references into a renumbered table (cursors,
    /// delivery and snapshot positions) that are remapped at the switch.
    Remap,
    /// Derived: not copied, rebuilt on the target from moved rows.
    Rebuild,
}

/// The placement of one system table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TablePlacement {
    /// Table name (schema `forgebase` on PostgreSQL).
    pub table: &'static str,
    /// Where its rows live.
    pub class: PlacementClass,
    /// How rows find their tenant ([`TenantKey::None`] unless `Tenant`).
    pub tenant_key: TenantKey,
    /// What a tenant move does with it.
    pub moves: MoveStrategy,
}

impl TablePlacement {
    /// Whether rows of this table can belong to a tenant on a shard.
    pub fn holds_tenant_rows(&self) -> bool {
        self.class == PlacementClass::Tenant
    }

    /// The tenant column, for [`TenantKey::Column`] tables.
    pub fn tenant_column(&self) -> Option<&'static str> {
        match self.tenant_key {
            TenantKey::Column { column } => Some(column),
            _ => None,
        }
    }
}

const fn control(table: &'static str) -> TablePlacement {
    TablePlacement {
        table,
        class: PlacementClass::Control,
        tenant_key: TenantKey::None,
        moves: MoveStrategy::Stay,
    }
}

const fn per_set(table: &'static str, moves: MoveStrategy) -> TablePlacement {
    TablePlacement {
        table,
        class: PlacementClass::PerSet,
        tenant_key: TenantKey::None,
        moves,
    }
}

const fn replicated(table: &'static str) -> TablePlacement {
    TablePlacement {
        table,
        class: PlacementClass::ControlReplicated,
        tenant_key: TenantKey::None,
        moves: MoveStrategy::Stay,
    }
}

const fn tenant(table: &'static str, column: &'static str, moves: MoveStrategy) -> TablePlacement {
    TablePlacement {
        table,
        class: PlacementClass::Tenant,
        tenant_key: TenantKey::Column { column },
        moves,
    }
}

const fn child(
    table: &'static str,
    column: &'static str,
    parent: &'static str,
    parent_column: &'static str,
    moves: MoveStrategy,
) -> TablePlacement {
    TablePlacement {
        table,
        class: PlacementClass::Tenant,
        tenant_key: TenantKey::Parent {
            column,
            table: parent,
            parent_column,
        },
        moves,
    }
}

const COPY: MoveStrategy = MoveStrategy::Copy;
const RENUMBER: MoveStrategy = MoveStrategy::Renumber;
const REMAP: MoveStrategy = MoveStrategy::Remap;
const REBUILD: MoveStrategy = MoveStrategy::Rebuild;

/// Every system table, by name. A table added by a migration must be added
/// here; the completeness test names it.
pub static TABLES: &[TablePlacement] = &[
    // --- Identity and sessions (control) ---
    control("_forgebase_admins"),
    control("_forgebase_auth_account_status"),
    // `tenant` binds a key to a tenant; keys are checked on control.
    control("_forgebase_auth_api_keys"),
    control("_forgebase_auth_audit"),
    control("_forgebase_auth_keys"),
    control("_forgebase_auth_lockouts"),
    control("_forgebase_auth_mfa"),
    control("_forgebase_auth_one_time"),
    control("_forgebase_auth_phones"),
    control("_forgebase_auth_recovery_codes"),
    control("_forgebase_auth_refresh_tokens"),
    control("_forgebase_auth_sessions"),
    control("_forgebase_auth_sms_codes"),
    control("_forgebase_oauth_clients"),
    control("_forgebase_oauth_codes"),
    control("_forgebase_oauth_consents"),
    control("_forgebase_oauth_device_codes"),
    control("_forgebase_oauth_families"),
    control("_forgebase_oauth_identities"),
    control("_forgebase_oauth_providers"),
    control("_forgebase_oauth_refresh_tokens"),
    control("_forgebase_oauth_requests"),
    control("_forgebase_oauth_revoked_tokens"),
    control("_forgebase_oauth_states"),
    control("_forgebase_oidc_keys"),
    control("_forgebase_realm_identity"),
    control("_forgebase_service_accounts"),
    control("_forgebase_sms_config"),
    control("_forgebase_suspensions"),
    control("_forgebase_webauthn_challenges"),
    control("_forgebase_webauthn_credentials"),
    // --- Policy and the tenancy tree (control) ---
    // `tenant` records the request's tenant; the audit log is control's.
    control("_forgebase_authz_audit"),
    control("_forgebase_environments"),
    control("_forgebase_invitations"),
    control("_forgebase_memberships"),
    control("_forgebase_orgs"),
    control("_forgebase_permission_overrides"),
    control("_forgebase_projects"),
    control("_forgebase_realm_bindings"),
    control("_forgebase_realms"),
    control("_forgebase_resource_rules"),
    control("_forgebase_role_sets"),
    control("_forgebase_roles"),
    control("_forgebase_team_members"),
    control("_forgebase_teams"),
    // Record ACL: native RLS (`forgebase.acl()`) reads it on every shard.
    replicated("_forgebase_resource_permissions"),
    // Direct logins' claims: `forgebase.claims()` reads them on every shard
    // a login connects to (FB-153).
    replicated("_forgebase_db_logins"),
    // --- Data-core catalog (control; DDL fans out to every set, FB-173) ---
    control("_forgebase_collection_policies"),
    control("_forgebase_collections"),
    control("_forgebase_indexes"),
    // Per-set state of index builds (FB-173), read and written on control.
    control("_forgebase_index_builds"),
    control("_forgebase_schema_history"),
    // Prior versions of collection records: with the record's tenant.
    TablePlacement {
        table: "_forgebase_record_versions",
        class: PlacementClass::Tenant,
        tenant_key: TenantKey::Record {
            collection_column: "collection_id",
            record_column: "record_id",
        },
        moves: COPY,
    },
    // --- Functions (control) ---
    control("_forgebase_function_kv_rules"),
    control("_forgebase_function_logs"),
    control("_forgebase_function_secrets"),
    control("_forgebase_function_versions"),
    control("_forgebase_functions"),
    // After-hook cursors over each set's realtime outbox.
    per_set("_forgebase_function_cursors", MoveStrategy::Stay),
    // --- Billing, credits, entitlements, usage (control) ---
    control("_forgebase_addon_features"),
    control("_forgebase_addons"),
    control("_forgebase_billing_audit"),
    control("_forgebase_billing_coupons"),
    control("_forgebase_billing_credit_notes"),
    control("_forgebase_billing_cursors"),
    control("_forgebase_billing_customers"),
    control("_forgebase_billing_invoice_lines"),
    control("_forgebase_billing_invoices"),
    control("_forgebase_billing_orders"),
    control("_forgebase_billing_payments"),
    control("_forgebase_billing_prices"),
    control("_forgebase_billing_products"),
    control("_forgebase_billing_providers"),
    control("_forgebase_billing_refunds"),
    control("_forgebase_billing_sequences"),
    control("_forgebase_billing_settings"),
    control("_forgebase_billing_store_products"),
    control("_forgebase_billing_store_purchases"),
    control("_forgebase_billing_subscription_items"),
    control("_forgebase_billing_subscriptions"),
    control("_forgebase_billing_sync_events"),
    control("_forgebase_billing_tax_rates"),
    control("_forgebase_billing_usage_records"),
    control("_forgebase_billing_webhook_events"),
    control("_forgebase_credit_accounts"),
    control("_forgebase_credit_allocations"),
    control("_forgebase_credit_cursors"),
    control("_forgebase_credit_ledger"),
    control("_forgebase_credit_lots"),
    control("_forgebase_credit_rates"),
    control("_forgebase_credit_usage"),
    control("_forgebase_custom_limits"),
    control("_forgebase_entitlement_audit"),
    control("_forgebase_entitlement_overrides"),
    control("_forgebase_features"),
    control("_forgebase_pack_grants"),
    control("_forgebase_plan_features"),
    control("_forgebase_plans"),
    control("_forgebase_quota_leases"),
    control("_forgebase_quota_policies"),
    control("_forgebase_subscription_addons"),
    control("_forgebase_subscriptions"),
    control("_forgebase_usage_alerts"),
    control("_forgebase_usage_counters"),
    control("_forgebase_usage_events"),
    control("_forgebase_usage_packs"),
    control("_forgebase_usage_reservations"),
    // --- Customize (control) ---
    control("_forgebase_agreement_acceptances"),
    control("_forgebase_agreements"),
    control("_forgebase_branding"),
    control("_forgebase_branding_assets"),
    control("_forgebase_domains"),
    control("_forgebase_email_deliveries"),
    control("_forgebase_email_templates"),
    // --- Topology (control, FB-170) ---
    control("_forgebase_db_moves"),
    control("_forgebase_db_nodes"),
    control("_forgebase_db_sets"),
    control("_forgebase_join_tokens"),
    control("_forgebase_tenant_placements"),
    control("_forgebase_xact_journal"),
    // --- Node bookkeeping (per set) ---
    per_set("_forgebase_meta", MoveStrategy::Stay),
    per_set("_forgebase_migrations", MoveStrategy::Stay),
    // Each set's change log; `tenant` says whose change it was. A move
    // resets the tenant's live subscriptions (`tenant_moved`).
    per_set("_forgebase_realtime_outbox", MoveStrategy::Stay),
    // --- Key-value ---
    tenant("_forgebase_kv_entries", "tenant", COPY),
    // --- Graph (canonical tables; the AGE graph is derived and rebuilt) ---
    tenant("_forgebase_graph_edges", "tenant", COPY),
    tenant("_forgebase_graph_nodes", "tenant", COPY),
    // --- Vector ---
    tenant("_forgebase_vector_collections", "tenant", COPY),
    child(
        "_forgebase_vector_fields",
        "collection_id",
        "_forgebase_vector_collections",
        "id",
        COPY,
    ),
    child(
        "_forgebase_vector_items",
        "collection_id",
        "_forgebase_vector_collections",
        "id",
        COPY,
    ),
    child(
        "_forgebase_vector_values",
        "collection_id",
        "_forgebase_vector_collections",
        "id",
        COPY,
    ),
    // Keyword index (PostgreSQL table; SQLite FTS5). Logical apply fires no
    // triggers, so the rows are copied with their items.
    child("_forgebase_vector_fts", "rowid", "_forgebase_vector_items", "key", COPY),
    // --- Jobs (G3: `tenant` since 0030_job_tenant) ---
    tenant("_forgebase_jobs", "tenant", COPY),
    tenant("_forgebase_job_schedules", "tenant", COPY),
    tenant("_forgebase_job_workflows", "tenant", COPY),
    child("_forgebase_job_attempts", "job_id", "_forgebase_jobs", "id", RENUMBER),
    child("_forgebase_job_deps", "job_id", "_forgebase_jobs", "id", COPY),
    child("_forgebase_job_events", "job_id", "_forgebase_jobs", "id", RENUMBER),
    // Queue settings and rate windows: the claim SQL of each set reads its
    // own copy (FB-172 keeps the settings in step with control).
    per_set("_forgebase_job_queues", MoveStrategy::Stay),
    // HTTP workers register once and claim round-robin across sets.
    control("_forgebase_job_workers"),
    // --- Queues (G16: `tenant` on topics, subscriptions and snapshots since 0031_queue_rls) ---
    tenant("_forgebase_queue_topics", "tenant", COPY),
    tenant("_forgebase_queue_subscriptions", "tenant", REMAP),
    child(
        "_forgebase_queue_messages",
        "topic",
        "_forgebase_queue_topics",
        "name",
        RENUMBER,
    ),
    child(
        "_forgebase_queue_dedup",
        "topic",
        "_forgebase_queue_topics",
        "name",
        COPY,
    ),
    child(
        "_forgebase_queue_deliveries",
        "subscription",
        "_forgebase_queue_subscriptions",
        "name",
        REMAP,
    ),
    child(
        "_forgebase_queue_dead_letters",
        "subscription",
        "_forgebase_queue_subscriptions",
        "name",
        COPY,
    ),
    tenant("_forgebase_queue_snapshots", "tenant", REMAP),
    child(
        "_forgebase_queue_snapshot_items",
        "snapshot",
        "_forgebase_queue_snapshots",
        "name",
        REMAP,
    ),
    // Rate windows (restart on the target) and relay cursors over each
    // set's outbox (record-source topics on control keep one per set).
    per_set("_forgebase_queue_rates", MoveStrategy::Stay),
    per_set("_forgebase_queue_state", MoveStrategy::Stay),
    // --- Storage (G14: `tenant` on buckets since 0029_storage_rls) ---
    tenant("_forgebase_storage_buckets", "tenant", COPY),
    child(
        "_forgebase_storage_acl",
        "bucket",
        "_forgebase_storage_buckets",
        "name",
        COPY,
    ),
    child(
        "_forgebase_storage_objects",
        "bucket",
        "_forgebase_storage_buckets",
        "name",
        COPY,
    ),
    child(
        "_forgebase_storage_uploads",
        "bucket",
        "_forgebase_storage_buckets",
        "name",
        COPY,
    ),
    child(
        "_forgebase_storage_parts",
        "upload_id",
        "_forgebase_storage_uploads",
        "id",
        COPY,
    ),
    // Content-addressed blob reference counts across every bucket of the set:
    // recounted on the target from the moved objects.
    per_set("_forgebase_storage_blobs", REBUILD),
    // Instance-wide storage settings (URL signing key).
    control("_forgebase_storage_settings"),
];

/// SQLite-only tables (FTS5 shadow tables of `_forgebase_vector_fts`): not
/// in the PostgreSQL schema, so not in the registry.
pub const SQLITE_ONLY_TABLES: &[&str] = &[
    "_forgebase_vector_fts_config",
    "_forgebase_vector_fts_data",
    "_forgebase_vector_fts_docsize",
    "_forgebase_vector_fts_idx",
];

/// Column names that mark a tenant column ([`problems`] checks each one is
/// a placement key or listed in [`TENANT_COLUMNS_NOT_KEYS`]).
pub const TENANT_COLUMN_NAMES: &[&str] = &["tenant", "_tenant"];

/// Tables with a tenant column that is deliberately **not** their placement
/// key, with the reason.
pub const TENANT_COLUMNS_NOT_KEYS: &[(&str, &str, &str)] = &[
    (
        "_forgebase_db_logins",
        "tenant",
        "the one tenant a direct login is bound to (FB-153); logins are control rows copied to every shard",
    ),
    (
        "_forgebase_auth_api_keys",
        "tenant",
        "the tenant a key is bound to; keys are control rows",
    ),
    (
        "_forgebase_authz_audit",
        "tenant",
        "the request's tenant; the audit log is control's",
    ),
    (
        "_forgebase_job_queues",
        "tenant",
        "queue settings keyed <tenant>/<queue> since 0035; each set keeps its own copy (FB-172 keeps them in step)",
    ),
    (
        "_forgebase_job_workers",
        "tenant",
        "the tenant a worker registered for since 0035; workers are control rows",
    ),
    (
        "_forgebase_realtime_outbox",
        "tenant",
        "whose change it was; each set keeps its own outbox",
    ),
    (
        "_forgebase_tenant_placements",
        "tenant",
        "the topology row about a tenant, on control",
    ),
    (
        "_forgebase_db_moves",
        "tenant",
        "the topology row about a tenant's move, on control",
    ),
];

/// Tenant columns that the registry already uses as placement keys but that
/// a package in flight adds: until it merges, [`problems`] accepts the
/// missing column. `(table, column, package)`.
pub const PENDING_TENANT_COLUMNS: &[(&str, &str, &str)] = &[];

/// The registry entry of a system table.
pub fn table(name: &str) -> Option<&'static TablePlacement> {
    TABLES.iter().find(|t| t.table == name)
}

/// The registry entries of one class.
pub fn tables_of(class: PlacementClass) -> impl Iterator<Item = &'static TablePlacement> {
    TABLES.iter().filter(move |t| t.class == class)
}

/// Column of user collection tables with `tenant_scoped: true`.
pub const COLLECTION_TENANT_COLUMN: &str = "_tenant";

/// Class and tenant key of a user collection table: tenant-scoped
/// collections are tenant tables keyed by `_tenant`; others live on control.
pub fn collection_placement(tenant_scoped: bool) -> (PlacementClass, TenantKey) {
    if tenant_scoped {
        (
            PlacementClass::Tenant,
            TenantKey::Column {
                column: COLLECTION_TENANT_COLUMN,
            },
        )
    } else {
        (PlacementClass::Control, TenantKey::None)
    }
}

/// System tables and their columns, as [`system_tables`] reads them.
pub type SchemaTables = BTreeMap<String, BTreeSet<String>>;

/// Reads every system table and its columns: schema `forgebase` on
/// PostgreSQL, tables named `_forgebase_*` on SQLite (FTS5 shadow tables
/// left out).
pub async fn system_tables(db: &Database) -> Result<SchemaTables, DbError> {
    let stmt = match db.kind() {
        BackendKind::Postgres => Statement::raw(
            "SELECT c.relname::text, a.attname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
             WHERE n.nspname = 'forgebase' AND c.relkind IN ('r', 'p') ORDER BY 1, a.attnum",
        ),
        BackendKind::Sqlite => Statement::raw(
            // A virtual table (FTS5) keys its rows by its hidden rowid.
            "SELECT m.name, p.name FROM sqlite_schema m, pragma_table_info(m.name) p \
             WHERE m.type = 'table' AND m.name LIKE '\\_forgebase\\_%' ESCAPE '\\' \
             UNION ALL SELECT name, 'rowid' FROM sqlite_schema \
             WHERE type = 'table' AND name LIKE '\\_forgebase\\_%' ESCAPE '\\' AND sql LIKE 'CREATE VIRTUAL TABLE%' \
             ORDER BY 1",
        ),
    };
    let rows = db.fetch_all(&stmt).await?;
    let mut out = SchemaTables::new();
    for row in &rows.rows {
        let (Some(Value::Text(table)), Some(Value::Text(column))) = (row.first(), row.get(1)) else {
            continue;
        };
        if SQLITE_ONLY_TABLES.contains(&table.as_str()) {
            continue;
        }
        out.entry(table.clone()).or_default().insert(column.clone());
    }
    Ok(out)
}

/// Checks [`TABLES`] against `schema` and returns every problem found
/// (empty = complete and consistent):
///
/// * a schema table missing from the registry, or a registry entry not in
///   the schema, or listed twice;
/// * a class and key that do not fit (a tenant table without a key, a
///   control table with one, a tenant table that is never moved);
/// * a key column, parent table or parent column that does not exist (a
///   column in [`PENDING_TENANT_COLUMNS`] may be missing), or a parent that
///   is not a tenant table;
/// * a tenant column ([`TENANT_COLUMN_NAMES`]) that is neither the table's
///   key nor listed in [`TENANT_COLUMNS_NOT_KEYS`].
pub fn problems(schema: &SchemaTables) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for t in TABLES {
        if !seen.insert(t.table) {
            out.push(format!("placement::TABLES lists {} twice", t.table));
        }
    }
    for name in schema.keys() {
        if table(name).is_none() {
            out.push(format!(
                "table {name} has no placement class: add it to forgebase_db::placement::TABLES (ADR-0029 §3.2)"
            ));
        }
    }
    let has = |table: &str, column: &str| schema.get(table).is_some_and(|c| c.contains(column));
    let pending = |table: &str, column: &str| {
        PENDING_TENANT_COLUMNS
            .iter()
            .any(|(t, c, _)| *t == table && *c == column)
    };
    for t in TABLES {
        let Some(columns) = schema.get(t.table) else {
            out.push(format!(
                "placement::TABLES lists {}, which is not in the schema: remove it",
                t.table
            ));
            continue;
        };
        match (t.class, t.tenant_key) {
            (PlacementClass::Tenant, TenantKey::None) => {
                out.push(format!("{} is a tenant table without a tenant key", t.table));
            }
            (PlacementClass::Tenant, _) if t.moves == MoveStrategy::Stay => {
                out.push(format!("{} is a tenant table that a move would leave behind", t.table));
            }
            (PlacementClass::Tenant, _) => {}
            (_, TenantKey::None) => {}
            (class, _) => out.push(format!("{} is a {} table with a tenant key", t.table, class.as_str())),
        }
        match t.tenant_key {
            TenantKey::None => {}
            TenantKey::Column { column } => {
                if !columns.contains(column) && !pending(t.table, column) {
                    out.push(format!("{}: tenant column {column} does not exist", t.table));
                }
            }
            TenantKey::Parent {
                column,
                table: parent,
                parent_column,
            } => {
                if !columns.contains(column) {
                    out.push(format!("{}: parent key column {column} does not exist", t.table));
                }
                match table(parent) {
                    Some(p) if p.class == PlacementClass::Tenant => {}
                    _ => out.push(format!("{}: parent {parent} is not a tenant table", t.table)),
                }
                if !has(parent, parent_column) {
                    out.push(format!(
                        "{}: parent column {parent}.{parent_column} does not exist",
                        t.table
                    ));
                }
            }
            TenantKey::Record {
                collection_column,
                record_column,
            } => {
                for c in [collection_column, record_column] {
                    if !columns.contains(c) {
                        out.push(format!("{}: record key column {c} does not exist", t.table));
                    }
                }
            }
        }
        for column in TENANT_COLUMN_NAMES {
            if !columns.contains(*column) || t.tenant_column() == Some(*column) {
                continue;
            }
            let exempt = TENANT_COLUMNS_NOT_KEYS
                .iter()
                .any(|(tbl, c, _)| *tbl == t.table && c == column);
            if !exempt {
                out.push(format!(
                    "{} has a {column} column but is classified {}: make it a tenant table keyed by {column}, \
                     or list it in placement::TENANT_COLUMNS_NOT_KEYS with the reason",
                    t.table,
                    t.class.as_str()
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_of(entries: &[(&str, &[&str])]) -> SchemaTables {
        entries
            .iter()
            .map(|(t, cols)| (t.to_string(), cols.iter().map(|c| c.to_string()).collect()))
            .collect()
    }

    #[test]
    fn registry_is_internally_consistent() {
        let mut names = BTreeSet::new();
        for t in TABLES {
            assert!(names.insert(t.table), "{} listed twice", t.table);
            assert!(t.table.starts_with("_forgebase_"), "{}", t.table);
            match t.class {
                PlacementClass::Tenant => {
                    assert_ne!(t.tenant_key, TenantKey::None, "{}", t.table);
                    assert_ne!(t.moves, MoveStrategy::Stay, "{}", t.table);
                }
                _ => {
                    assert_eq!(t.tenant_key, TenantKey::None, "{}", t.table);
                    assert!(
                        matches!(t.moves, MoveStrategy::Stay | MoveStrategy::Rebuild),
                        "{}",
                        t.table
                    );
                }
            }
            if let TenantKey::Parent { table: parent, .. } = t.tenant_key {
                assert_eq!(
                    table(parent).map(|p| p.class),
                    Some(PlacementClass::Tenant),
                    "{} -> {parent}",
                    t.table
                );
            }
        }
        for (t, _, _) in TENANT_COLUMNS_NOT_KEYS.iter().chain(PENDING_TENANT_COLUMNS) {
            assert!(table(t).is_some(), "{t}");
        }
        for t in SQLITE_ONLY_TABLES {
            assert!(table(t).is_none(), "{t}");
        }
    }

    #[test]
    fn collection_tables_follow_tenant_scoped() {
        assert_eq!(
            collection_placement(true),
            (PlacementClass::Tenant, TenantKey::Column { column: "_tenant" })
        );
        assert_eq!(collection_placement(false), (PlacementClass::Control, TenantKey::None));
    }

    #[test]
    fn problems_name_what_is_wrong() {
        // Start from a schema that matches the registry exactly.
        let mut schema = SchemaTables::new();
        for t in TABLES {
            let cols = schema.entry(t.table.to_owned()).or_default();
            cols.insert("id".into());
            match t.tenant_key {
                TenantKey::Column { column } => {
                    cols.insert(column.into());
                }
                TenantKey::Parent { column, .. } => {
                    cols.insert(column.into());
                }
                TenantKey::Record {
                    collection_column,
                    record_column,
                } => {
                    cols.insert(collection_column.into());
                    cols.insert(record_column.into());
                }
                TenantKey::None => {}
            }
        }
        for (t, c, _) in TENANT_COLUMNS_NOT_KEYS {
            schema.get_mut(*t).unwrap().insert((*c).into());
        }
        for t in TABLES {
            if let TenantKey::Parent {
                table: parent,
                parent_column,
                ..
            } = t.tenant_key
            {
                schema.get_mut(parent).unwrap().insert(parent_column.into());
            }
        }
        assert_eq!(problems(&schema), Vec::<String>::new());

        // A new table, a removed one, a new tenant column on a control table,
        // and a missing key column.
        let mut bad = schema.clone();
        bad.extend(schema_of(&[("_forgebase_new_thing", &["id"])]));
        bad.remove("_forgebase_admins");
        bad.get_mut("_forgebase_functions").unwrap().insert("tenant".into());
        bad.get_mut("_forgebase_kv_entries").unwrap().remove("tenant");
        let found = problems(&bad);
        assert_eq!(found.len(), 4, "{found:#?}");
        assert!(
            found[0].contains("_forgebase_new_thing has no placement class"),
            "{found:#?}"
        );
        assert!(
            found
                .iter()
                .any(|p| p.contains("lists _forgebase_admins, which is not in the schema"))
        );
        assert!(
            found
                .iter()
                .any(|p| p.contains("_forgebase_functions has a tenant column"))
        );
        assert!(
            found
                .iter()
                .any(|p| p.contains("_forgebase_kv_entries: tenant column tenant does not exist"))
        );
    }
}
