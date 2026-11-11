//! Checks a node passes before it serves reads (ADR-0029 §2.4).

use forgebase_core::{ErrorCode, ErrorKind};
use serde::Serialize;

use crate::backend::{Backend, Statement, Value};
use crate::{BackendKind, Database, DbError};

/// Oldest replica server accepted (ADR-0028 decision 1).
const MIN_VERSION_NUM: i64 = 160_000;

const FACTS_SQL: &str = "SELECT \
    CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END AS in_recovery, \
    (SELECT system_identifier::text FROM pg_control_system()) AS system_identifier, \
    current_setting('server_version_num')::int8 AS version_num, \
    current_setting('server_version') AS version, \
    (SELECT timeline_id FROM pg_control_checkpoint())::int8 AS timeline, \
    CASE WHEN to_regnamespace('forgebase') IS NULL THEN 0 \
         WHEN NOT has_schema_privilege('forgebase', 'USAGE') THEN 0 \
         WHEN to_regclass('forgebase._forgebase_migrations') IS NULL THEN 0 \
         WHEN has_table_privilege('forgebase._forgebase_migrations', 'SELECT') THEN 1 \
         ELSE 0 END AS schema_readable";

/// What a PostgreSQL server says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplicaFacts {
    /// `pg_is_in_recovery()`.
    pub in_recovery: bool,
    /// `pg_control_system().system_identifier`: equal on a primary and all
    /// its physical replicas.
    pub system_identifier: String,
    /// `server_version_num` (`180003`).
    pub version_num: u32,
    /// `server_version` (`18.3`).
    pub version: String,
    /// Timeline of the latest checkpoint.
    pub timeline: Option<i64>,
    /// The connecting role can read the ForgeBase schema.
    pub schema_readable: bool,
}

/// Why a node cannot serve as a replica of a set.
#[derive(Debug, thiserror::Error)]
pub enum ReplicaCheckError {
    /// The node is not a usable replica of this primary (`400 node_mismatch`).
    #[error("node {node} is not a usable replica: {reason}")]
    Mismatch {
        /// Node name.
        node: String,
        /// What did not match.
        reason: String,
    },
    /// The node or the primary could not be asked.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl ErrorCode for ReplicaCheckError {
    fn code(&self) -> &'static str {
        match self {
            ReplicaCheckError::Mismatch { .. } => "node_mismatch",
            ReplicaCheckError::Db(e) => e.code(),
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            ReplicaCheckError::Mismatch { .. } => ErrorKind::InvalidInput,
            ReplicaCheckError::Db(e) => e.kind(),
        }
    }
}

fn text(row: &[Value], i: usize) -> String {
    match row.get(i) {
        Some(Value::Text(s)) => s.clone(),
        Some(Value::Integer(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn int(row: &[Value], i: usize) -> Option<i64> {
    row.get(i).and_then(Value::as_i64)
}

/// What the server behind `db` says about itself (PostgreSQL only).
pub async fn primary_facts(db: &Database) -> Result<ReplicaFacts, DbError> {
    if db.kind() != BackendKind::Postgres {
        return Err(DbError::unsupported("read replicas", db.kind()));
    }
    let rows = db.fetch_all(&Statement::new(FACTS_SQL, Vec::new())).await?;
    let row = rows
        .rows
        .first()
        .ok_or_else(|| DbError::Statement("the server facts query returned no row".into()))?;
    Ok(ReplicaFacts {
        in_recovery: int(row, 0) == Some(1),
        system_identifier: text(row, 1),
        version_num: int(row, 2).and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        version: text(row, 3),
        timeline: int(row, 4),
        schema_readable: int(row, 5) == Some(1),
    })
}

/// Checks that `replica` (node `name`) is a usable read replica of
/// `primary`: in recovery, the same `system_identifier`, PostgreSQL 16 or
/// newer, and the role can read the ForgeBase schema. Returns the replica's
/// facts (the admin API stores version, identifier and timeline).
pub async fn check_replica(
    primary: &Database,
    replica: &Database,
    name: &str,
) -> Result<ReplicaFacts, ReplicaCheckError> {
    let p = primary_facts(primary).await?;
    let r = primary_facts(replica).await?;
    let mismatch = |reason: String| ReplicaCheckError::Mismatch {
        node: name.to_owned(),
        reason,
    };
    if !r.in_recovery {
        return Err(mismatch(
            "the server is not in recovery (pg_is_in_recovery() is false)".into(),
        ));
    }
    if r.system_identifier != p.system_identifier {
        return Err(mismatch(format!(
            "system_identifier {} differs from the primary's {} (a replica of another cluster)",
            r.system_identifier, p.system_identifier
        )));
    }
    if i64::from(r.version_num) < MIN_VERSION_NUM {
        return Err(mismatch(format!("PostgreSQL {} is older than 16", r.version)));
    }
    if !r.schema_readable {
        return Err(mismatch("the role cannot read the forgebase schema".into()));
    }
    Ok(r)
}
