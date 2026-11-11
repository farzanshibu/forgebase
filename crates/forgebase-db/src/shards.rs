//! Schema rollout over database sets (ADR-0029 §3.7, §3.9, FB-173):
//!
//! * [`migrate_sets`]: system migrations on control, then on every shard
//!   (at most four at a time); a shard below head is `migrating` meanwhile,
//!   so its tenants get `503 shard_migrating`;
//! * [`activate_shard`]: what a new shard needs before it takes tenants
//!   (migrate, sequence ranges, catalog replay, the `fb_control`
//!   subscription, `active`);
//! * the `control_replicated` tables ([`CONTROL_PUBLICATION`]): published
//!   on control, one subscription per shard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use forgebase_core::ErrorCode;
use forgebase_migrations::multi::{
    self, BoxFuture as MigrationFuture, MigrationHooks, MigrationNode, MultiOptions, MultiReport, NodeError,
};
use forgebase_migrations::{AppliedMigration, MigrationStatus};
use serde::Serialize;

use crate::backend::{Backend, BoxFuture, Statement, Value};
use crate::placement::{self, PlacementClass};
use crate::router::{DbRouter, RouterError, SetHandle};
use crate::topology::{self, DbSet, SetKind, SetState};
use crate::{BackendKind, Database, DbError, SecretString};

impl From<DbError> for NodeError {
    fn from(e: DbError) -> Self {
        NodeError::new(e.code(), e.to_string())
    }
}

/// One set's primary as a [`MigrationNode`].
struct SetNode {
    handle: SetHandle,
    backups: Option<PathBuf>,
}

impl MigrationNode for SetNode {
    fn id(&self) -> &str {
        self.handle.id()
    }

    fn status(&self) -> MigrationFuture<'_, Result<MigrationStatus, NodeError>> {
        Box::pin(async move { Ok(self.handle.database().migration_status().await?) })
    }

    fn migrate(&self) -> MigrationFuture<'_, Result<Vec<AppliedMigration>, NodeError>> {
        Box::pin(async move {
            let report = self.handle.database().migrate(self.backups.as_deref()).await?;
            Ok(report.applied)
        })
    }
}

/// Marks a shard `migrating` before its migrations and back after.
struct MarkMigrating<'a> {
    router: &'a DbRouter,
    before: Mutex<HashMap<String, SetState>>,
}

impl MigrationHooks for MarkMigrating<'_> {
    fn starting<'a>(&'a self, node: &'a str) -> MigrationFuture<'a, Result<(), NodeError>> {
        Box::pin(async move {
            let state = self.router.set(node).map(|h| h.set().state).unwrap_or(SetState::Active);
            if state != SetState::Migrating {
                self.before
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(node.to_owned(), state);
            }
            mark(self.router, node, SetState::Migrating).await
        })
    }

    fn finished<'a>(&'a self, node: &'a str, ok: bool) -> MigrationFuture<'a, ()> {
        Box::pin(async move {
            if !ok {
                // Below head: its tenants keep getting `shard_migrating`.
                return;
            }
            let back = self
                .before
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(node)
                .unwrap_or(SetState::Active);
            if let Err(e) = mark(self.router, node, back).await {
                tracing::warn!(set = %node, error = %e, "could not mark the migrated set back");
            }
        })
    }
}

async fn mark(router: &DbRouter, set: &str, state: SetState) -> Result<(), NodeError> {
    topology::update_set(router.control().database(), set, Some(state), None, None).await?;
    router
        .reload()
        .await
        .map_err(|e| NodeError::new(e.code(), e.to_string()))
}

/// Where a set's pre-migration snapshots go: `<backups_dir>/sets/<set id>`
/// (control keeps `backups_dir` itself).
pub fn set_backups_dir(backups_dir: &Path, set: &DbSet) -> PathBuf {
    if set.is_control() {
        backups_dir.to_owned()
    } else {
        backups_dir.join("sets").join(&set.id)
    }
}

/// Migrates control, then every open shard of `router` (at most
/// `options.max_parallel` at a time). A shard with pending migrations is
/// `migrating` until they are applied (its tenants get `503
/// shard_migrating`); one that fails stays `migrating`. A shard left
/// `migrating` by an earlier run that is now at head is made `active`.
/// Single-set routers migrate control only.
pub async fn migrate_sets(router: &DbRouter, backups_dir: Option<&Path>, options: MultiOptions) -> MultiReport {
    let node = |h: SetHandle| SetNode {
        backups: backups_dir.map(|d| set_backups_dir(d, h.set())),
        handle: h,
    };
    let control = node(router.control());
    let shards: Vec<SetNode> = router
        .sets()
        .into_iter()
        .filter(|h| !h.is_control())
        .map(node)
        .collect();
    let refs: Vec<&dyn MigrationNode> = shards.iter().map(|s| s as &dyn MigrationNode).collect();
    let hooks = MarkMigrating {
        router,
        before: Mutex::new(HashMap::new()),
    };
    let report = multi::migrate_all(&control, &refs, options, &hooks).await;
    for (s, r) in shards.iter().zip(&report.shards) {
        if r.is_ok()
            && router
                .set(s.handle.id())
                .is_some_and(|h| h.set().state == SetState::Migrating)
            && let Err(e) = mark(router, s.handle.id(), SetState::Active).await
        {
            tracing::warn!(set = %s.handle.id(), error = %e, "could not mark the set active");
        }
    }
    report
}

/// Replays the collection catalog onto a new shard: every collection's
/// table, indexes, views and native policies, in one transaction
/// (`forgebase-data` implements it, `DataService::replay_catalog`).
pub trait CatalogReplay: Send + Sync {
    /// Creates the catalog's objects on `target` (empty, migrated).
    fn replay<'a>(&'a self, target: &'a Database) -> BoxFuture<'a, Result<(), DbError>>;
}

/// What [`activate_shard`] needs besides the router.
#[derive(Clone, Copy)]
pub struct ShardSetup<'a> {
    /// Pre-migration snapshots (`<dir>/sets/<id>`), if any.
    pub backups_dir: Option<&'a Path>,
    /// Creates the collections (none: the catalog is empty).
    pub replay: Option<&'a dyn CatalogReplay>,
    /// How the shard reaches control for the `fb_control` subscription
    /// (a libpq connection string with credentials); `None` skips it.
    pub control_conninfo: Option<&'a SecretString>,
}

impl std::fmt::Debug for ShardSetup<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardSetup")
            .field("backups_dir", &self.backups_dir)
            .field("replay", &self.replay.is_some())
            .field("control_conninfo", &self.control_conninfo.map(|_| "<redacted>"))
            .finish()
    }
}

/// Makes a provisioned shard take tenants (ADR-0029 §3.7, §3.9): checks
/// that the node can take part in two-phase commits, migrates it, confines
/// control's sequences to set 0 when this is the first shard, confines the
/// shard's to its own range, replays the catalog, subscribes it to
/// `fb_control`, marks it `active` and reloads the router. `db` is the
/// shard's primary, opened by the caller (the router does not open
/// `provisioning` sets). Safe to run again after a failure.
pub async fn activate_shard(
    router: &DbRouter,
    set: &DbSet,
    db: &Database,
    setup: ShardSetup<'_>,
) -> Result<DbSet, RouterError> {
    let control = router.control().database().clone();
    if set.kind != SetKind::Shard {
        return Err(RouterError::Unsupported(format!("{} is not a shard", set.id)));
    }
    if db.kind() != BackendKind::Postgres || control.kind() != BackendKind::Postgres {
        return Err(RouterError::PostgresRequired);
    }
    for (name, node) in [("control", &control), (set.id.as_str(), db)] {
        if node.max_prepared_transactions().await? <= 0 {
            return Err(RouterError::Unsupported(format!(
                "database set {name:?} has max_prepared_transactions = 0; shards need it above 0 (managed nodes use 16)"
            )));
        }
    }
    db.migrate(setup.backups_dir.map(|d| set_backups_dir(d, set)).as_deref())
        .await?;
    let topo = topology::load(&control).await?;
    let first = !topo
        .sets
        .iter()
        .any(|s| s.kind == SetKind::Shard && s.id != set.id && s.state != SetState::Retired);
    if first {
        topology::apply_sequence_range(&control, 0).await?;
    }
    topology::apply_sequence_range(db, set.number).await?;
    if let Some(replay) = setup.replay {
        replay.replay(db).await?;
    }
    if let Some(conninfo) = setup.control_conninfo {
        ensure_control_publication(&control).await?;
        subscribe_to_control(db, &control, set.number, conninfo).await?;
    }
    let updated = topology::update_set(&control, &set.id, Some(SetState::Active), None, None)
        .await?
        .ok_or_else(|| RouterError::UnknownSet(set.id.clone()))?;
    router.reload().await?;
    Ok(updated)
}

/// Publication of the `control_replicated` tables on control.
pub const CONTROL_PUBLICATION: &str = "fb_control";

/// The subscription (and replication slot on control) of set `number`.
pub fn control_subscription_name(number: u16) -> String {
    format!("fb_control_{number}")
}

/// The `control_replicated` tables of the placement registry.
pub fn control_replicated_tables() -> Vec<&'static str> {
    placement::TABLES
        .iter()
        .filter(|t| t.class == PlacementClass::ControlReplicated)
        .map(|t| t.table)
        .collect()
}

async fn existing_tables(db: &Database, names: &[&str]) -> Result<Vec<String>, DbError> {
    let mut out = Vec::new();
    for n in names {
        let rows = db
            .fetch_all(&Statement::new(
                "SELECT 1 FROM pg_catalog.pg_tables WHERE schemaname = 'forgebase' AND tablename = ?1",
                vec![(*n).into()],
            ))
            .await?;
        if !rows.is_empty() {
            out.push((*n).to_owned());
        }
    }
    Ok(out)
}

/// Creates (or brings up to date) the publication [`CONTROL_PUBLICATION`]
/// of the `control_replicated` tables that exist on control. Returns them.
/// Control needs `wal_level = logical` for subscribers.
pub async fn ensure_control_publication(control: &Database) -> Result<Vec<String>, DbError> {
    if control.kind() != BackendKind::Postgres {
        return Err(DbError::unsupported("logical replication", control.kind()));
    }
    let tables = existing_tables(control, &control_replicated_tables()).await?;
    let list = tables
        .iter()
        .map(|t| format!("forgebase.{t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let exists = !control
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_publication WHERE pubname = ?1",
            vec![CONTROL_PUBLICATION.into()],
        ))
        .await?
        .is_empty();
    let sql = match (exists, tables.is_empty()) {
        (false, true) => format!("CREATE PUBLICATION {CONTROL_PUBLICATION}"),
        (false, false) => format!("CREATE PUBLICATION {CONTROL_PUBLICATION} FOR TABLE {list}"),
        (true, true) => return Ok(tables),
        (true, false) => format!("ALTER PUBLICATION {CONTROL_PUBLICATION} SET TABLE {list}"),
    };
    control.run_autocommit(&[sql]).await?;
    Ok(tables)
}

fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Subscribes shard `number` to [`CONTROL_PUBLICATION`] (copying the
/// current rows first). The replication slot is created on control
/// beforehand (`pg_create_logical_replication_slot`), which also works when
/// both databases are in one cluster. `conninfo` is how the shard reaches
/// control. `Ok(false)` when the subscription exists already. The shard's
/// copies must be empty (a new shard).
pub async fn subscribe_to_control(
    shard: &Database,
    control: &Database,
    number: u16,
    conninfo: &SecretString,
) -> Result<bool, DbError> {
    let name = control_subscription_name(number);
    let exists = !shard
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_subscription WHERE subname = ?1 AND subdbid = \
             (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())",
            vec![name.clone().into()],
        ))
        .await?
        .is_empty();
    if exists {
        return Ok(false);
    }
    let slot = !control
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_replication_slots WHERE slot_name = ?1",
            vec![name.clone().into()],
        ))
        .await?
        .is_empty();
    if !slot {
        control
            .run_autocommit(&[format!(
                "SELECT pg_catalog.pg_create_logical_replication_slot({}, 'pgoutput')",
                quote_literal(&name)
            )])
            .await?;
    }
    let create = format!(
        "CREATE SUBSCRIPTION {name} CONNECTION {} PUBLICATION {CONTROL_PUBLICATION} \
         WITH (create_slot = false, slot_name = {}, copy_data = true, origin = none, disable_on_error = false)",
        quote_literal(conninfo.expose()),
        quote_literal(&name)
    );
    shard.run_autocommit(&[create]).await?;
    Ok(true)
}

/// Drops shard `number`'s subscription and its slot on control (draining
/// or removing a shard). Missing ones are fine.
pub async fn unsubscribe_from_control(shard: &Database, control: &Database, number: u16) -> Result<(), DbError> {
    let name = control_subscription_name(number);
    let exists = !shard
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_subscription WHERE subname = ?1",
            vec![name.clone().into()],
        ))
        .await?
        .is_empty();
    if exists {
        // Detach the slot so DROP does not need to reach control.
        shard
            .run_autocommit(&[
                format!("ALTER SUBSCRIPTION {name} DISABLE"),
                format!("ALTER SUBSCRIPTION {name} SET (slot_name = NONE)"),
                format!("DROP SUBSCRIPTION {name}"),
            ])
            .await?;
    }
    control
        .run_autocommit(&[format!(
            "SELECT pg_catalog.pg_drop_replication_slot(slot_name) FROM pg_catalog.pg_replication_slots \
             WHERE slot_name = {}",
            quote_literal(&name)
        )])
        .await
}

/// State of a shard's `fb_control` subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControlSubscription {
    /// Subscription name.
    pub name: String,
    /// Whether it is enabled.
    pub enabled: bool,
    /// Tables of the publication on this shard, and how many are ready
    /// (initial copy done, streaming).
    pub tables: u32,
    /// Tables done copying.
    pub ready: u32,
}

/// The `fb_control` subscription of shard `number`, if any.
pub async fn control_subscription(shard: &Database, number: u16) -> Result<Option<ControlSubscription>, DbError> {
    let name = control_subscription_name(number);
    let rows = shard
        .fetch_all(&Statement::new(
            "SELECT CASE WHEN s.subenabled THEN 1 ELSE 0 END, \
                    (SELECT count(*) FROM pg_catalog.pg_subscription_rel r WHERE r.srsubid = s.oid), \
                    (SELECT count(*) FROM pg_catalog.pg_subscription_rel r WHERE r.srsubid = s.oid AND r.srsubstate = 'r') \
             FROM pg_catalog.pg_subscription s WHERE s.subname = ?1",
            vec![name.clone().into()],
        ))
        .await?;
    Ok(rows.rows.first().map(|r| {
        let n = |i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0);
        ControlSubscription {
            name,
            enabled: n(0) == 1,
            tables: u32::try_from(n(1)).unwrap_or(0),
            ready: u32::try_from(n(2)).unwrap_or(0),
        }
    }))
}
