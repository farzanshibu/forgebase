//! Embedded system migrations for the ForgeBase primary database.
//!
//! * Migrations are SQL files under `migrations/system/`, compiled into the
//!   binary with `include_str!` and listed in [`SYSTEM_MIGRATIONS`].
//! * Each applied migration is recorded in `_forgebase_migrations` with its
//!   SHA-256 checksum. A shipped migration whose text changes is detected and
//!   the database is refused ([`MigrationError::ChecksumMismatch`]).
//! * A database migrated by a newer binary is refused
//!   ([`MigrationError::DatabaseNewer`]) instead of being silently downgraded.
//! * Every migration runs in its own `BEGIN IMMEDIATE` transaction together with
//!   its bookkeeping row and a `PRAGMA foreign_key_check`: it is applied
//!   completely or not at all. `PRAGMA user_version` mirrors the schema version
//!   for external tools.
//! * Running the migrator twice is a no-op the second time.
//!
//! PostgreSQL has its own runner and SQL files ([`postgres`]): a baseline for
//! versions 1–25, then one twin per later SQLite migration.

pub mod multi;
pub mod postgres;

use std::time::Instant;

use forgebase_core::{ErrorCode, ErrorKind};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Connection, Row, SqliteConnection};

/// Name of the bookkeeping table.
pub const BOOKKEEPING_TABLE: &str = "_forgebase_migrations";

const CREATE_BOOKKEEPING: &str = "CREATE TABLE IF NOT EXISTS _forgebase_migrations (
    version      INTEGER NOT NULL PRIMARY KEY,
    name         TEXT    NOT NULL,
    checksum     TEXT    NOT NULL,
    applied_at   TEXT    NOT NULL,
    execution_ms INTEGER NOT NULL
) STRICT";

/// One schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Strictly increasing version, starting at 1.
    pub version: i64,
    /// snake_case name (the file name without the version prefix).
    pub name: &'static str,
    /// SQL text; may contain several statements.
    pub sql: &'static str,
}

impl Migration {
    /// Declares a migration.
    pub const fn new(version: i64, name: &'static str, sql: &'static str) -> Self {
        Self { version, name, sql }
    }

    /// Lowercase hex SHA-256 of the SQL text.
    pub fn checksum(&self) -> String {
        Sha256::digest(self.sql.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// ForgeBase's own schema, in order. Append only; never edit a shipped entry.
pub static SYSTEM_MIGRATIONS: &[Migration] = &[
    Migration::new(
        1,
        "initial",
        include_str!("../../../migrations/system/0001_initial.sql"),
    ),
    Migration::new(
        2,
        "data_core",
        include_str!("../../../migrations/system/0002_data_core.sql"),
    ),
    Migration::new(3, "auth", include_str!("../../../migrations/system/0003_auth.sql")),
    Migration::new(4, "policy", include_str!("../../../migrations/system/0004_policy.sql")),
    Migration::new(5, "kv", include_str!("../../../migrations/system/0005_kv.sql")),
    Migration::new(6, "graph", include_str!("../../../migrations/system/0006_graph.sql")),
    Migration::new(7, "jobs", include_str!("../../../migrations/system/0007_jobs.sql")),
    Migration::new(
        8,
        "oauth_webauthn",
        include_str!("../../../migrations/system/0008_oauth_webauthn.sql"),
    ),
    Migration::new(
        9,
        "entitlements",
        include_str!("../../../migrations/system/0009_entitlements.sql"),
    ),
    Migration::new(
        11,
        "realtime",
        include_str!("../../../migrations/system/0011_realtime.sql"),
    ),
    Migration::new(12, "vector", include_str!("../../../migrations/system/0012_vector.sql")),
    Migration::new(
        13,
        "impersonation",
        include_str!("../../../migrations/system/0013_impersonation.sql"),
    ),
    Migration::new(
        14,
        "identity",
        include_str!("../../../migrations/system/0014_identity.sql"),
    ),
    Migration::new(
        15,
        "credits",
        include_str!("../../../migrations/system/0015_credits.sql"),
    ),
    Migration::new(
        16,
        "functions",
        include_str!("../../../migrations/system/0016_functions.sql"),
    ),
    Migration::new(
        17,
        "storage",
        include_str!("../../../migrations/system/0017_storage.sql"),
    ),
    Migration::new(
        18,
        "oidc_provider",
        include_str!("../../../migrations/system/0018_oidc_provider.sql"),
    ),
    Migration::new(
        19,
        "customize",
        include_str!("../../../migrations/system/0019_customize.sql"),
    ),
    Migration::new(
        20,
        "billing",
        include_str!("../../../migrations/system/0020_billing.sql"),
    ),
    Migration::new(21, "queue", include_str!("../../../migrations/system/0021_queue.sql")),
    Migration::new(
        22,
        "jobs_long_tasks",
        include_str!("../../../migrations/system/0022_jobs_long_tasks.sql"),
    ),
    Migration::new(
        23,
        "identity_admin",
        include_str!("../../../migrations/system/0023_identity_admin.sql"),
    ),
    Migration::new(
        24,
        "billing_stores",
        include_str!("../../../migrations/system/0024_billing_stores.sql"),
    ),
    Migration::new(
        25,
        "queue_ready_order",
        include_str!("../../../migrations/system/0025_queue_ready_order.sql"),
    ),
    Migration::new(
        26,
        "resource_rules",
        include_str!("../../../migrations/system/0026_resource_rules.sql"),
    ),
    Migration::new(
        27,
        "functions_js",
        include_str!("../../../migrations/system/0027_functions_js.sql"),
    ),
    Migration::new(
        28,
        "key_tenant",
        include_str!("../../../migrations/system/0028_key_tenant.sql"),
    ),
    Migration::new(
        29,
        "storage_rls",
        include_str!("../../../migrations/system/0029_storage_rls.sql"),
    ),
    Migration::new(
        30,
        "job_tenant",
        include_str!("../../../migrations/system/0030_job_tenant.sql"),
    ),
    Migration::new(
        31,
        "queue_rls",
        include_str!("../../../migrations/system/0031_queue_rls.sql"),
    ),
    Migration::new(
        33,
        "db_topology",
        include_str!("../../../migrations/system/0033_db_topology.sql"),
    ),
    Migration::new(
        35,
        "job_queue_tenant",
        include_str!("../../../migrations/system/0035_job_queue_tenant.sql"),
    ),
    Migration::new(
        38,
        "schedule_fire",
        include_str!("../../../migrations/system/0038_schedule_fire.sql"),
    ),
    Migration::new(
        40,
        "native_rls",
        include_str!("../../../migrations/system/0040_native_rls.sql"),
    ),
    Migration::new(
        41,
        "index_builds",
        include_str!("../../../migrations/system/0041_index_builds.sql"),
    ),
    Migration::new(
        42,
        "db_logins",
        include_str!("../../../migrations/system/0042_db_logins.sql"),
    ),
    Migration::new(
        43,
        "db_node_agents",
        include_str!("../../../migrations/system/0043_db_node_agents.sql"),
    ),
    Migration::new(
        50,
        "queue_push_attempts",
        include_str!("../../../migrations/system/0050_queue_push_attempts.sql"),
    ),
    Migration::new(
        51,
        "queue_push_health",
        include_str!("../../../migrations/system/0051_queue_push_health.sql"),
    ),
    Migration::new(
        52,
        "queue_redrive_catalog",
        include_str!("../../../migrations/system/0052_queue_redrive_catalog.sql"),
    ),
    Migration::new(
        53,
        "billing_webhook_deliveries",
        include_str!("../../../migrations/system/0053_billing_webhook_deliveries.sql"),
    ),
    Migration::new(
        56,
        "portable_trigram",
        include_str!("../../../migrations/system/0056_portable_trigram.sql"),
    ),
    Migration::new(
        58,
        "alerts_slos",
        include_str!("../../../migrations/system/0058_alerts_slos.sql"),
    ),
    Migration::new(
        59,
        "trace_context",
        include_str!("../../../migrations/system/0059_trace_context.sql"),
    ),
    Migration::new(
        60,
        "insights",
        include_str!("../../../migrations/system/0060_insights.sql"),
    ),
    Migration::new(
        61,
        "audit_search",
        include_str!("../../../migrations/system/0061_audit_search.sql"),
    ),
    Migration::new(
        65,
        "oauth_identity_avatar",
        include_str!("../../../migrations/system/0065_oauth_identity_avatar.sql"),
    ),
    Migration::new(
        66,
        "email_providers",
        include_str!("../../../migrations/system/0066_email_providers.sql"),
    ),
];

/// A migration recorded as applied in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Version.
    pub version: i64,
    /// Name.
    pub name: String,
    /// Checksum recorded when it was applied.
    pub checksum: String,
    /// RFC 3339 time it was applied.
    pub applied_at: String,
    /// How long it took.
    pub execution_ms: i64,
}

/// A known migration that has not been applied yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMigration {
    /// Version.
    pub version: i64,
    /// Name.
    pub name: &'static str,
}

/// Something about the applied migrations that blocks further migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationProblem {
    /// An applied migration's text differs from the embedded one.
    ChecksumMismatch {
        /// Version.
        version: i64,
        /// Name.
        name: String,
        /// Checksum of the embedded SQL.
        expected: String,
        /// Checksum recorded in the database.
        found: String,
    },
    /// The database has a migration this binary does not know (it was
    /// migrated by a newer ForgeBase).
    UnknownVersion {
        /// Version.
        version: i64,
        /// Name recorded in the database.
        name: String,
    },
}

/// Applied, pending and problematic migrations of a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    /// Highest applied version (0 when none).
    pub current_version: i64,
    /// Highest version this binary knows.
    pub latest_version: i64,
    /// Applied migrations, in version order.
    pub applied: Vec<AppliedMigration>,
    /// Known migrations not applied yet, in version order.
    pub pending: Vec<PendingMigration>,
    /// Blocking problems.
    pub problems: Vec<MigrationProblem>,
}

impl MigrationStatus {
    /// No pending migrations and no problems.
    pub fn is_up_to_date(&self) -> bool {
        self.pending.is_empty() && self.problems.is_empty()
    }
}

/// Applies an ordered set of migrations.
#[derive(Debug, Clone, Copy)]
pub struct Migrator<'a> {
    migrations: &'a [Migration],
}

impl Migrator<'static> {
    /// Migrator for [`SYSTEM_MIGRATIONS`].
    pub fn system() -> Self {
        Self {
            migrations: SYSTEM_MIGRATIONS,
        }
    }
}

impl<'a> Migrator<'a> {
    /// Migrator for a custom set. Versions must be positive and strictly increasing.
    pub fn new(migrations: &'a [Migration]) -> Result<Self, MigrationError> {
        let mut last = 0;
        for m in migrations {
            if m.version <= last {
                return Err(MigrationError::InvalidSet(format!(
                    "version {} ({}) is not greater than {last}",
                    m.version, m.name
                )));
            }
            if m.name.is_empty() {
                return Err(MigrationError::InvalidSet(format!("version {} has no name", m.version)));
            }
            last = m.version;
        }
        Ok(Self { migrations })
    }

    /// The migrations of this set.
    pub fn migrations(&self) -> &'a [Migration] {
        self.migrations
    }

    /// Highest known version (0 for an empty set).
    pub fn latest_version(&self) -> i64 {
        self.migrations.last().map_or(0, |m| m.version)
    }

    /// Reads the migration state without changing the database.
    pub async fn status(&self, conn: &mut SqliteConnection) -> Result<MigrationStatus, MigrationError> {
        let has_table: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1")
                .bind(BOOKKEEPING_TABLE)
                .fetch_one(&mut *conn)
                .await?;
        let applied = if has_table > 0 {
            sqlx::query(
                "SELECT version, name, checksum, applied_at, execution_ms
                 FROM _forgebase_migrations ORDER BY version",
            )
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .map(|row| {
                Ok(AppliedMigration {
                    version: row.try_get(0)?,
                    name: row.try_get(1)?,
                    checksum: row.try_get(2)?,
                    applied_at: row.try_get(3)?,
                    execution_ms: row.try_get(4)?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?
        } else {
            Vec::new()
        };

        let mut problems = Vec::new();
        for a in &applied {
            match self.migrations.iter().find(|m| m.version == a.version) {
                Some(m) => {
                    let expected = m.checksum();
                    if expected != a.checksum {
                        problems.push(MigrationProblem::ChecksumMismatch {
                            version: a.version,
                            name: a.name.clone(),
                            expected,
                            found: a.checksum.clone(),
                        });
                    }
                }
                None => problems.push(MigrationProblem::UnknownVersion {
                    version: a.version,
                    name: a.name.clone(),
                }),
            }
        }
        let pending = self
            .migrations
            .iter()
            .filter(|m| !applied.iter().any(|a| a.version == m.version))
            .map(|m| PendingMigration {
                version: m.version,
                name: m.name,
            })
            .collect();
        Ok(MigrationStatus {
            current_version: applied.last().map_or(0, |a| a.version),
            latest_version: self.latest_version(),
            applied,
            pending,
            problems,
        })
    }

    /// Applies every pending migration, each in its own transaction.
    /// Returns the migrations applied by this call (empty when up to date).
    pub async fn run(&self, conn: &mut SqliteConnection) -> Result<Vec<AppliedMigration>, MigrationError> {
        sqlx::raw_sql(CREATE_BOOKKEEPING).execute(&mut *conn).await?;
        let status = self.status(conn).await?;
        if let Some(problem) = status.problems.into_iter().next() {
            return Err(problem.into());
        }

        let mut done = Vec::new();
        for pending in &status.pending {
            let Some(m) = self.migrations.iter().find(|m| m.version == pending.version) else {
                continue;
            };
            if let Some(applied) = apply_one(conn, m).await? {
                done.push(applied);
            }
        }
        Ok(done)
    }
}

/// Applies one migration atomically. Returns `None` when another process
/// applied it between our status read and taking the write lock.
async fn apply_one(conn: &mut SqliteConnection, m: &Migration) -> Result<Option<AppliedMigration>, MigrationError> {
    let checksum = m.checksum();
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;

    let already: Option<String> = sqlx::query_scalar("SELECT checksum FROM _forgebase_migrations WHERE version = ?1")
        .bind(m.version)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(found) = already {
        if found != checksum {
            return Err(MigrationError::ChecksumMismatch {
                version: m.version,
                name: m.name.to_owned(),
                expected: checksum,
                found,
            });
        }
        return Ok(None);
    }

    let started = Instant::now();
    sqlx::raw_sql(m.sql)
        .execute(&mut *tx)
        .await
        .map_err(|source| MigrationError::Failed {
            version: m.version,
            name: m.name.to_owned(),
            source,
        })?;

    let violations = sqlx::query("PRAGMA foreign_key_check").fetch_all(&mut *tx).await?.len();
    if violations > 0 {
        return Err(MigrationError::ForeignKeyViolation {
            version: m.version,
            name: m.name.to_owned(),
            violations,
        });
    }

    let execution_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let applied_at = forgebase_core::now_rfc3339();
    sqlx::query(
        "INSERT INTO _forgebase_migrations (version, name, checksum, applied_at, execution_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(m.version)
    .bind(m.name)
    .bind(&checksum)
    .bind(&applied_at)
    .bind(execution_ms)
    .execute(&mut *tx)
    .await?;
    // PRAGMA does not accept bind parameters; the version is an integer we own.
    sqlx::query(AssertSqlSafe(format!("PRAGMA user_version = {}", m.version)))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(version = m.version, name = m.name, execution_ms, "applied migration");
    Ok(Some(AppliedMigration {
        version: m.version,
        name: m.name.to_owned(),
        checksum,
        applied_at,
        execution_ms,
    }))
}

/// Migration errors.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// The migration set itself is malformed (a bug in ForgeBase).
    #[error("invalid migration set: {0}")]
    InvalidSet(String),
    /// A shipped migration was edited after it was applied.
    #[error(
        "migration {version} ({name}) was modified after it was applied: \
         checksum {found} in the database, {expected} in this binary"
    )]
    ChecksumMismatch {
        /// Version.
        version: i64,
        /// Name.
        name: String,
        /// Checksum of the embedded SQL.
        expected: String,
        /// Checksum recorded in the database.
        found: String,
    },
    /// The database was migrated by a newer ForgeBase.
    #[error(
        "database has migration {version} ({name}) which this binary does not know; \
         it was created by a newer ForgeBase"
    )]
    DatabaseNewer {
        /// Version.
        version: i64,
        /// Name recorded in the database.
        name: String,
    },
    /// A migration's SQL failed; nothing from it was applied.
    #[error("migration {version} ({name}) failed and was rolled back: {source}")]
    Failed {
        /// Version.
        version: i64,
        /// Name.
        name: String,
        /// Database error.
        #[source]
        source: sqlx::Error,
    },
    /// A migration left foreign-key violations; it was rolled back.
    #[error("migration {version} ({name}) left {violations} foreign-key violation(s) and was rolled back")]
    ForeignKeyViolation {
        /// Version.
        version: i64,
        /// Name.
        name: String,
        /// Number of violating rows.
        violations: usize,
    },
    /// Any other database error.
    #[error("database error during migration: {0}")]
    Database(#[from] sqlx::Error),
}

impl From<MigrationProblem> for MigrationError {
    fn from(p: MigrationProblem) -> Self {
        match p {
            MigrationProblem::ChecksumMismatch {
                version,
                name,
                expected,
                found,
            } => MigrationError::ChecksumMismatch {
                version,
                name,
                expected,
                found,
            },
            MigrationProblem::UnknownVersion { version, name } => MigrationError::DatabaseNewer { version, name },
        }
    }
}

impl ErrorCode for MigrationError {
    fn code(&self) -> &'static str {
        match self {
            MigrationError::InvalidSet(_) => "migration_set_invalid",
            MigrationError::ChecksumMismatch { .. } => "migration_checksum_mismatch",
            MigrationError::DatabaseNewer { .. } => "database_newer_than_binary",
            MigrationError::Failed { .. } => "migration_failed",
            MigrationError::ForeignKeyViolation { .. } => "migration_foreign_key_violation",
            MigrationError::Database(_) => "database_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            MigrationError::ChecksumMismatch { .. } | MigrationError::DatabaseNewer { .. } => ErrorKind::Conflict,
            _ => ErrorKind::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory() -> SqliteConnection {
        SqliteConnection::connect("sqlite::memory:").await.unwrap()
    }

    async fn table_exists(conn: &mut SqliteConnection, name: &str) -> bool {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1")
            .bind(name.to_owned())
            .fetch_one(conn)
            .await
            .unwrap();
        n == 1
    }

    #[tokio::test]
    async fn system_migrations_apply_and_are_idempotent() {
        let mut conn = memory().await;
        let migrator = Migrator::system();
        let first = migrator.run(&mut conn).await.unwrap();
        assert_eq!(first.len(), SYSTEM_MIGRATIONS.len());
        assert!(table_exists(&mut conn, "_forgebase_meta").await);

        let status = migrator.status(&mut conn).await.unwrap();
        assert!(status.is_up_to_date());
        assert_eq!(status.current_version, migrator.latest_version());

        let second = migrator.run(&mut conn).await.unwrap();
        assert!(second.is_empty(), "second run must apply nothing");
        let again = migrator.status(&mut conn).await.unwrap();
        assert_eq!(again, status, "status unchanged by a second run");

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM _forgebase_migrations")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(rows, SYSTEM_MIGRATIONS.len() as i64);
        let user_version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(user_version, migrator.latest_version());
    }

    #[tokio::test]
    async fn status_does_not_create_tables() {
        let mut conn = memory().await;
        let status = Migrator::system().status(&mut conn).await.unwrap();
        assert_eq!(status.current_version, 0);
        assert_eq!(status.pending.len(), SYSTEM_MIGRATIONS.len());
        assert!(!table_exists(&mut conn, BOOKKEEPING_TABLE).await);
    }

    #[tokio::test]
    async fn failing_migration_is_rolled_back_completely() {
        static SET: &[Migration] = &[
            Migration::new(1, "one", "CREATE TABLE a (x INTEGER);"),
            Migration::new(
                2,
                "two",
                "CREATE TABLE b (x INTEGER); INSERT INTO missing_table VALUES (1);",
            ),
        ];
        let mut conn = memory().await;
        let err = Migrator::new(SET).unwrap().run(&mut conn).await.unwrap_err();
        assert!(matches!(err, MigrationError::Failed { version: 2, .. }), "{err}");
        assert!(table_exists(&mut conn, "a").await, "migration 1 committed");
        assert!(!table_exists(&mut conn, "b").await, "migration 2 rolled back");
        let status = Migrator::new(SET).unwrap().status(&mut conn).await.unwrap();
        assert_eq!(status.current_version, 1);
        assert_eq!(status.pending.len(), 1);
    }

    #[tokio::test]
    async fn foreign_key_violations_roll_back() {
        static SET: &[Migration] = &[Migration::new(
            1,
            "fk",
            "PRAGMA defer_foreign_keys = ON;
             CREATE TABLE parent (id INTEGER PRIMARY KEY);
             CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id));
             INSERT INTO child (id, parent_id) VALUES (1, 42);",
        )];
        let mut conn = memory().await;
        let err = Migrator::new(SET).unwrap().run(&mut conn).await.unwrap_err();
        assert!(
            matches!(err, MigrationError::ForeignKeyViolation { violations: 1, .. }),
            "{err}"
        );
        assert!(!table_exists(&mut conn, "child").await);
    }

    #[tokio::test]
    async fn edited_migration_is_refused() {
        static ORIGINAL: &[Migration] = &[Migration::new(1, "one", "CREATE TABLE a (x INTEGER);")];
        static EDITED: &[Migration] = &[Migration::new(1, "one", "CREATE TABLE a (x TEXT);")];
        let mut conn = memory().await;
        Migrator::new(ORIGINAL).unwrap().run(&mut conn).await.unwrap();
        let err = Migrator::new(EDITED).unwrap().run(&mut conn).await.unwrap_err();
        assert_eq!(err.code(), "migration_checksum_mismatch");
        let status = Migrator::new(EDITED).unwrap().status(&mut conn).await.unwrap();
        assert!(!status.is_up_to_date());
    }

    #[tokio::test]
    async fn newer_database_is_refused() {
        static NEW: &[Migration] = &[
            Migration::new(1, "one", "CREATE TABLE a (x INTEGER);"),
            Migration::new(2, "two", "CREATE TABLE b (x INTEGER);"),
        ];
        let mut conn = memory().await;
        Migrator::new(NEW).unwrap().run(&mut conn).await.unwrap();
        let err = Migrator::new(&NEW[..1]).unwrap().run(&mut conn).await.unwrap_err();
        assert!(matches!(err, MigrationError::DatabaseNewer { version: 2, .. }), "{err}");
    }

    #[test]
    fn invalid_sets_are_rejected() {
        static BAD: &[Migration] = &[Migration::new(2, "b", ""), Migration::new(1, "a", "")];
        assert!(Migrator::new(BAD).is_err());
        static ZERO: &[Migration] = &[Migration::new(0, "z", "")];
        assert!(Migrator::new(ZERO).is_err());
        assert!(Migrator::new(SYSTEM_MIGRATIONS).is_ok());
    }

    #[test]
    fn every_system_migration_file_is_registered() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/system");
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        files.sort();
        let registered: Vec<String> = SYSTEM_MIGRATIONS
            .iter()
            .map(|m| format!("{:04}_{}.sql", m.version, m.name))
            .collect();
        assert_eq!(files, registered);
    }

    #[test]
    fn checksum_is_stable_sha256() {
        let m = Migration::new(1, "x", "");
        assert_eq!(
            m.checksum(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
