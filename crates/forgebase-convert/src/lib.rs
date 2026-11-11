//! `forgebase db convert` (FB-151, ADR-0028 §6.4): a table-level copy of a
//! ForgeBase database from SQLite to PostgreSQL or back.
//!
//! The caller (the CLI) owns the outside: the data-directory lock, the
//! backup, opening and migrating both databases, the backend marker, the
//! configuration and keeping the old file. [`convert`] does the inside:
//! 1. preconditions: both databases at the binary's newest schema version,
//!    the target empty, the source consistent (SQLite: integrity and
//!    foreign keys; collection names valid on PostgreSQL) or free of
//!    PostgreSQL-only objects and extension data (`--to sqlite`, see
//!    [`refusals`]);
//! 2. the target's collection tables and managed indexes, made by the data
//!    core (`create_collection`, `create_index`), so they are exactly what
//!    that engine would have made;
//! 3. every table, catalog-driven (the PostgreSQL catalog is the list; a
//!    table on one side only fails the run), copied in one transaction on
//!    the target with triggers off: `COPY … FROM STDIN` into PostgreSQL,
//!    `COPY … TO STDOUT` out of it; values converted by column type
//!    ([`canon::Kind`]); SQLite `rowid` ↔ `_rowid`;
//! 4. derived data rebuilt (`_forgebase_vector_fts`), identity sequences
//!    set past their maximum (and past SQLite's `sqlite_sequence`);
//! 5. verification in the same transaction: per table, row count and a
//!    SHA-256 over the rows in primary-key order in a canonical encoding
//!    ([`canon`]); any mismatch rolls the copy back;
//! 6. a report: per-table counts and hashes, and the raw-SQL indexes and
//!    other objects the administrator must recreate by hand.

pub mod canon;
mod copytext;
pub mod extensions;
mod pg;
mod plan;
mod refusals;
mod schema;
mod sqlite;

use std::path::PathBuf;

use forgebase_db::{BackendKind, Database, DbError, SecretString, Value};
use futures_util::TryStreamExt as _;
use serde::Serialize;
use sqlx::{AssertSqlSafe, PgConnection, SqliteConnection};

pub use extensions::{Prepared, prepare_portable};

use crate::canon::TableHash;
use crate::plan::{TablePlan, VECTOR_FTS};

/// Why a conversion did not run or was rolled back.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// A precondition does not hold (schema version, empty target, …).
    #[error("{0}")]
    Precondition(String),
    /// Objects or data that cannot be converted, one line each.
    #[error("cannot convert:\n  - {}", .0.join("\n  - "))]
    Refused(Vec<String>),
    /// The target's collection tables or indexes could not be made.
    #[error("{0}")]
    Schema(String),
    /// Bringing extension data to portable rows failed.
    #[error("{0}")]
    Prepare(String),
    /// A table could not be copied.
    #[error("copying {table}: {message}")]
    Copy {
        /// `schema.table`.
        table: String,
        /// What went wrong.
        message: String,
    },
    /// The verification found differences; the copy was rolled back.
    #[error("verification failed, nothing was written:\n  - {}", .0.join("\n  - "))]
    Mismatch(Vec<String>),
    /// Database error through the backend layer.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Driver error on a copy connection.
    #[error("database: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Data core error.
    #[error(transparent)]
    Data(#[from] forgebase_data::DataError),
}

/// How the copy reaches a database, next to its [`Database`] handle.
#[derive(Debug, Clone)]
pub enum Conn {
    /// The SQLite file.
    Sqlite(PathBuf),
    /// A PostgreSQL URL. A superuser (`forgebase_super` on the bundled
    /// engine) lets the copy turn triggers off with
    /// `session_replication_role`; another role must own the tables
    /// (`ALTER TABLE … DISABLE TRIGGER USER`).
    Postgres(SecretString),
}

/// One side of a conversion: the migrated database and its copy connection.
#[derive(Debug, Clone, Copy)]
pub struct Endpoint<'a> {
    /// The database (migrated to the binary's newest version).
    pub db: &'a Database,
    /// How the copy connects to it.
    pub conn: &'a Conn,
}

/// One copied table in the report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TableReport {
    /// `forgebase` (system) or `public` (collection).
    pub schema: String,
    /// Table name.
    pub table: String,
    /// Rows copied (equal on both sides).
    pub rows: u64,
    /// SHA-256 of the rows in the canonical encoding (equal on both sides).
    pub sha256: String,
}

/// An object the conversion did not recreate.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManualObject {
    /// `index`, `trigger` or `view`.
    pub kind: String,
    /// Object name.
    pub name: String,
    /// Table it belongs to.
    pub table: String,
    /// Its SQL on the source engine.
    pub sql: String,
}

/// What a conversion did.
#[derive(Debug, Clone, Serialize)]
pub struct ConvertReport {
    /// `sqlite` or `postgres`.
    pub from: &'static str,
    /// `sqlite` or `postgres`.
    pub to: &'static str,
    /// Schema version of both databases.
    pub schema_version: i64,
    /// Collections created on the target.
    pub collections: usize,
    /// Managed indexes created on the target (automatic indexes come with
    /// their collection).
    pub indexes: usize,
    /// Copied tables, system tables first.
    pub tables: Vec<TableReport>,
    /// Rows rebuilt in `_forgebase_vector_fts` (derived from the vector items).
    pub derived_rows: u64,
    /// Identity sequences set on the target.
    pub sequences: usize,
    /// Raw-SQL indexes, triggers and views to recreate by hand (their SQL
    /// may not parse on the other engine).
    pub manual: Vec<ManualObject>,
    /// Other notes.
    pub notes: Vec<String>,
}

impl ConvertReport {
    /// Rows copied in all tables.
    pub fn total_rows(&self) -> u64 {
        self.tables.iter().map(|t| t.rows).sum()
    }
}

/// Progress callback: one line per step.
pub type Progress<'a> = &'a (dyn Fn(&str) + Send + Sync);

fn kind_name(k: BackendKind) -> &'static str {
    match k {
        BackendKind::Sqlite => "sqlite",
        BackendKind::Postgres => "postgres",
    }
}

/// Converts `source` into the empty `target` (see the crate docs).
///
/// # Errors
/// [`ConvertError`]; on any error after the copy started the target's copy
/// transaction is rolled back (the collection tables made in step 2 stay;
/// the caller discards the target).
pub async fn convert(
    source: Endpoint<'_>,
    target: Endpoint<'_>,
    progress: Progress<'_>,
) -> Result<ConvertReport, ConvertError> {
    let (from, to) = (source.db.kind(), target.db.kind());
    if from == to {
        return Err(ConvertError::Precondition(format!(
            "source and target are both {}",
            kind_name(from)
        )));
    }
    match (source.conn, target.conn, from) {
        (Conn::Sqlite(_), Conn::Postgres(_), BackendKind::Sqlite)
        | (Conn::Postgres(_), Conn::Sqlite(_), BackendKind::Postgres) => {}
        _ => {
            return Err(ConvertError::Precondition(
                "the copy connections do not match the databases".into(),
            ));
        }
    }

    // 1. Schema versions.
    let src_status = source.db.migration_status().await?;
    if !src_status.pending.is_empty() || !src_status.problems.is_empty() {
        return Err(ConvertError::Precondition(format!(
            "the source is at schema v{} but this build expects v{}: run `forgebase migrate` first",
            src_status.current_version, src_status.latest_version
        )));
    }
    let tgt_status = target.db.migration_status().await?;
    if tgt_status.current_version != src_status.current_version || !tgt_status.pending.is_empty() {
        return Err(ConvertError::Precondition(format!(
            "the target is at schema v{}, the source at v{}",
            tgt_status.current_version, src_status.current_version
        )));
    }
    let collections_on_target = schema::load(target.db).await?.collections.len();
    if collections_on_target > 0 {
        return Err(ConvertError::Precondition(format!(
            "the target database is not empty ({collections_on_target} collection(s)); convert into an empty database"
        )));
    }

    let cat = schema::load(source.db).await?;
    let mut notes = Vec::new();
    match from {
        BackendKind::Sqlite => {
            let mut problems = source.db.integrity_check().await?;
            let fk = source.db.foreign_key_violations().await?;
            if fk > 0 {
                problems.push(format!(
                    "{fk} row(s) violate foreign keys (PRAGMA foreign_key_check lists them)"
                ));
            }
            for c in &cat.collections {
                if let Err(e) = c.validate_postgres_names(None) {
                    problems.push(format!("collection {}: {e}", c.name));
                }
            }
            if !problems.is_empty() {
                return Err(ConvertError::Refused(problems));
            }
        }
        BackendKind::Postgres => {
            let Conn::Postgres(url) = source.conn else {
                return unreachable_conn();
            };
            let mut conn = pg::connect(url.expose()).await?;
            let views: Vec<String> = cat
                .collections
                .iter()
                .filter(|c| c.is_view())
                .map(|c| c.name.clone())
                .collect();
            let found = refusals::check(&mut conn, &views).await;
            let _ = sqlx::Connection::close(conn).await;
            let found = found?;
            if !found.problems.is_empty() {
                return Err(ConvertError::Refused(found.problems));
            }
            notes.extend(found.notes);
        }
    }

    // 2. Collection tables and managed indexes on the target.
    progress(&format!(
        "creating {} collection(s) and {} index(es) on the target",
        cat.collections.len(),
        cat.indexes.len()
    ));
    let built = schema::build(target.db, &cat).await?;
    if built.deferred_fields > 0 {
        notes.push(format!(
            "{} relation field(s) of a relation cycle were added after their target collection",
            built.deferred_fields
        ));
    }

    // 3-5. Copy, rebuild, sequences, verification.
    let mut report = ConvertReport {
        from: kind_name(from),
        to: kind_name(to),
        schema_version: src_status.current_version,
        collections: built.collections,
        indexes: built.indexes,
        tables: Vec::new(),
        derived_rows: 0,
        sequences: 0,
        manual: Vec::new(),
        notes,
    };
    match (source.conn, target.conn) {
        (Conn::Sqlite(path), Conn::Postgres(url)) => {
            let mut src = sqlite::connect(path, true).await?;
            let mut dst = pg::connect(url.expose()).await?;
            let r = to_postgres(&mut src, &mut dst, &mut report, progress).await;
            let _ = sqlx::Connection::close(src).await;
            let _ = sqlx::Connection::close(dst).await;
            r?;
        }
        (Conn::Postgres(url), Conn::Sqlite(path)) => {
            let mut src = pg::connect(url.expose()).await?;
            let mut dst = sqlite::connect(path, false).await?;
            let r = to_sqlite(&mut src, &mut dst, &mut report, progress).await;
            let _ = sqlx::Connection::close(src).await;
            let _ = sqlx::Connection::close(dst).await;
            r?;
        }
        _ => return unreachable_conn(),
    }
    Ok(report)
}

fn unreachable_conn<T>() -> Result<T, ConvertError> {
    Err(ConvertError::Precondition(
        "the copy connections do not match the databases".into(),
    ))
}

fn copy_err(plan: &TablePlan, message: impl Into<String>) -> ConvertError {
    ConvertError::Copy {
        table: plan.label(),
        message: message.into(),
    }
}

fn compare(plan: &TablePlan, source: &(u64, String), target: &(u64, String), out: &mut Vec<String>) {
    if source != target {
        out.push(format!(
            "{}: source {} row(s) sha256 {}, target {} row(s) sha256 {}",
            plan.label(),
            source.0,
            source.1,
            target.0,
            target.1
        ));
    }
}

fn table_report(plan: &TablePlan, hash: &(u64, String)) -> TableReport {
    TableReport {
        schema: plan.pg.schema.clone(),
        table: plan.pg.name.clone(),
        rows: hash.0,
        sha256: hash.1.clone(),
    }
}

/// Columns PostgreSQL has and SQLite does not must be empty (`--to
/// sqlite`), or are left NULL (`--to postgres`).
async fn check_pg_only(conn: &mut PgConnection, plans: &[TablePlan]) -> Result<(), ConvertError> {
    let mut problems = Vec::new();
    for p in plans {
        for c in &p.pg_only {
            let n = pg::scalar_i64(
                conn,
                &format!(
                    "SELECT count(*)::bigint FROM {} WHERE {} IS NOT NULL",
                    p.pg.qualified(),
                    pg::quote(c)
                ),
            )
            .await?
            .unwrap_or(0);
            if n > 0 {
                problems.push(format!(
                    "column {}.{c} exists only on PostgreSQL and holds {n} value(s)",
                    p.label()
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ConvertError::Refused(problems))
    }
}

// ------------------------------------------------------------ to PostgreSQL

async fn to_postgres(
    src: &mut SqliteConnection,
    dst: &mut PgConnection,
    report: &mut ConvertReport,
    progress: Progress<'_>,
) -> Result<(), ConvertError> {
    let pg_tables = pg::tables(dst).await?;
    let sqlite_tables = sqlite::tables(src).await?;
    let (plans, _) = plan::pair(&pg_tables, &sqlite_tables, forgebase_migrations::postgres::ROWID_TABLES)
        .map_err(ConvertError::Refused)?;
    report.manual = sqlite_manual_objects(src, dst, &plans).await?;
    let seqs = sqlite::sequences(src).await?;
    let has_fts = pg_tables
        .iter()
        .any(|t| t.name == VECTOR_FTS && t.schema == "forgebase");
    let superuser = pg::is_superuser(dst).await?;

    pg::exec(dst, "BEGIN").await?;
    let r = async {
        if superuser {
            pg::exec(dst, "SET LOCAL session_replication_role = replica").await?;
        } else {
            for p in &plans {
                pg::exec(dst, &format!("ALTER TABLE {} DISABLE TRIGGER USER", p.pg.qualified())).await?;
            }
            pg::exec(dst, "SET CONSTRAINTS ALL DEFERRED").await?;
        }
        let mut all: Vec<String> = plans.iter().map(|p| p.pg.qualified()).collect();
        if has_fts {
            all.push(format!("forgebase.{}", pg::quote(VECTOR_FTS)));
        }
        pg::exec(dst, &format!("TRUNCATE {}", all.join(", "))).await?;

        let mut source_hashes = Vec::with_capacity(plans.len());
        for p in &plans {
            let h = copy_table_to_pg(src, dst, p).await?;
            progress(&format!("copied {} ({} row(s))", p.label(), h.0));
            source_hashes.push(h);
        }
        if has_fts {
            pg::exec(
                dst,
                "INSERT INTO forgebase._forgebase_vector_fts (rowid, content) \
                 SELECT key, content FROM forgebase._forgebase_vector_items WHERE content IS NOT NULL",
            )
            .await?;
            report.derived_rows = pg::scalar_i64(dst, "SELECT count(*)::bigint FROM forgebase._forgebase_vector_fts")
                .await?
                .unwrap_or(0)
                .try_into()
                .unwrap_or(0);
        }
        for p in &plans {
            let floor = seqs.iter().find(|(t, _)| t == &p.sqlite).map(|(_, s)| *s);
            report.sequences += pg::reset_identities(dst, &p.pg, |col| {
                // `sqlite_sequence` tracks the AUTOINCREMENT key, never `_rowid`.
                (col != "_rowid").then_some(floor).flatten()
            })
            .await?;
        }

        progress("verifying");
        let mut mismatches = Vec::new();
        for (p, source) in plans.iter().zip(&source_hashes) {
            let mut h = TableHash::new(p.kinds(), !p.pk.is_empty());
            let kinds = p.kinds();
            pg::copy_out(dst, &p.pg_select(), async |rows| {
                for r in rows {
                    let values = decode_row(p, &kinds, r)?;
                    h.push(&values).map_err(|e| copy_err(p, e))?;
                }
                Ok(())
            })
            .await?;
            let target = h.finish();
            compare(p, source, &target, &mut mismatches);
            report.tables.push(table_report(p, source));
        }
        if !superuser {
            for p in &plans {
                pg::exec(dst, &format!("ALTER TABLE {} ENABLE TRIGGER USER", p.pg.qualified())).await?;
            }
            // Deferred foreign keys are checked here, before the commit.
            pg::exec(dst, "SET CONSTRAINTS ALL IMMEDIATE").await?;
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(ConvertError::Mismatch(mismatches))
        }
    }
    .await;
    match r {
        Ok(()) => pg::exec(dst, "COMMIT").await,
        Err(e) => {
            let _ = pg::exec(dst, "ROLLBACK").await;
            Err(e)
        }
    }
}

async fn copy_table_to_pg(
    src: &mut SqliteConnection,
    dst: &mut PgConnection,
    p: &TablePlan,
) -> Result<(u64, String), ConvertError> {
    let kinds = p.kinds();
    let cols: Vec<String> = p.columns.iter().map(|c| pg::quote(&c.pg)).collect();
    let sql = format!("COPY {} ({}) FROM STDIN", p.pg.qualified(), cols.join(", "));
    let mut hash = TableHash::new(kinds.clone(), !p.pk.is_empty());
    let mut copy = dst.copy_in_raw(&sql).await?;
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut rows = sqlx::query(AssertSqlSafe(p.sqlite_select()))
        .persistent(false)
        .fetch(&mut *src);
    let result: Result<(), ConvertError> = async {
        while let Some(row) = rows.try_next().await? {
            let values = sqlite::row_values(&row)?;
            hash.push(&values).map_err(|e| copy_err(p, e))?;
            for (i, (v, k)) in values.iter().zip(&kinds).enumerate() {
                if i > 0 {
                    buf.push(b'\t');
                }
                copytext::encode_field(&mut buf, *k, v)
                    .map_err(|e| copy_err(p, format!("column {}: {e}", p.columns[i].pg)))?;
            }
            buf.push(b'\n');
            if buf.len() >= 1 << 20 {
                copy.send(std::mem::take(&mut buf)).await?;
            }
        }
        if !buf.is_empty() {
            copy.send(std::mem::take(&mut buf)).await?;
        }
        Ok(())
    }
    .await;
    drop(rows);
    match result {
        Ok(()) => {
            let n = copy.finish().await.map_err(|e| copy_err(p, e.to_string()))?;
            let h = hash.finish();
            if n != h.0 {
                return Err(copy_err(p, format!("read {} row(s), PostgreSQL took {n}", h.0)));
            }
            Ok(h)
        }
        Err(e) => {
            let _ = copy.abort("conversion failed").await;
            Err(e)
        }
    }
}

fn decode_row(p: &TablePlan, kinds: &[canon::Kind], row: pg::CopyRow) -> Result<Vec<Value>, ConvertError> {
    if row.len() != kinds.len() {
        return Err(copy_err(p, "the server sent a row with a different number of fields"));
    }
    row.into_iter()
        .zip(kinds)
        .enumerate()
        .map(|(i, (f, k))| {
            copytext::decode_field(*k, f).map_err(|e| copy_err(p, format!("column {}: {e}", p.columns[i].pg)))
        })
        .collect()
}

/// SQLite indexes, triggers and views on collection tables that the
/// target did not get (raw SQL from the SQL editor).
async fn sqlite_manual_objects(
    src: &mut SqliteConnection,
    dst: &mut PgConnection,
    plans: &[TablePlan],
) -> Result<Vec<ManualObject>, ConvertError> {
    let collection_tables: Vec<&str> = plans
        .iter()
        .filter(|p| p.pg.schema == "public")
        .map(|p| p.sqlite.as_str())
        .collect();
    let made: Vec<String> = pg::public_indexes(dst).await?.into_iter().map(|(n, _, _)| n).collect();
    let views: Vec<String> = pg::text_rows(
        dst,
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind = 'v'",
    )
    .await?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .collect();
    let rows = sqlite::rows(
        src,
        "SELECT type, name, tbl_name, sql FROM sqlite_master \
         WHERE type IN ('index', 'trigger', 'view') AND sql IS NOT NULL ORDER BY tbl_name, name",
    )
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let (kind, name, table, sql) = (
            r[0].as_str().unwrap_or_default(),
            r[1].as_str().unwrap_or_default(),
            r[2].as_str().unwrap_or_default(),
            r[3].as_str().unwrap_or_default(),
        );
        let missing = match kind {
            "index" => collection_tables.contains(&table) && !made.iter().any(|m| m == name),
            // Full-text triggers belong to FTS5 tables the GIN index replaces.
            "trigger" => collection_tables.contains(&table) && !name.starts_with("_forgebase_fts_"),
            "view" => !views.iter().any(|v| v == name),
            _ => false,
        };
        if missing {
            out.push(ManualObject {
                kind: kind.to_owned(),
                name: name.to_owned(),
                table: table.to_owned(),
                sql: sql.to_owned(),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- to SQLite

async fn to_sqlite(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
    report: &mut ConvertReport,
    progress: Progress<'_>,
) -> Result<(), ConvertError> {
    let pg_tables = pg::tables(src).await?;
    let sqlite_tables = sqlite::tables(dst).await?;
    let (plans, _) = plan::pair(&pg_tables, &sqlite_tables, forgebase_migrations::postgres::ROWID_TABLES)
        .map_err(ConvertError::Refused)?;
    check_pg_only(src, &plans).await?;
    report.manual = pg_manual_objects(src, dst).await?;
    let has_fts = sqlite_tables.iter().any(|t| t.name == VECTOR_FTS);
    // AUTOINCREMENT tables on the target and the source's sequence positions.
    let mut positions = Vec::new();
    for p in &plans {
        let ddl = sqlite::rows(
            dst,
            &format!(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = {}",
                sqlite::literal(&p.sqlite)
            ),
        )
        .await?;
        let autoinc = ddl
            .first()
            .and_then(|r| r[0].as_str())
            .is_some_and(|s| s.to_ascii_uppercase().contains("AUTOINCREMENT"));
        if !autoinc {
            continue;
        }
        for c in p.pg.columns.iter().filter(|c| c.identity && c.name != "_rowid") {
            if let Some(pos) = pg::sequence_position(src, &p.pg, &c.name).await? {
                positions.push((p.sqlite.clone(), pos));
            }
        }
    }

    pg::exec(src, "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
    sqlite::exec(dst, "BEGIN IMMEDIATE").await?;
    let r = async {
        // System triggers (append-only guards, the FTS sync) stay off for
        // the copy; collection tables keep theirs (full-text sync).
        let triggers = sqlite::rows(
            dst,
            "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND sql IS NOT NULL \
             AND tbl_name LIKE '\\_forgebase%' ESCAPE '\\'",
        )
        .await?;
        for t in &triggers {
            sqlite::exec(
                dst,
                &format!("DROP TRIGGER {}", sqlite::quote(t[0].as_str().unwrap_or_default())),
            )
            .await?;
        }
        for p in &plans {
            sqlite::exec(dst, &format!("DELETE FROM {}", sqlite::quote(&p.sqlite))).await?;
        }
        if has_fts {
            sqlite::exec(dst, "DELETE FROM _forgebase_vector_fts").await?;
        }
        let mut source_hashes = Vec::with_capacity(plans.len());
        for p in &plans {
            let h = copy_table_to_sqlite(src, dst, p).await?;
            progress(&format!("copied {} ({} row(s))", p.label(), h.0));
            source_hashes.push(h);
        }
        if has_fts {
            sqlite::exec(
                dst,
                "INSERT INTO _forgebase_vector_fts (rowid, content) \
                 SELECT key, content FROM _forgebase_vector_items WHERE content IS NOT NULL",
            )
            .await?;
            report.derived_rows = sqlite::rows(
                dst,
                "SELECT count(*) FROM _forgebase_vector_items WHERE content IS NOT NULL",
            )
            .await?
            .first()
            .and_then(|r| r[0].as_i64())
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        }
        for (table, pos) in &positions {
            let have = sqlite::rows(
                dst,
                &format!(
                    "SELECT seq FROM sqlite_sequence WHERE name = {}",
                    sqlite::literal(table)
                ),
            )
            .await?;
            match have.first().and_then(|r| r[0].as_i64()) {
                Some(seq) if seq >= *pos => {}
                Some(_) => {
                    sqlite::exec(
                        dst,
                        &format!(
                            "UPDATE sqlite_sequence SET seq = {pos} WHERE name = {}",
                            sqlite::literal(table)
                        ),
                    )
                    .await?;
                }
                None => {
                    sqlite::exec(
                        dst,
                        &format!(
                            "INSERT INTO sqlite_sequence (name, seq) VALUES ({}, {pos})",
                            sqlite::literal(table)
                        ),
                    )
                    .await?;
                }
            }
            report.sequences += 1;
        }
        for t in &triggers {
            sqlite::exec(dst, t[1].as_str().unwrap_or_default()).await?;
        }
        let fk = sqlite::rows(dst, "PRAGMA foreign_key_check").await?;
        if !fk.is_empty() {
            return Err(ConvertError::Mismatch(vec![format!(
                "{} row(s) violate foreign keys after the copy",
                fk.len()
            )]));
        }

        progress("verifying");
        let mut mismatches = Vec::new();
        for (p, source) in plans.iter().zip(&source_hashes) {
            let mut h = TableHash::new(p.kinds(), !p.pk.is_empty());
            let mut rows = sqlx::query(AssertSqlSafe(p.sqlite_select()))
                .persistent(false)
                .fetch(&mut *dst);
            while let Some(row) = rows.try_next().await? {
                h.push(&sqlite::row_values(&row)?).map_err(|e| copy_err(p, e))?;
            }
            drop(rows);
            let target = h.finish();
            compare(p, source, &target, &mut mismatches);
            report.tables.push(table_report(p, source));
        }
        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(ConvertError::Mismatch(mismatches))
        }
    }
    .await;
    let _ = pg::exec(src, "ROLLBACK").await;
    match r {
        Ok(()) => sqlite::exec(dst, "COMMIT").await,
        Err(e) => {
            let _ = sqlite::exec(dst, "ROLLBACK").await;
            Err(e)
        }
    }
}

async fn copy_table_to_sqlite(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
    p: &TablePlan,
) -> Result<(u64, String), ConvertError> {
    let kinds = p.kinds();
    let cols: Vec<String> = p
        .columns
        .iter()
        .map(|c| {
            if c.sqlite == "rowid" {
                "rowid".to_owned()
            } else {
                sqlite::quote(&c.sqlite)
            }
        })
        .collect();
    let marks = vec!["?"; cols.len()].join(", ");
    let insert = format!(
        "INSERT INTO {} ({}) VALUES ({marks})",
        sqlite::quote(&p.sqlite),
        cols.join(", ")
    );
    let mut hash = TableHash::new(kinds.clone(), !p.pk.is_empty());
    pg::copy_out(src, &p.pg_select(), async |rows| {
        for r in rows {
            let values = decode_row(p, &kinds, r)?;
            hash.push(&values).map_err(|e| copy_err(p, e))?;
            let mut q = sqlx::query(AssertSqlSafe(insert.clone()));
            for v in values {
                q = match v {
                    Value::Null => q.bind(None::<i64>),
                    Value::Integer(i) => q.bind(i),
                    Value::Real(f) => q.bind(f),
                    Value::Text(s) => q.bind(s),
                    Value::Blob(b) => q.bind(b),
                };
            }
            q.execute(&mut *dst).await.map_err(|e| copy_err(p, e.to_string()))?;
        }
        Ok(())
    })
    .await?;
    Ok(hash.finish())
}

/// PostgreSQL indexes on collection tables that the SQLite target did not
/// get (raw SQL from the SQL editor), and the dropped native policies'
/// helper objects are not listed (they are regenerated).
async fn pg_manual_objects(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
) -> Result<Vec<ManualObject>, ConvertError> {
    let made: Vec<String> = sqlite::rows(dst, "SELECT name FROM sqlite_master WHERE name IS NOT NULL")
        .await?
        .into_iter()
        .filter_map(|r| r[0].as_str().map(str::to_owned))
        .collect();
    Ok(pg::public_indexes(src)
        .await?
        .into_iter()
        .filter(|(name, _, _)| !made.iter().any(|m| m == name))
        .map(|(name, table, sql)| ManualObject {
            kind: "index".into(),
            name,
            table,
            sql,
        })
        .collect())
}
