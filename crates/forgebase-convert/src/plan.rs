//! Which tables are copied and how: the PostgreSQL catalog is the list
//! (ADR-0028 §6.4 step 4), so a table missing on either side fails the run.

use crate::canon::Kind;
use crate::pg::{PgTable, quote as pq};
use crate::sqlite::{SqliteTable, quote as sq};

/// The migration ledger: each engine keeps its own (same versions, its own
/// checksums), never copied.
pub(crate) const LEDGER: &str = "_forgebase_migrations";

/// Derived from `_forgebase_vector_items.content` (an FTS5 table on SQLite,
/// a `tsvector` table on PostgreSQL): rebuilt, not copied.
pub(crate) const VECTOR_FTS: &str = "_forgebase_vector_fts";

/// Rows of `_forgebase_meta` that are not copied: the extension pins and
/// DDL versions of FB-161 (`ext.<feature>.*`). They describe the old
/// engine; the target's first start decides again (portable data, auto
/// mode accelerates).
pub(crate) const META_FILTER: (&str, &str) = ("_forgebase_meta", "key NOT LIKE 'ext.%'");

/// One copied column.
#[derive(Debug, Clone)]
pub(crate) struct ColumnPlan {
    /// Name on PostgreSQL.
    pub pg: String,
    /// Name on SQLite (`rowid` for `_rowid`).
    pub sqlite: String,
    /// Comparison and conversion kind.
    pub kind: Kind,
}

/// One copied table.
#[derive(Debug, Clone)]
pub(crate) struct TablePlan {
    pub pg: PgTable,
    pub sqlite: String,
    pub columns: Vec<ColumnPlan>,
    /// Positions (in `columns`) of the primary key; empty = none.
    pub pk: Vec<usize>,
    /// A row filter, the same SQL on both engines.
    pub filter: Option<&'static str>,
    /// PostgreSQL columns SQLite does not have: must be NULL in every row.
    pub pg_only: Vec<String>,
}

impl TablePlan {
    /// `SELECT … FROM … [WHERE …] ORDER BY …` on PostgreSQL.
    pub fn pg_select(&self) -> String {
        let cols: Vec<String> = self.columns.iter().map(|c| pq(&c.pg)).collect();
        let mut sql = format!("SELECT {} FROM {}", cols.join(", "), self.pg.qualified());
        if let Some(f) = self.filter {
            sql.push_str(&format!(" WHERE {f}"));
        }
        if !self.pk.is_empty() {
            let keys: Vec<String> = self
                .pk
                .iter()
                .map(|&i| {
                    let c = &self.columns[i];
                    if c.kind.is_textual() {
                        format!("{}::text COLLATE \"C\"", pq(&c.pg))
                    } else {
                        pq(&c.pg)
                    }
                })
                .collect();
            sql.push_str(&format!(" ORDER BY {}", keys.join(", ")));
        }
        sql
    }

    /// The same rows on SQLite, in the same order (byte order for text).
    pub fn sqlite_select(&self) -> String {
        let col = |c: &ColumnPlan| {
            if c.sqlite == "rowid" {
                "rowid".to_owned()
            } else {
                sq(&c.sqlite)
            }
        };
        let cols: Vec<String> = self.columns.iter().map(col).collect();
        let mut sql = format!("SELECT {} FROM {}", cols.join(", "), sq(&self.sqlite));
        if let Some(f) = self.filter {
            sql.push_str(&format!(" WHERE {f}"));
        }
        if !self.pk.is_empty() {
            let keys: Vec<String> = self
                .pk
                .iter()
                .map(|&i| {
                    let c = &self.columns[i];
                    if c.kind.is_textual() {
                        format!("{} COLLATE BINARY", col(c))
                    } else {
                        col(c)
                    }
                })
                .collect();
            sql.push_str(&format!(" ORDER BY {}", keys.join(", ")));
        }
        sql
    }

    /// Column kinds, in copy order.
    pub fn kinds(&self) -> Vec<Kind> {
        self.columns.iter().map(|c| c.kind).collect()
    }

    /// `schema.table` for reports.
    pub fn label(&self) -> String {
        format!("{}.{}", self.pg.schema, self.pg.name)
    }
}

/// What was left out on purpose.
#[derive(Debug, Default)]
pub(crate) struct Skipped {
    /// SQLite tables that are engine internals (FTS5 tables and their
    /// shadow tables, `sqlite_*`).
    pub sqlite_internal: Vec<String>,
    /// SQLite tables derived from an index definition and built again on
    /// the target (the trigram tables of ADR-0034).
    pub sqlite_derived: Vec<String>,
}

fn shadow_of(name: &str, virtuals: &[&str]) -> bool {
    virtuals.iter().any(|v| {
        name.strip_prefix(v)
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|suffix| matches!(suffix, "data" | "idx" | "content" | "docsize" | "config"))
    })
}

/// Pairs the tables of both engines. `derived` names SQLite tables that
/// are built from an index definition (trigram tables): never copied.
/// `Err` lists every table or column found on one side only.
pub(crate) fn pair(
    pg: &[PgTable],
    sqlite: &[SqliteTable],
    rowid_tables: &[&str],
    derived: &[String],
) -> Result<(Vec<TablePlan>, Skipped), Vec<String>> {
    let mut problems = Vec::new();
    let mut plans = Vec::new();
    let mut skipped = Skipped::default();
    let virtuals: Vec<&str> = sqlite
        .iter()
        .filter(|t| t.is_virtual)
        .map(|t| t.name.as_str())
        .collect();
    let mut used = vec![false; sqlite.len()];
    for (i, t) in sqlite.iter().enumerate() {
        if t.name.starts_with("sqlite_") || t.is_virtual || shadow_of(&t.name, &virtuals) || t.name == LEDGER {
            used[i] = true;
            if t.name != LEDGER {
                skipped.sqlite_internal.push(t.name.clone());
            }
        } else if derived.contains(&t.name) {
            used[i] = true;
            skipped.sqlite_derived.push(t.name.clone());
        }
    }
    for t in pg {
        if t.name == LEDGER || t.name == VECTOR_FTS {
            continue;
        }
        let found = sqlite
            .iter()
            .position(|s| s.name == t.name)
            .or_else(|| sqlite.iter().position(|s| s.name.eq_ignore_ascii_case(&t.name)));
        let Some(si) = found.filter(|&i| !sqlite[i].is_virtual) else {
            problems.push(format!(
                "table {}.{} exists on PostgreSQL but not on SQLite",
                t.schema, t.name
            ));
            continue;
        };
        used[si] = true;
        let s = &sqlite[si];
        let rowid = rowid_tables.contains(&t.name.as_str());
        let mut columns = Vec::new();
        let mut pg_only = Vec::new();
        let mut seen = vec![false; s.columns.len()];
        for c in t.columns.iter().filter(|c| !c.generated) {
            if rowid && c.name == "_rowid" {
                columns.push(ColumnPlan {
                    pg: c.name.clone(),
                    sqlite: "rowid".into(),
                    kind: c.kind,
                });
                continue;
            }
            let m = s
                .columns
                .iter()
                .position(|(n, _)| n == &c.name)
                .or_else(|| s.columns.iter().position(|(n, _)| n.eq_ignore_ascii_case(&c.name)));
            match m {
                Some(j) => {
                    seen[j] = true;
                    columns.push(ColumnPlan {
                        pg: c.name.clone(),
                        sqlite: s.columns[j].0.clone(),
                        kind: c.kind,
                    });
                }
                None => pg_only.push(c.name.clone()),
            }
        }
        for (j, (n, _)) in s.columns.iter().enumerate() {
            if !seen[j] {
                problems.push(format!("column {}.{n} exists on SQLite but not on PostgreSQL", t.name));
            }
        }
        let pk =
            t.pk.iter()
                .filter_map(|k| columns.iter().position(|c| &c.pg == k))
                .collect::<Vec<_>>();
        let pk = if pk.len() == t.pk.len() { pk } else { Vec::new() };
        plans.push(TablePlan {
            pg: t.clone(),
            sqlite: s.name.clone(),
            columns,
            pk,
            filter: (t.name == META_FILTER.0).then_some(META_FILTER.1),
            pg_only,
        });
    }
    for (i, s) in sqlite.iter().enumerate() {
        if !used[i] {
            problems.push(format!("table {} exists on SQLite but not on PostgreSQL", s.name));
        }
    }
    if problems.is_empty() {
        Ok((plans, skipped))
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::PgColumn;

    fn pgt(name: &str, cols: &[(&str, &str)], pk: &[&str]) -> PgTable {
        PgTable {
            schema: "forgebase".into(),
            name: name.into(),
            columns: cols
                .iter()
                .map(|(n, t)| PgColumn {
                    name: (*n).into(),
                    kind: Kind::of_pg(t),
                    generated: *t == "tsvector",
                    identity: false,
                })
                .collect(),
            pk: pk.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn st(name: &str, cols: &[&str], virt: bool) -> SqliteTable {
        SqliteTable {
            name: name.into(),
            is_virtual: virt,
            columns: cols.iter().map(|c| ((*c).to_owned(), 0)).collect(),
        }
    }

    #[test]
    fn pairs_tables_and_skips_engine_internals() {
        let pg = vec![
            pgt("_forgebase_jobs", &[("id", "text"), ("_rowid", "int8")], &["id"]),
            pgt(LEDGER, &[("version", "int8")], &["version"]),
            pgt(VECTOR_FTS, &[("rowid", "int8"), ("fts", "tsvector")], &["rowid"]),
            pgt("_forgebase_meta", &[("key", "text"), ("value", "text")], &["key"]),
        ];
        let sqlite = vec![
            st("_forgebase_jobs", &["id"], false),
            st(LEDGER, &["version"], false),
            st(VECTOR_FTS, &[], true),
            st("_forgebase_vector_fts_data", &["id"], false),
            st("_forgebase_meta", &["key", "value"], false),
            st("sqlite_sequence", &["name", "seq"], false),
        ];
        let (plans, skipped) = pair(&pg, &sqlite, &["_forgebase_jobs"], &[]).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].columns[1].sqlite, "rowid");
        assert!(plans[0].sqlite_select().contains("rowid"));
        assert_eq!(plans[1].filter, Some(META_FILTER.1));
        assert!(plans[1].pg_select().ends_with("ORDER BY \"key\"::text COLLATE \"C\""));
        assert!(
            skipped
                .sqlite_internal
                .contains(&"_forgebase_vector_fts_data".to_owned())
        );
    }

    #[test]
    fn one_sided_tables_and_columns_fail() {
        let pg = vec![pgt("a", &[("id", "text")], &["id"]), pgt("b", &[("id", "text")], &[])];
        let sqlite = vec![st("a", &["id", "extra"], false), st("c", &["id"], false)];
        let problems = pair(&pg, &sqlite, &[], &[]).unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
    }

    #[test]
    fn trigram_tables_are_derived_not_one_sided() {
        let pg = vec![pgt("things", &[("id", "text"), ("t", "text")], &["id"])];
        let sqlite = vec![
            st("things", &["id", "t"], false),
            st("_forgebase_trgm_things_tri", &["rid", "k", "tri"], false),
        ];
        let derived = ["_forgebase_trgm_things_tri".to_owned()];
        let (plans, skipped) = pair(&pg, &sqlite, &[], &derived).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(skipped.sqlite_derived, derived);
        // Without its index definition it is a one-sided table.
        let problems = pair(&pg, &sqlite, &[], &[]).unwrap_err();
        assert!(problems[0].contains("_forgebase_trgm_things_tri"), "{problems:?}");
    }
}
