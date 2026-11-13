//! SQL of the functions tables (migration 0016).

use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use serde_json::Value as Json;

use crate::error::{FunctionError, Result};
use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{ExecutionLog, InvokeAccess, KvAccess, KvRule, LogLine, SecretInfo, VersionConfig, VersionInfo};

/// A function row with its active version's configuration.
#[derive(Debug, Clone)]
pub(crate) struct FunctionRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) access: InvokeAccess,
    pub(crate) active_version: i64,
    pub(crate) latest_version: i64,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) created_by: String,
    pub(crate) config: VersionConfig,
}

fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or_default()
}

fn config_of(raw: &str) -> Result<VersionConfig> {
    serde_json::from_str(raw)
        .map_err(|e| FunctionError::Internal(format!("stored function config does not decode: {e}")))
}

const FUNCTION_COLUMNS: &str = "f.id, f.name, f.description, f.access, f.active_version, f.latest_version,
     f.created_at, f.updated_at, f.created_by, v.config";

fn function_rows(rows: &Rows) -> Result<Vec<FunctionRow>> {
    rows.rows
        .iter()
        .map(|r| {
            Ok(FunctionRow {
                id: text(r, 0),
                name: text(r, 1),
                description: text(r, 2),
                access: InvokeAccess::parse(&text(r, 3))
                    .ok_or_else(|| FunctionError::Internal("unknown stored access mode".into()))?,
                active_version: int(r, 4),
                latest_version: int(r, 5),
                created_at: text(r, 6),
                updated_at: text(r, 7),
                created_by: text(r, 8),
                config: config_of(&text(r, 9))?,
            })
        })
        .collect()
}

/// Every function with its active configuration.
pub(crate) async fn load_all(db: &dyn Backend) -> Result<Vec<FunctionRow>> {
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT {FUNCTION_COLUMNS} FROM _forgebase_functions f
                 JOIN _forgebase_function_versions v ON v.function_id = f.id AND v.version = f.active_version
                 ORDER BY f.name"
            ),
            vec![],
        ))
        .await?;
    function_rows(&rows)
}

/// The function row by name, inside a write transaction.
pub(crate) async fn find_in_tx(tx: &mut dyn WriteTx, name: &str) -> Result<Option<(String, i64, i64)>> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT id, active_version, latest_version FROM _forgebase_functions WHERE name = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(rows.rows.first().map(|r| (text(r, 0), int(r, 1), int(r, 2))))
}

/// Runs `stmts` in one transaction.
pub(crate) async fn write(db: &dyn Backend, stmts: Vec<Statement>) -> Result<()> {
    let mut tx = db.begin(TxMode::Normal).await?;
    for s in &stmts {
        if let Err(e) = tx.execute(s).await {
            let _ = tx.rollback().await;
            return Err(e.into());
        }
    }
    tx.commit().await?;
    Ok(())
}

/// The module bytes of one version.
pub(crate) async fn wasm(db: &dyn Backend, function_id: &str, version: i64) -> Result<Option<Vec<u8>>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT wasm FROM _forgebase_function_versions WHERE function_id = ?1 AND version = ?2",
            vec![function_id.into(), version.into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| match r.first() {
        Some(Value::Blob(b)) => Some(b.clone()),
        _ => None,
    }))
}

/// A text column of one version (`source` or `js`).
async fn version_text(db: &dyn Backend, function_id: &str, version: i64, column: &str) -> Result<Option<String>> {
    let rows = db
        .fetch_all(&Statement::new(
            format!("SELECT {column} FROM _forgebase_function_versions WHERE function_id = ?1 AND version = ?2"),
            vec![function_id.into(), version.into()],
        ))
        .await?;
    Ok(rows
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_str)
        .map(str::to_owned))
}

/// The JavaScript the engine runs for a JS/TS version, with its source map (TypeScript).
pub(crate) async fn script(
    db: &dyn Backend,
    function_id: &str,
    version: i64,
) -> Result<Option<(String, Option<String>)>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT js, source_map FROM _forgebase_function_versions WHERE function_id = ?1 AND version = ?2",
            vec![function_id.into(), version.into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| {
        let js = r.first().and_then(Value::as_str)?.to_owned();
        Some((js, r.get(1).and_then(Value::as_str).map(str::to_owned)))
    }))
}

/// The source of a JS/TS version as deployed.
pub(crate) async fn source(db: &dyn Backend, function_id: &str, version: i64) -> Result<Option<String>> {
    version_text(db, function_id, version, "source").await
}

/// The configuration of one version.
pub(crate) async fn version_config(db: &dyn Backend, function_id: &str, version: i64) -> Result<Option<VersionConfig>> {
    version_text(db, function_id, version, "config")
        .await?
        .map(|c| config_of(&c))
        .transpose()
}

/// Key-value namespace rules of every function, by function id.
pub(crate) async fn kv_rules(db: &dyn Backend) -> Result<HashMap<String, Arc<Vec<KvRule>>>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT function_id, namespace, access FROM _forgebase_function_kv_rules ORDER BY function_id, namespace",
            vec![],
        ))
        .await?;
    let mut out: HashMap<String, Vec<KvRule>> = HashMap::new();
    for r in &rows.rows {
        out.entry(text(r, 0)).or_default().push(KvRule {
            namespace: text(r, 1),
            access: if text(r, 2) == "write" {
                KvAccess::Write
            } else {
                KvAccess::Read
            },
        });
    }
    Ok(out.into_iter().map(|(k, v)| (k, Arc::new(v))).collect())
}

/// Versions of a function, newest first.
pub(crate) async fn versions(db: &dyn Backend, function_id: &str, active: i64) -> Result<Vec<VersionInfo>> {
    // Size in bytes of the source text or the module.
    let size = if db.dialect().name() == "postgres" {
        "coalesce(octet_length(source), octet_length(wasm))"
    } else {
        "coalesce(length(CAST(source AS BLOB)), length(wasm))"
    };
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT version, sha256, {size}, config, created_at, created_by \
                 FROM _forgebase_function_versions WHERE function_id = ?1 ORDER BY version DESC"
            ),
            vec![function_id.into()],
        ))
        .await?;
    rows.rows
        .iter()
        .map(|r| {
            let version = int(r, 0);
            Ok(VersionInfo {
                version,
                sha256: text(r, 1),
                size: int(r, 2),
                config: config_of(&text(r, 3))?,
                active: version == active,
                created_at: text(r, 4),
                created_by: text(r, 5),
            })
        })
        .collect()
}

/// Whether version `version` exists.
pub(crate) async fn version_exists(db: &dyn Backend, function_id: &str, version: i64) -> Result<bool> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT 1 FROM _forgebase_function_versions WHERE function_id = ?1 AND version = ?2",
            vec![function_id.into(), version.into()],
        ))
        .await?;
    Ok(!rows.rows.is_empty())
}

/// The newest version below `below`.
pub(crate) async fn previous_version(db: &dyn Backend, function_id: &str, below: i64) -> Result<Option<i64>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT max(version) FROM _forgebase_function_versions WHERE function_id = ?1 AND version < ?2",
            vec![function_id.into(), below.into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| r.first()).and_then(Value::as_i64))
}

/// Sealed secrets of a function: `(name, sealed)`.
pub(crate) async fn sealed_secrets(db: &dyn Backend, function_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT name, sealed FROM _forgebase_function_secrets WHERE function_id = ?1 ORDER BY name",
            vec![function_id.into()],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| match r.get(1) {
            Some(Value::Blob(b)) => Some((text(r, 0), b.clone())),
            _ => None,
        })
        .collect())
}

/// Secret metadata.
pub(crate) async fn secret_infos(db: &dyn Backend, function_id: &str) -> Result<Vec<SecretInfo>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT name, created_at, updated_at FROM _forgebase_function_secrets WHERE function_id = ?1 ORDER BY name",
            vec![function_id.into()],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| SecretInfo {
            name: text(r, 0),
            created_at: text(r, 1),
            updated_at: text(r, 2),
        })
        .collect())
}

/// One execution to record.
#[derive(Debug)]
pub(crate) struct NewLog<'a> {
    pub(crate) function_id: &'a str,
    pub(crate) function: &'a str,
    pub(crate) version: i64,
    pub(crate) trigger: &'a str,
    pub(crate) status: &'a str,
    pub(crate) duration_ms: i64,
    pub(crate) fuel_used: i64,
    pub(crate) error: Option<String>,
    pub(crate) logs: &'a [LogLine],
    pub(crate) request_id: Option<String>,
    pub(crate) created_at: String,
    /// The execution's profile as JSON (ADR-0036 §3).
    pub(crate) profile: Option<String>,
}

/// Inserts an execution log row; with `keep`, prunes the function's rows
/// beyond the newest `keep`.
pub(crate) async fn insert_log(db: &dyn Backend, log: NewLog<'_>, keep: Option<i64>) -> Result<()> {
    let lines = serde_json::to_string(log.logs).unwrap_or_else(|_| "[]".into());
    let mut stmts = vec![Statement::new(
        "INSERT INTO _forgebase_function_logs
             (function_id, function, version, trigger, status, duration_ms, fuel_used, error, logs, request_id, created_at, profile)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        vec![
            log.function_id.into(),
            log.function.into(),
            log.version.into(),
            log.trigger.into(),
            log.status.into(),
            log.duration_ms.into(),
            log.fuel_used.into(),
            log.error.into(),
            lines.into(),
            log.request_id.into(),
            log.created_at.into(),
            log.profile.into(),
        ],
    )];
    if let Some(keep) = keep {
        stmts.push(Statement::new(
            "DELETE FROM _forgebase_function_logs WHERE function_id = ?1 AND id <= (
                 SELECT id FROM _forgebase_function_logs WHERE function_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
            vec![log.function_id.into(), keep.into()],
        ));
    }
    write(db, stmts).await
}

/// Newest execution logs of a function.
pub(crate) async fn logs(db: &dyn Backend, function_id: &str, limit: i64) -> Result<Vec<ExecutionLog>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT id, function, version, trigger, status, duration_ms, fuel_used, error, logs, request_id, created_at
             FROM _forgebase_function_logs WHERE function_id = ?1 ORDER BY id DESC LIMIT ?2",
            vec![function_id.into(), limit.into()],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| ExecutionLog {
            id: int(r, 0),
            function: text(r, 1),
            version: int(r, 2),
            trigger: text(r, 3),
            status: text(r, 4),
            duration_ms: int(r, 5),
            fuel_used: int(r, 6),
            error: r.get(7).and_then(Value::as_str).map(str::to_owned),
            logs: serde_json::from_str::<Json>(&text(r, 8))
                .ok()
                .and_then(|j| serde_json::from_value(j).ok())
                .unwrap_or_default(),
            request_id: r.get(9).and_then(Value::as_str).map(str::to_owned),
            created_at: text(r, 10),
        })
        .collect())
}

/// A consumer cursor.
pub(crate) async fn cursor(db: &dyn Backend, name: &str) -> Result<Option<i64>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT seq FROM _forgebase_function_cursors WHERE name = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(rows.rows.first().map(|r| int(r, 0)))
}

/// Stores a consumer cursor.
pub(crate) async fn set_cursor(db: &dyn Backend, name: &str, seq: i64, now: &str) -> Result<()> {
    write(
        db,
        vec![Statement::new(
            "INSERT INTO _forgebase_function_cursors (name, seq, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET seq = excluded.seq, updated_at = excluded.updated_at",
            vec![name.into(), seq.into(), now.into()],
        )],
    )
    .await
}

/// Filters of [`invocations`].
#[derive(Debug, Clone, Default)]
pub(crate) struct InvocationFilter {
    pub(crate) function: Option<String>,
    pub(crate) version: Option<i64>,
    pub(crate) status: Option<String>,
    pub(crate) trigger: Option<String>,
    pub(crate) request_id: Option<String>,
    /// Only rows with an id below this (paging backwards).
    pub(crate) before_id: Option<i64>,
    pub(crate) only_errors: bool,
    pub(crate) min_duration_ms: Option<i64>,
}

/// Newest executions with their profile, over all functions or one.
pub(crate) async fn invocations(
    db: &dyn Backend,
    f: &InvocationFilter,
    limit: i64,
) -> Result<Vec<crate::model::InvocationDetail>> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT id, function, version, trigger, status, duration_ms, fuel_used, error, logs, request_id, created_at, profile
             FROM _forgebase_function_logs
             WHERE (CAST(?1 AS TEXT) IS NULL OR function = ?1)
               AND (CAST(?2 AS BIGINT) IS NULL OR version = ?2)
               AND (CAST(?3 AS TEXT) IS NULL OR status = ?3)
               AND (CAST(?4 AS TEXT) IS NULL OR trigger = ?4)
               AND (CAST(?5 AS TEXT) IS NULL OR request_id = ?5)
               AND (CAST(?6 AS BIGINT) IS NULL OR id < ?6)
               AND (?7 = 0 OR status <> 'ok')
               AND (CAST(?8 AS BIGINT) IS NULL OR duration_ms >= ?8)
             ORDER BY id DESC LIMIT ?9",
            vec![
                f.function.clone().into(),
                f.version.into(),
                f.status.clone().into(),
                f.trigger.clone().into(),
                f.request_id.clone().into(),
                f.before_id.into(),
                i64::from(f.only_errors).into(),
                f.min_duration_ms.into(),
                limit.into(),
            ],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| crate::model::InvocationDetail {
            log: ExecutionLog {
                id: int(r, 0),
                function: text(r, 1),
                version: int(r, 2),
                trigger: text(r, 3),
                status: text(r, 4),
                duration_ms: int(r, 5),
                fuel_used: int(r, 6),
                error: r.get(7).and_then(Value::as_str).map(str::to_owned),
                logs: serde_json::from_str::<Json>(&text(r, 8))
                    .ok()
                    .and_then(|j| serde_json::from_value(j).ok())
                    .unwrap_or_default(),
                request_id: r.get(9).and_then(Value::as_str).map(str::to_owned),
                created_at: text(r, 10),
            },
            profile: r
                .get(11)
                .and_then(Value::as_str)
                .and_then(|p| serde_json::from_str::<Json>(p).ok()),
        })
        .collect())
}
