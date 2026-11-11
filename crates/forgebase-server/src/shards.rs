//! Database sets in the server (FB-171, ADR-0029 §3.5).
//!
//! * [`open_router`]: the [`DbRouter`] over the migrated control database.
//!   SQLite has one fixed set ([`DbRouter::single`]); PostgreSQL reads the
//!   topology (`<data_dir>/topology.json` is rewritten after changes) and
//!   opens every other routable set, and every replica, through
//!   [`NodeConnector`].
//! * [`NodeConnector`]: opens a node from its sealed URL (`url_sealed`:
//!   lowercase hex of [`Keyring::seal`] with the additional data
//!   [`node_url_aad`]); [`seal_node_url`] writes that form (FB-175 stores
//!   it). Local nodes (no URL) are opened by FB-174's shard provisioning.
//! * [`RELATION_DELETES_JOB`]: the fan-out of `ON DELETE` actions of shared
//!   records to every set ([`forgebase_data::RelationFanout`]), as a
//!   reserved job.

use std::fmt;
use std::sync::Arc;

use forgebase_auth::crypto::Keyring;
use forgebase_core::config::DatabaseConfig;
use forgebase_data::{DataService, RelationDeletes, RelationFanout};
use forgebase_db::topology::{DbNode, DbSet};
use forgebase_db::{BoxFuture, Database, DbError, DbOptions, DbRouter, DbTarget, RouterOptions, SetConnector};
use forgebase_jobs::{HandlerError, JobContext, JobRegistry, JobService, NewJob};
use serde_json::{Value as Json, json};

use crate::ServerError;

/// Job kind of the cross-set `ON DELETE` fan-out (reserved).
pub const RELATION_DELETES_JOB: &str = "data.relation_deletes";

/// Additional data that binds a sealed node URL to its node.
pub fn node_url_aad(node_id: &str) -> Vec<u8> {
    format!("forgebase/db-node/{node_id}").into_bytes()
}

/// Seals a node's connection URL for `_forgebase_db_nodes.url_sealed`.
pub fn seal_node_url(keyring: &Keyring, node_id: &str, url: &str) -> Result<String, String> {
    let sealed = keyring
        .seal(url.as_bytes(), &node_url_aad(node_id))
        .map_err(|e| e.to_string())?;
    Ok(sealed.iter().map(|b| format!("{b:02x}")).collect())
}

/// Opens a URL sealed by [`seal_node_url`].
pub fn open_node_url(keyring: &Keyring, node_id: &str, sealed: &str) -> Result<String, String> {
    let text = sealed.trim();
    let bytes = (0..text.len())
        .step_by(2)
        .map(|i| text.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .filter(|_| text.len().is_multiple_of(2))
        .ok_or_else(|| format!("sealed URL of node {node_id} is not hex"))?;
    let plain = keyring
        .open(&bytes, &node_url_aad(node_id))
        .map_err(|e| e.to_string())?;
    String::from_utf8(plain).map_err(|_| format!("sealed URL of node {node_id} is not text"))
}

/// Opens shard primaries and replicas from their sealed URLs, with the
/// server's PostgreSQL pool settings.
#[derive(Clone)]
pub struct NodeConnector {
    keyring: Keyring,
    config: DatabaseConfig,
}

impl fmt::Debug for NodeConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeConnector").finish_non_exhaustive()
    }
}

impl NodeConnector {
    /// A connector unsealing with `keyring`.
    pub fn new(keyring: Keyring, config: DatabaseConfig) -> Self {
        Self { keyring, config }
    }

    async fn open(&self, node: &DbNode, read_only: bool) -> Result<Database, DbError> {
        let Some(sealed) = node.url_sealed.as_deref() else {
            return Err(DbError::Unsupported(format!(
                "node {} has no connection URL (local nodes are opened by their shard provisioning)",
                node.name
            )));
        };
        let url = open_node_url(&self.keyring, &node.id, sealed).map_err(DbError::Unsupported)?;
        let mut opts = DbOptions::from_config_target(&self.config, DbTarget::postgres(url, false));
        opts.read_pool_size = self.config.postgres.pool_size;
        if read_only {
            opts = opts.read_only();
        }
        Database::open(opts).await
    }
}

impl SetConnector for NodeConnector {
    fn connect<'a>(&'a self, _set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(self.open(primary, false))
    }

    fn connect_replica<'a>(&'a self, _set: &'a DbSet, node: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(self.open(node, true))
    }
}

/// The router over the migrated control database `db` (see the module
/// documentation).
pub async fn open_router(
    db: &Database,
    config: &forgebase_core::Config,
    data_dir: &std::path::Path,
    keyring: Keyring,
) -> Result<DbRouter, ServerError> {
    if db.kind() != forgebase_db::BackendKind::Postgres {
        return Ok(DbRouter::single(db.clone()));
    }
    let options = RouterOptions {
        connector: Some(Arc::new(NodeConnector::new(keyring, config.database.clone()))),
        topology_file: Some(data_dir.join("topology.json")),
        ..RouterOptions::default()
    };
    DbRouter::open(db.clone(), options)
        .await
        .map_err(|e| ServerError::Backend(format!("cannot open the database topology: {e}")))
}

/// Startup on a topology with shards: drops the foreign keys of relations
/// from tenant data to shared collections on control (the data core
/// enforces them, ADR-0029 §3.6) and warns about rules that follow such
/// relations (they are refused at query time).
pub async fn prepare_data(data: &DataService) {
    if !data.is_sharded() {
        return;
    }
    match data.drop_cross_placement_keys().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(keys = n, "dropped foreign keys of relations across database sets"),
        Err(e) => tracing::warn!(error = %e, "foreign keys of relations across database sets not dropped"),
    }
    match data.cross_shard_offenders().await {
        Ok(list) if list.is_empty() => {}
        Ok(list) => {
            for o in &list {
                tracing::warn!(
                    collection = %o.collection,
                    rule = %o.rule,
                    path = %o.path,
                    "rule follows a relation across database sets; it is refused (relation_crosses_shards)"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "rules not checked for relations across database sets"),
    }
}

/// Schedules [`RelationDeletes`] as [`RELATION_DELETES_JOB`] jobs.
#[derive(Debug, Clone)]
pub struct JobFanout {
    jobs: JobService,
}

impl JobFanout {
    /// A fan-out on `jobs`.
    pub fn new(jobs: JobService) -> Self {
        Self { jobs }
    }
}

impl RelationFanout for JobFanout {
    fn schedule(&self, deletes: RelationDeletes) -> BoxFuture<'static, Result<(), String>> {
        let jobs = self.jobs.clone();
        Box::pin(async move {
            let payload = serde_json::to_value(&deletes).map_err(|e| e.to_string())?;
            let mut job = NewJob::new(RELATION_DELETES_JOB, payload);
            job.max_attempts = Some(10);
            jobs.enqueue(&forgebase_data::AccessContext::system(), job)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

/// Registers the [`RELATION_DELETES_JOB`] handler (reserved: only the
/// system and privileged callers may enqueue it).
pub fn register_jobs(registry: JobRegistry, data: DataService) -> JobRegistry {
    registry.register(RELATION_DELETES_JOB, move |ctx: JobContext| {
        let data = data.clone();
        async move {
            let deletes: RelationDeletes = serde_json::from_value(ctx.payload().clone())
                .map_err(|e| HandlerError::fatal(format!("bad payload: {e}")))?;
            let done = data
                .apply_relation_deletes(&deletes)
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
            Ok::<Json, HandlerError>(json!({ "sets": done }))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_urls_round_trip_and_bind_to_their_node() {
        let k = Keyring::from_master(&[7u8; 32]);
        let sealed = seal_node_url(&k, "n1", "postgres://u:p@h/db").unwrap();
        assert!(!sealed.contains("postgres"));
        assert_eq!(open_node_url(&k, "n1", &sealed).unwrap(), "postgres://u:p@h/db");
        assert!(open_node_url(&k, "n2", &sealed).is_err());
        assert!(open_node_url(&k, "n1", "zz").is_err());
    }
}
