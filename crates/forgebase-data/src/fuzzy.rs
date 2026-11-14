//! Trigram indexes and `fuzzy` search on every backend (ADR-0029 §1.8,
//! ADR-0034).
//!
//! Three engines answer the same API (index kind `trigram`, list parameters
//! `fuzzy`, `fuzzy_fields`, `fuzzy_threshold`) with `pg_trgm`'s semantics
//! (see [`forgebase_query::trigram`]):
//!
//! | Engine | Where | Index | Candidates | Score |
//! |---|---|---|---|---|
//! | [`TrigramEngine::PgTrgm`] | PostgreSQL with `pg_trgm` in use | GIN `gin_trgm_ops` | `col % q` | `similarity()` |
//! | [`TrigramEngine::PgPortable`] | PostgreSQL without it (`trigram = "off"`, or no extension) | GIN over `forgebase._forgebase_trgm(col)` (`text[]`, built-in `array_ops`) | `trgm(col) && trgm(q)` | `forgebase._forgebase_trgm_similarity` |
//! | [`TrigramEngine::SqlitePortable`] | SQLite | table `_forgebase_trgm_<index> (rid, k, tri)` kept by triggers | rows sharing ⌈t·\|q\|⌉ trigrams | shared / union from the table |
//!
//! The PostgreSQL portable engine computes trigrams with the server's own
//! `lower()` and `[[:alnum:]]` (migration 0056), so it matches `pg_trgm` on
//! the same server; the SQLite engine classifies characters with the fold
//! table of migration 0056 and reads the first
//! [`MAX_CHARS`](forgebase_query::trigram::MAX_CHARS) characters. The query
//! side of the SQLite engine turns the search text into trigrams in Rust
//! with the same classification ([`forgebase_query::trigram::trigrams`]).
//!
//! An index is built by the engine in use when it is created (and on each
//! shard by the same rule, FB-173); searching works on whatever index exists,
//! so switching `trigram` between `pg_trgm` and portable keeps answers
//! correct, and re-creating the index (drop + create) makes it fast again.

use forgebase_db::{Dialect, EXTENSION_SCHEMA, Feature, Value};
use forgebase_query::trigram::{self, DEFAULT_THRESHOLD, MAX_CHARS};
use forgebase_query::{Compiler, FieldPath, SqlBuilder};
use forgebase_schema::Collection;

use crate::catalog::CatalogSnapshot;
use crate::ddl::{is_pg, rel};
use crate::indexes::{IndexDefinition, key_exprs};
use crate::{DataError, DataService};

/// Prefix of the SQLite trigram tables (and their triggers).
pub(crate) const TRGM_PREFIX: &str = "_forgebase_trgm_";
/// Trigram set of a text on PostgreSQL (migration 0056).
const PG_TRGM_FN: &str = "forgebase._forgebase_trgm";
/// Similarity of two trigram sets on PostgreSQL (migration 0056).
const PG_SIM_FN: &str = "forgebase._forgebase_trgm_similarity";
/// Reference tables of the SQLite engine (migration 0056).
const POSITIONS: &str = "_forgebase_trgm_positions";
const FOLD: &str = "_forgebase_trgm_fold";

/// The engine behind trigram indexes and `fuzzy` (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrigramEngine {
    /// PostgreSQL's `pg_trgm` extension (the accelerator).
    PgTrgm,
    /// The portable engine on PostgreSQL (SQL functions, GIN over `text[]`).
    PgPortable,
    /// The portable engine on SQLite (a trigram table per index).
    SqlitePortable,
}

impl TrigramEngine {
    /// The engine of a database with dialect `d`, where `pg_trgm` says
    /// whether the extension is in use.
    pub fn of(d: &dyn Dialect, pg_trgm: bool) -> Self {
        match (is_pg(d), pg_trgm) {
            (false, _) => TrigramEngine::SqlitePortable,
            (true, true) => TrigramEngine::PgTrgm,
            (true, false) => TrigramEngine::PgPortable,
        }
    }

    /// `pg_trgm` or `portable`.
    pub fn name(self) -> &'static str {
        match self {
            TrigramEngine::PgTrgm => "pg_trgm",
            TrigramEngine::PgPortable | TrigramEngine::SqlitePortable => "portable",
        }
    }
}

impl DataService {
    /// Whether `pg_trgm` serves trigram indexes and `fuzzy` here.
    pub(crate) fn pg_trgm(&self) -> bool {
        self.inner.backend.feature_active(Feature::Trigram)
    }

    /// The trigram engine of this service's database.
    pub fn trigram_engine(&self) -> TrigramEngine {
        TrigramEngine::of(self.inner.backend.dialect(), self.pg_trgm())
    }
}

/// The SQLite trigram table of index `name`.
pub(crate) fn side_table(d: &dyn Dialect, name: &str) -> String {
    d.fit_ident(&format!("{TRGM_PREFIX}{name}"))
}

fn check_shape(def: &IndexDefinition) -> Result<(), DataError> {
    if def.unique || def.partial.is_some() {
        return Err(DataError::invalid(
            "invalid_index",
            "trigram indexes cannot be unique or partial",
        ));
    }
    Ok(())
}

/// DDL creating trigram index `def` with the engine of `cat`'s database.
pub(crate) fn create_sql(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    check_shape(def)?;
    let q = |s: &str| d.quote_ident(s);
    let table = rel(d, &coll.name);
    let exprs = key_exprs(d, cat, coll, def, "")?;
    match TrigramEngine::of(d, cat.pg_trgm()) {
        // One `gin_trgm_ops` key per field, written without a cast so the
        // planner matches `~` (`ILIKE`) and `%` on the plain column (FB-166).
        TrigramEngine::PgTrgm => {
            let keys: Vec<String> = exprs
                .iter()
                .map(|e| format!("({e}) {EXTENSION_SCHEMA}.gin_trgm_ops"))
                .collect();
            Ok(vec![format!(
                "CREATE INDEX {} ON {table} USING gin ({})",
                q(&def.name),
                keys.join(", ")
            )])
        }
        // The query renders the same expression (`pg_array`), so the planner
        // matches the expression index.
        TrigramEngine::PgPortable => {
            let keys: Vec<String> = exprs.iter().map(|e| format!("({})", pg_array(e))).collect();
            Ok(vec![format!(
                "CREATE INDEX {} ON {table} USING gin ({})",
                q(&def.name),
                keys.join(", ")
            )])
        }
        TrigramEngine::SqlitePortable => {
            let side = side_table(d, &def.name);
            let mut out = vec![
                format!(
                    "CREATE TABLE {} (rid INTEGER NOT NULL, k INTEGER NOT NULL, tri TEXT NOT NULL, \
                     PRIMARY KEY (tri, k, rid)) WITHOUT ROWID",
                    q(&side)
                ),
                format!(
                    "CREATE INDEX {} ON {} (rid, k, tri)",
                    q(&format!("{side}_rid")),
                    q(&side)
                ),
            ];
            out.extend(sqlite_triggers(d, cat, coll, def)?);
            out.extend(sqlite_fill(d, cat, coll, def)?);
            Ok(out)
        }
    }
}

/// After SQLite rebuilt a collection's table (which drops its triggers):
/// the triggers again and the trigram table refilled from the new rows.
/// PostgreSQL never rebuilds tables; there this is [`create_sql`].
pub(crate) fn rebuild_sql(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    if is_pg(d) {
        return create_sql(d, cat, coll, def);
    }
    check_shape(def)?;
    let mut out = sqlite_triggers(d, cat, coll, def)?;
    out.push(format!("DELETE FROM {}", d.quote_ident(&side_table(d, &def.name))));
    out.extend(sqlite_fill(d, cat, coll, def)?);
    Ok(out)
}

/// DDL dropping trigram index `def` (and, on SQLite, its table and
/// triggers).
pub(crate) fn drop_sql(d: &dyn Dialect, def: &IndexDefinition) -> Vec<String> {
    if is_pg(d) {
        return vec![format!("DROP INDEX IF EXISTS {}", rel(d, &def.name))];
    }
    let side = side_table(d, &def.name);
    let q = |s: &str| d.quote_ident(s);
    vec![
        format!("DROP TRIGGER IF EXISTS {}", q(&format!("{side}_ai"))),
        format!("DROP TRIGGER IF EXISTS {}", q(&format!("{side}_ad"))),
        format!("DROP TRIGGER IF EXISTS {}", q(&format!("{side}_au"))),
        format!("DROP TABLE IF EXISTS {}", q(&side)),
    ]
}

/// `forgebase._forgebase_trgm((<expr>)::text)`: a value's trigram set on
/// PostgreSQL (index and query render it alike).
fn pg_array(expr: &str) -> String {
    format!("{PG_TRGM_FN}(({expr})::text)")
}

/// SQLite: the padded text the trigrams are windows of, for value `v` (any
/// SQL expression; `NULL` for a `NULL` or empty value). Walks the first
/// [`MAX_CHARS`] characters over `_forgebase_trgm_positions`; ASCII letters
/// and digits are word characters (`A-Z` lowercased), other ASCII separates
/// words (two spaces), other characters are read from `_forgebase_trgm_fold`
/// (absent: a word character as it is). `forgebase_query::trigram` does the
/// same in Rust.
fn sqlite_padded(v: &str) -> String {
    format!(
        "(SELECT '  ' || group_concat(CASE \
         WHEN c.u BETWEEN 97 AND 122 OR c.u BETWEEN 48 AND 57 THEN c.ch \
         WHEN c.u BETWEEN 65 AND 90 THEN char(c.u + 32) \
         WHEN c.u < 128 THEN '  ' \
         ELSE coalesce((SELECT CASE f.lower_cp WHEN 0 THEN '  ' ELSE char(f.lower_cp) END \
         FROM {FOLD} AS f WHERE f.cp = c.u), c.ch) END, '' ORDER BY c.i) || ' ' \
         FROM (SELECT p.i AS i, substr(x.v, p.i, 1) AS ch, unicode(substr(x.v, p.i, 1)) AS u \
         FROM (SELECT CAST(({v}) AS TEXT) AS v) AS x \
         JOIN {POSITIONS} AS p ON p.i <= min(length(x.v), {MAX_CHARS})) AS c)"
    )
}

/// SQLite: `INSERT` of the trigram rows of key `k` for the rows of `x`, a
/// relation of `(rid, s)` with `s` the padded text, computed once per row
/// (`x` must not be flattened into the join: the padded text is a
/// subquery). Windows start at every position (two a row of
/// `_forgebase_trgm_positions`, a range scan of its key); a window that
/// ends in two spaces is not a trigram.
fn sqlite_insert(side: &str, k: usize, x: &str) -> String {
    format!(
        "INSERT INTO {side} (rid, k, tri) SELECT DISTINCT x.rid, {k}, substr(x.s, 2 * p.i - 1 + h.o, 3) \
         FROM {x} \
         JOIN {POSITIONS} AS p ON p.i <= (length(x.s) - 1) / 2 \
         JOIN (SELECT 0 AS o UNION ALL SELECT 1) AS h ON 2 * p.i - 1 + h.o <= length(x.s) - 2 \
         WHERE substr(x.s, 2 * p.i + h.o, 2) <> '  '"
    )
}

fn sqlite_triggers(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    let q = |s: &str| d.quote_ident(s);
    let table = q(&coll.name);
    let side_name = side_table(d, &def.name);
    let side = q(&side_name);
    let new_vals = key_exprs(d, cat, coll, def, "new")?;
    let old_vals = key_exprs(d, cat, coll, def, "old")?;
    let inserts: String = new_vals
        .iter()
        .enumerate()
        .map(|(k, v)| {
            // `LIMIT 1` keeps SQLite from flattening the one-row subquery.
            let x = format!("(SELECT new.rowid AS rid, {} AS s LIMIT 1) AS x", sqlite_padded(v));
            format!("{};", sqlite_insert(&side, k, &x))
        })
        .collect::<Vec<_>>()
        .join(" ");
    let delete = format!("DELETE FROM {side} WHERE rid = old.rowid;");
    let changed = old_vals
        .iter()
        .zip(&new_vals)
        .map(|(o, n)| format!("({o}) IS NOT ({n})"))
        .collect::<Vec<_>>()
        .join(" OR ");
    Ok(vec![
        format!(
            "CREATE TRIGGER {} AFTER INSERT ON {table} BEGIN {inserts} END",
            q(&format!("{side_name}_ai"))
        ),
        format!(
            "CREATE TRIGGER {} AFTER DELETE ON {table} BEGIN {delete} END",
            q(&format!("{side_name}_ad"))
        ),
        format!(
            "CREATE TRIGGER {} AFTER UPDATE ON {table} WHEN {changed} BEGIN {delete} {inserts} END",
            q(&format!("{side_name}_au"))
        ),
    ])
}

/// SQLite: the trigram rows of every existing row (index creation, table
/// rebuild).
fn sqlite_fill(
    d: &dyn Dialect,
    cat: &CatalogSnapshot,
    coll: &Collection,
    def: &IndexDefinition,
) -> Result<Vec<String>, DataError> {
    let table = d.quote_ident(&coll.name);
    let side = d.quote_ident(&side_table(d, &def.name));
    Ok(key_exprs(d, cat, coll, def, "t")?
        .iter()
        .enumerate()
        .map(|(k, v)| {
            // Materialised: each row's padded text is computed once.
            let cte = format!(
                "WITH x (rid, s) AS MATERIALIZED (SELECT t.rowid, {} FROM {table} AS t) ",
                sqlite_padded(v)
            );
            format!("{cte}{}", sqlite_insert(&side, k, "x"))
        })
        .collect())
}

/// A rendered `fuzzy` condition: what to add to `WHERE` and the similarity
/// to order by (best over the fields, `float8`).
pub(crate) struct FuzzySql {
    /// The match condition (fields OR-ed).
    pub(crate) matches: String,
    /// The best similarity of a row, as a double.
    pub(crate) score: String,
}

/// Renders `fuzzy` over `fields` (paths that are keys of `trigram`) of row
/// `alias` for the search text `text` with `threshold` (`None`: the
/// default, which `pg_trgm`'s `%` reads from the server).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    engine: TrigramEngine,
    compiler: &Compiler<'_>,
    b: &mut SqlBuilder<'_>,
    coll: &Collection,
    alias: &str,
    fields: &[String],
    trigram: &[IndexDefinition],
    text: &str,
    threshold: Option<f64>,
) -> Result<FuzzySql, DataError> {
    let mut cols = Vec::with_capacity(fields.len());
    for path in fields {
        cols.push(compiler.scalar(b, coll, alias, &FieldPath::parse(path)?)?);
    }
    let (matches, sims): (Vec<String>, Vec<String>) = match engine {
        TrigramEngine::PgTrgm => {
            let p = b.bind(text.to_owned());
            let sims: Vec<String> = cols.iter().map(|c| format!("similarity({c}, {p})")).collect();
            let matches = match threshold {
                // `%` uses `pg_trgm.similarity_threshold` and the GIN index.
                None => cols.iter().map(|c| format!("{c} % {p}")).collect(),
                // An explicit threshold above the default stays index
                // assisted (`%` narrows, the comparison refines); below it
                // `%` would cut too much, so only the comparison runs.
                Some(t) => {
                    let tb = b.bind(Value::Real(t));
                    cols.iter()
                        .zip(&sims)
                        .map(|(c, sim)| {
                            if t >= DEFAULT_THRESHOLD {
                                format!("({c} % {p} AND {sim} >= {tb})")
                            } else {
                                format!("{sim} >= {tb}")
                            }
                        })
                        .collect()
                }
            };
            (matches, sims)
        }
        TrigramEngine::PgPortable => {
            let p = b.bind(text.to_owned());
            let t = threshold.unwrap_or(DEFAULT_THRESHOLD);
            let tb = b.bind(Value::Real(t));
            // An InitPlan: the search text's trigrams are computed once.
            let query = format!("(SELECT {PG_TRGM_FN}(CAST({p} AS text)))");
            let mut matches = Vec::with_capacity(cols.len());
            let mut sims = Vec::with_capacity(cols.len());
            for c in &cols {
                let arr = pg_array(c);
                let sim = format!("{PG_SIM_FN}({arr}, {query})");
                // Above 0 a match shares a trigram: `&&` uses the GIN index.
                matches.push(if t > 0.0 {
                    format!("({arr} && {query} AND {sim} >= {tb})")
                } else {
                    format!("{sim} >= {tb}")
                });
                sims.push(sim);
            }
            (matches, sims)
        }
        TrigramEngine::SqlitePortable => {
            let grams = trigram::trigrams(text);
            let t = threshold.unwrap_or(DEFAULT_THRESHOLD);
            let qn = i64::try_from(grams.len()).unwrap_or(i64::MAX);
            let min = i64::try_from(trigram::min_shared(grams.len(), t)).unwrap_or(i64::MAX);
            let qj = b.bind(serde_json::to_string(&grams)?);
            let qn = b.bind(qn);
            let tb = b.bind(Value::Real(t));
            let minb = b.bind(min);
            let d = b.dialect();
            let mut matches = Vec::with_capacity(cols.len());
            let mut sims = Vec::with_capacity(cols.len());
            for (path, c) in fields.iter().zip(&cols) {
                let (name, k) = trigram
                    .iter()
                    .find_map(|ix| ix.keys.iter().position(|key| &key.path == path).map(|k| (&ix.name, k)))
                    .ok_or_else(|| DataError::Internal(format!("no trigram index key for {path}")))?;
                let side = d.quote_ident(&side_table(d, name));
                let sim = format!(
                    "(CASE WHEN ({c}) IS NULL THEN NULL ELSE (SELECT CASE WHEN a.n = 0 THEN 0.0 \
                     ELSE CAST(a.m AS REAL) / (a.n + {qn} - a.m) END FROM (SELECT count(*) AS n, \
                     count(CASE WHEN s.tri IN (SELECT value FROM json_each({qj})) THEN 1 END) AS m \
                     FROM {side} AS s WHERE s.rid = {alias}.rowid AND s.k = {k}) AS a) END)"
                );
                matches.push(if t > 0.0 {
                    format!(
                        "({alias}.rowid IN (SELECT s.rid FROM {side} AS s WHERE s.k = {k} \
                         AND s.tri IN (SELECT value FROM json_each({qj})) GROUP BY s.rid \
                         HAVING count(*) >= {minb}) AND {sim} >= {tb})"
                    )
                } else {
                    format!("{sim} >= {tb}")
                });
                sims.push(sim);
            }
            (matches, sims)
        }
    };
    let score = match (engine, sims.len()) {
        (TrigramEngine::SqlitePortable, 1) => format!("CAST({} AS REAL)", sims[0]),
        // SQLite's max() is NULL when any argument is; a matching row has a
        // score on some field, so -1 never wins.
        (TrigramEngine::SqlitePortable, _) => format!(
            "max({})",
            sims.iter()
                .map(|s| format!("coalesce({s}, -1.0)"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (_, 1) => format!("({})::float8", sims[0]),
        _ => format!("(GREATEST({}))::float8", sims.join(", ")),
    };
    Ok(FuzzySql {
        matches: matches.join(" OR "),
        score,
    })
}

/// The SQLite objects behind one trigram index (ADR-0034 §1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteTrigramObjects {
    /// The trigram table `_forgebase_trgm_<index>`.
    pub table: String,
    /// The triggers that keep it: after insert, delete and update.
    pub triggers: [String; 3],
}

/// The names of the SQLite trigram table and triggers of index `index`.
/// They are derived from the index definition, so `forgebase db convert`
/// leaves them out of the table pairing and of the hand-made objects it
/// reports, and builds them again on a SQLite target.
pub fn sqlite_objects(index: &str) -> SqliteTrigramObjects {
    let table = side_table(&forgebase_db::SqliteDialect, index);
    SqliteTrigramObjects {
        triggers: [format!("{table}_ai"), format!("{table}_ad"), format!("{table}_au")],
        table,
    }
}

/// One trigram index taken out of a bulk load and built again after it
/// (`forgebase db convert`, ADR-0034 addendum). The statements are the
/// data core's own: [`create_sql`] on PostgreSQL, [`rebuild_sql`] on SQLite,
/// for the engine of the database the service runs on.
#[derive(Debug, Clone)]
pub struct TrigramReload {
    /// Index name.
    pub index: String,
    /// Collection name.
    pub collection: String,
    /// The engine the index is built with.
    pub engine: TrigramEngine,
    /// The collection's table, as SQL (quoted; `public.` on PostgreSQL).
    pub table: String,
    /// SQLite: the trigram table, as SQL (its rows are the index).
    pub side_table: Option<String>,
    /// Before the load: PostgreSQL drops the index, SQLite its triggers.
    pub suspend: Vec<String>,
    /// After the load: PostgreSQL creates the index; SQLite creates the
    /// triggers again and fills the trigram table from the loaded rows.
    pub reload: Vec<String>,
}

impl DataService {
    /// Every trigram index of this database, with the statements that take
    /// it out of a bulk load into its collection and build it afterwards
    /// (see [`TrigramReload`]). Read from `_forgebase_indexes` and the
    /// stored collection definitions, not from the cached catalog.
    ///
    /// # Errors
    /// A definition that does not read, or names a collection that does
    /// not exist; database errors.
    pub async fn trigram_reloads(&self) -> Result<Vec<TrigramReload>, DataError> {
        let backend = self.inner.backend.as_ref();
        let d = backend.dialect();
        let cat = CatalogSnapshot::load(backend).await?;
        let engine = TrigramEngine::of(d, cat.pg_trgm());
        let rows = backend
            .fetch_all(&forgebase_db::Statement::raw(
                "SELECT collection_id, definition FROM _forgebase_indexes WHERE kind = 'btree' ORDER BY name",
            ))
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            let mut def: IndexDefinition = serde_json::from_str(r[1].as_str().unwrap_or("{}"))?;
            if def.kind != crate::IndexKind::Trigram {
                continue;
            }
            let coll = r[0]
                .as_str()
                .and_then(|id| cat.get(id))
                .ok_or_else(|| DataError::Internal(format!("trigram index {} has no collection", def.name)))?;
            def.collection = coll.name.clone();
            let side = (!is_pg(d)).then(|| d.quote_ident(&side_table(d, &def.name)));
            let suspend = if is_pg(d) {
                drop_sql(d, &def)
            } else {
                sqlite_objects(&def.name)
                    .triggers
                    .iter()
                    .map(|t| format!("DROP TRIGGER IF EXISTS {}", d.quote_ident(t)))
                    .collect()
            };
            out.push(TrigramReload {
                reload: rebuild_sql(d, &cat, coll, &def)?,
                suspend,
                side_table: side,
                table: rel(d, &coll.name),
                engine,
                collection: coll.name.clone(),
                index: def.name,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_db::{PgDialect, SqliteDialect};

    fn def(name: &str, keys: &[&str]) -> IndexDefinition {
        IndexDefinition {
            name: name.into(),
            kind: crate::IndexKind::Trigram,
            ..IndexDefinition::btree("products", keys)
        }
    }

    #[test]
    fn engines_by_backend_and_extension() {
        assert_eq!(TrigramEngine::of(&SqliteDialect, true), TrigramEngine::SqlitePortable);
        assert_eq!(TrigramEngine::of(&PgDialect, true), TrigramEngine::PgTrgm);
        assert_eq!(TrigramEngine::of(&PgDialect, false), TrigramEngine::PgPortable);
        assert_eq!(TrigramEngine::PgPortable.name(), "portable");
    }

    #[test]
    fn drops_name_every_object() {
        let sqlite = drop_sql(&SqliteDialect, &def("ix", &["name"]));
        assert_eq!(sqlite.len(), 4);
        assert!(sqlite[3].contains("\"_forgebase_trgm_ix\""), "{sqlite:?}");
        assert_eq!(
            drop_sql(&PgDialect, &def("ix", &["name"])),
            ["DROP INDEX IF EXISTS public.\"ix\""]
        );
    }

    #[test]
    fn sqlite_object_names_match_the_ddl() {
        let o = sqlite_objects("ix");
        assert_eq!(o.table, "_forgebase_trgm_ix");
        let drops = drop_sql(&SqliteDialect, &def("ix", &["name"]));
        for (t, sql) in o.triggers.iter().zip(&drops) {
            assert_eq!(sql, &format!("DROP TRIGGER IF EXISTS \"{t}\""));
        }
    }
}
