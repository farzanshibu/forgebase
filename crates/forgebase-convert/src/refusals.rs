//! What `--to sqlite` refuses (ADR-0028 §6.4, FB-158 addendum): objects
//! that exist only on PostgreSQL and data still held by an extension. Every
//! problem is listed, so one run names everything to fix.

use sqlx::PgConnection;

use crate::ConvertError;
use crate::pg::text_rows;

/// What [`check`] found that is dropped rather than refused.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    /// Refusals, one line each.
    pub problems: Vec<String>,
    /// Notes for the report (a derived AGE graph is dropped).
    pub notes: Vec<String>,
}

async fn names(conn: &mut PgConnection, sql: &str) -> Result<Vec<String>, ConvertError> {
    Ok(text_rows(conn, sql)
        .await?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect())
}

async fn schema_exists(conn: &mut PgConnection, schema: &str) -> Result<bool, ConvertError> {
    Ok(!names(
        conn,
        &format!(
            "SELECT nspname::text FROM pg_namespace WHERE nspname = {}",
            crate::pg::literal(schema)
        ),
    )
    .await?
    .is_empty())
}

/// Checks a PostgreSQL source before `--to sqlite`. `views` are the view
/// collections of the catalog (their views are recreated, not refused).
pub(crate) async fn check(conn: &mut PgConnection, views: &[String]) -> Result<Findings, ConvertError> {
    let mut f = Findings::default();
    let p = &mut f.problems;

    // Topology: one set, no replicas (ADR-0029 §3; FB-179 is the multi-set tool).
    let sets = names(
        conn,
        "SELECT name::text FROM forgebase._forgebase_db_sets WHERE state <> 'retired' ORDER BY number",
    )
    .await
    .unwrap_or_default();
    if sets.len() > 1 {
        p.push(format!(
            "the cluster has {} database sets ({}); SQLite holds one: drain and remove the shards first",
            sets.len(),
            sets.join(", ")
        ));
    }
    let replicas = names(
        conn,
        "SELECT name::text FROM forgebase._forgebase_db_nodes WHERE role = 'replica' AND state <> 'retired' ORDER BY name",
    )
    .await
    .unwrap_or_default();
    if !replicas.is_empty() {
        p.push(format!(
            "the cluster has replicas ({}); remove them first (SQLite has no replicas)",
            replicas.join(", ")
        ));
    }

    // Row-level security policies written by hand (ForgeBase's own are `fb_*`).
    for n in names(
        conn,
        "SELECT format('%s on %I.%I', pol.polname, n.nspname, c.relname) FROM pg_policy pol \
         JOIN pg_class c ON c.oid = pol.polrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname IN ('forgebase', 'public') AND pol.polname NOT LIKE 'fb\\_%' ORDER BY 1",
    )
    .await?
    {
        p.push(format!(
            "row-level security policy {n} was written by hand; SQLite has no policies"
        ));
    }

    // Functions in `public` that no extension owns (PL/pgSQL and others).
    for n in names(
        conn,
        "SELECT format('%I(%s) [%s]', p.proname, pg_get_function_identity_arguments(p.oid), l.lanname) \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace JOIN pg_language l ON l.oid = p.prolang \
         WHERE n.nspname = 'public' AND NOT EXISTS \
         (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') \
         ORDER BY 1",
    )
    .await?
    {
        p.push(format!("function public.{n} exists only on PostgreSQL"));
    }

    // Triggers written by hand on collection tables.
    for n in names(
        conn,
        "SELECT format('%s on %I', t.tgname, c.relname) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND NOT t.tgisinternal ORDER BY 1",
    )
    .await?
    {
        p.push(format!("trigger {n} exists only on PostgreSQL"));
    }

    // Types, domains and free-standing sequences made in `public`.
    for n in names(
        conn,
        "SELECT format('type %I', t.typname) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
         WHERE n.nspname = 'public' AND t.typtype IN ('e', 'd', 'r', 'm') AND NOT EXISTS \
         (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e') \
         UNION ALL SELECT format('sequence %I', c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind = 'S' AND NOT EXISTS \
         (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.deptype IN ('a', 'i')) \
         ORDER BY 1",
    )
    .await?
    {
        p.push(format!("{n} exists only on PostgreSQL"));
    }

    // Views that are not view collections.
    for n in names(
        conn,
        "SELECT format('%s %I', CASE c.relkind WHEN 'm' THEN 'materialized view' ELSE 'view' END, c.relname) \
         || E'\\t' || c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'public' AND c.relkind IN ('v', 'm') ORDER BY 1",
    )
    .await?
    {
        let (label, rel) = n.split_once('\t').unwrap_or((&n, ""));
        if !views.iter().any(|v| v == rel) {
            p.push(format!(
                "{label} is not a view collection; recreate it on SQLite by hand"
            ));
        }
    }

    // Columns of extension types (citext is mapped to TEXT COLLATE NOCASE;
    // the pgvector column is checked with the vector data below).
    for n in names(
        conn,
        "SELECT format('%I.%I.%I (%s from extension %s)', ns.nspname, c.relname, a.attname, t.typname, e.extname) \
         FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace ns ON ns.oid = c.relnamespace \
         JOIN pg_type t ON t.oid = a.atttypid \
         JOIN pg_depend d ON d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e' \
         JOIN pg_extension e ON e.oid = d.refobjid \
         WHERE ns.nspname IN ('forgebase', 'public') AND c.relkind IN ('r', 'p') AND a.attnum > 0 \
         AND NOT a.attisdropped AND e.extname <> 'citext' \
         AND NOT (c.relname = '_forgebase_vector_values' AND a.attname = 'vec') ORDER BY 1",
    )
    .await?
    {
        p.push(format!("column {n} has a type only PostgreSQL has"));
    }

    // Extension data (FB-162, FB-164, FB-165).
    let vec_col = names(
        conn,
        "SELECT 1::text FROM pg_attribute WHERE attrelid = to_regclass('forgebase._forgebase_vector_values') \
         AND attname = 'vec' AND NOT attisdropped",
    )
    .await?;
    if !vec_col.is_empty() {
        let n = crate::pg::scalar_i64(
            conn,
            "SELECT count(*)::bigint FROM forgebase._forgebase_vector_values WHERE vec IS NOT NULL",
        )
        .await?
        .unwrap_or(0);
        if n > 0 {
            p.push(format!(
                "{n} vector(s) are stored in pgvector: run the vector.convert job with {{\"to\": \"portable\"}} \
                 first (forgebase db convert --to sqlite does it unless --no-prepare is given)"
            ));
        }
    }
    if schema_exists(conn, "pgmq").await? {
        let queues = names(
            conn,
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'pgmq' AND c.relkind = 'r' AND c.relname LIKE 'q\\_fbs\\_%' ORDER BY 1",
        )
        .await?;
        let mut owed = 0;
        for q in &queues {
            owed += crate::pg::scalar_i64(
                conn,
                &format!("SELECT count(*)::bigint FROM pgmq.{}", crate::pg::quote(q)),
            )
            .await?
            .unwrap_or(0);
        }
        if !queues.is_empty() {
            p.push(format!(
                "{} subscription queue(s) with {owed} delivery(ies) are held by pgmq: run the queue.convert job \
                 with {{\"to\": \"portable\"}} first (forgebase db convert --to sqlite does it unless --no-prepare \
                 is given)",
                queues.len()
            ));
        }
    }
    if schema_exists(conn, "cron").await? {
        let jobs = names(
            conn,
            "SELECT jobname::text FROM cron.job WHERE jobname LIKE 'fbsched\\_%' ORDER BY 1",
        )
        .await
        .unwrap_or_default();
        if !jobs.is_empty() {
            p.push(format!(
                "{} schedule(s) are fired by pg_cron: start the server once with \
                 [database.postgres.extensions] cron = \"off\" to hand them back to the ForgeBase tick \
                 (forgebase db convert --to sqlite does it unless --no-prepare is given)",
                jobs.len()
            ));
        }
    }

    // A derived AGE graph is dropped, not refused (the graph port is the
    // canonical store; FB-163 rebuilds the AGE graph from it).
    if schema_exists(conn, "ag_catalog").await? {
        let graphs = names(conn, "SELECT name::text FROM ag_catalog.ag_graph ORDER BY 1")
            .await
            .unwrap_or_default();
        if !graphs.is_empty() {
            f.notes.push(format!(
                "Apache AGE graph(s) {} are derived from the graph tables and are not copied",
                graphs.join(", ")
            ));
        }
    }
    Ok(f)
}
