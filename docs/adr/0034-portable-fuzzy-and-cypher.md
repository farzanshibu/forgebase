# ADR-0034: Portable fuzzy search, portable openCypher, capability matrix

- Status: accepted
- Date: 2026-11-14
- Scope: a portable engine for the `trigram` index kind and the `fuzzy`
  list parameters (SQLite, and PostgreSQL without `pg_trgm`); a portable
  engine for `POST /api/v1/graph/cypher` (SQLite, and PostgreSQL without
  Apache AGE); one endpoint that says which engine runs each feature.
- Builds on: [ADR-0029](0029-postgres-extensions-replicas-sharding.md)
  (decision 1: extensions are accelerators with a portable fallback;
  §1.5 and the FB-163 addendum: openCypher on AGE; §1.8 and the FB-166
  addendum: `pg_trgm`), [ADR-0003](0003-data-core.md) (index manager,
  list queries), [ADR-0006](0006-graph.md) (graph tables, rules).
- Migration: `0056_portable_trigram` (both backends). Version 0057 is
  reserved for this work and unused.

## Context

ADR-0029 decision 1 makes every PostgreSQL extension an accelerator behind
an unchanged API, with a portable implementation where the extension is
missing. Two features had no portable implementation:

- **Fuzzy search.** Decision 7 and FB-166 made the `trigram` index kind
  and `fuzzy` / `fuzzy_fields` / `fuzzy_threshold` PostgreSQL-with-
  `pg_trgm` only: SQLite and `trigram = "off"` answered `400
  index_kind_unsupported` / `400 fuzzy_unsupported`.
- **openCypher.** Decision 4 and FB-163 run `POST /graph/cypher` on a
  derived AGE graph. AGE has no Windows build and most hosted PostgreSQL
  servers lack it; SQLite, Windows engines and those servers answered `400
  cypher_unsupported`.

The gaps were confirmed in the code before this work:
`DataService::trigram_indexes` and `create_index` refused without
`Feature::Trigram`, `create_index_sql` refused on SQLite, and
`GraphService::cypher` returned `CypherUnsupported` without
`Feature::Graph`.

## Decisions at a glance

| # | Decision |
|---|---|
| 1 | Fuzzy search runs everywhere with `pg_trgm`'s semantics. Three engines: `pg_trgm` (accelerator), PostgreSQL portable (SQL functions + a GIN index over `text[]`), SQLite portable (a trigger-maintained trigram table per index). |
| 2 | SQLite needs no loadable code: trigrams are computed in SQL over two seeded reference tables (positions, a Unicode fold table), because the workspace forbids `unsafe` and sqlx 0.9 has no safe scalar-function API. |
| 3 | An index is built by the engine in use when it is created (and on each shard by the same rule); a query uses the engine in use now and stays correct on any index. |
| 4 | openCypher runs everywhere: where AGE is not in use, a portable engine parses a read-only subset (tokenizer + recursive descent, `forgebase-query`) and compiles it to one SQL statement over the canonical graph tables (`forgebase-graph`). |
| 5 | Same endpoint, request, permission, rules, tenant isolation and response shape on both engines; the response gains `engine: "age" \| "portable"`. The engine follows `[database.postgres.extensions] graph` (`auto`/`on`/`off`) through `Database::feature_active(Feature::Graph)`; there is no request override. |
| 6 | Outside the subset: `400 cypher_unsupported` naming the construct and its line and column; writes `400 cypher_read_only` (as on AGE); syntax `400 cypher_invalid`; limits `400 cypher_too_complex`. |
| 7 | Where the two engines could differ, the portable engine follows AGE 1.8 (direction is the stored one, `n.__id` reads the ForgeBase id, a walk may reuse an edge of another relationship of the clause). A differential test runs every subset query on both. |
| 8 | `GET /api/system/capabilities` (and `database.capabilities` of `/api/system/info`) lists every feature with backend, engine, accelerator or not, state, mode, reason and limits. `docs/feature-parity.md` is the human table. |

## 1. Portable fuzzy search

### 1.1 Semantics

`forgebase_query::trigram` states the model, `pg_trgm`'s: the text is
lowercased and split into words (runs of letters and digits); each word is
padded with two spaces in front and one behind; its trigrams are the
three-character windows of the padded word; a value's trigram *set* has
each once; similarity is `|shared| / (|a| + |b| - |shared|)`, `0` when
either set is empty; a row matches when its similarity reaches the
threshold (default `0.3`, `pg_trgm.similarity_threshold`'s); results are
ordered by the best similarity over the fuzzy fields, then by id, unless
`sort` is given.

**Tolerance (documented, tested).**

| Engine | Equal to `pg_trgm` on the same server | Differs |
|---|---|---|
| PostgreSQL portable | Classification and case by the server's own `lower()` and `[[:alnum:]]`, i.e. what `pg_trgm` uses; similarity in `real` as `pg_trgm` computes it. | `pg_trgm` stores multibyte trigrams as 3-byte CRCs: two different trigrams could collide there and not here. |
| SQLite portable | ASCII exactly (letters, digits, separators, case). The fold table (1266 code points of Latin-1, Latin Extended A/B, IPA, Greek, Cyrillic, Armenian, Hebrew, Latin Extended Additional, Greek Extended, general punctuation, currency, CJK punctuation, full-width forms) gives `pg_trgm`'s behaviour under the bundled engine's `C.UTF-8` locale: letters lowercased (Unicode simple lowercase), non-letters separate words. | Characters outside those blocks are word characters compared as they are (emoji and CJK ideographs count as letters; no case folding); only the first 4096 characters of a value count; similarity in double precision (orderings equal for values of normal length). |

The test engine of the suites uses the `C` locale, where `pg_trgm` treats
every non-ASCII character as a separator; the bundled engine uses
`C.UTF-8`. The corpus tests therefore compare on ASCII and check the
Unicode classification of the SQLite engine against the Rust model.

### 1.2 SQLite engine

A trigram index `ix` on keys `k0 … kn` is:

- `_forgebase_trgm_ix (rid INTEGER, k INTEGER, tri TEXT, PRIMARY KEY (tri, k, rid)) WITHOUT ROWID`
  and an index on `(rid, k, tri)`;
- three triggers (`…_ai`, `…_ad`, `…_au`; the update trigger only when a
  key value changed) that delete and re-insert the row's trigrams;
- a fill of the existing rows (a materialised CTE, so each value's padded
  text is computed once).

The trigram SQL walks the first 4096 characters of a value over
`_forgebase_trgm_positions` (1 … 4100): ASCII `0-9 a-z` stay, `A-Z` are
lowercased, other ASCII becomes two spaces, other characters are looked up
in `_forgebase_trgm_fold` (`lower_cp = 0`: two spaces). The padded text is
`'  ' || … || ' '`; every window whose last two characters are not both
spaces is a trigram (windows ending in two spaces straddle or sit between
words), two windows per position row. `forgebase_query::trigram::trigrams`
does the same in Rust for the search text; tests compare the trigger's
rows with the Rust model on a Unicode corpus.

A query binds the search text's trigrams as JSON. Candidates are rows that
share at least `⌈t × |q|⌉` trigrams (similarity ≤ shared / |q|), found
through the primary key; each candidate's score is
`shared / (n + |q| - shared)` from two range scans of `(rid, k, tri)`. A
threshold of `0` matches every row with a value, as `similarity >= 0`
does. Plan test: the fuzzy list is `SEARCH s USING PRIMARY KEY (tri=? AND
k=?)` plus rowid lookups, no table scan.

Life cycle: a table rebuild (dropping a field) re-creates the triggers and
refills the table; a renamed field carries the index (SQLite rewrites the
triggers, the index definition follows the rename); dropping the index or
the collection drops the trigram table and triggers.

### 1.3 PostgreSQL portable engine

Migration 0056 adds `forgebase._forgebase_trgm(text) RETURNS text[]`
(IMMUTABLE: the sorted trigram set, `regexp_split_to_table(lower(t),
'[^[:alnum:]]+')`, padded words, every window once) and
`forgebase._forgebase_trgm_similarity(text[], text[]) RETURNS real`. A
trigram index is `CREATE INDEX ix ON t USING gin ((forgebase._forgebase_trgm((col)::text)), …)`
with the built-in `array_ops`, so `CREATE INDEX CONCURRENTLY` and the
shard roll-out of FB-173 (`_forgebase_index_builds`) work unchanged. The
query renders the same expression: `trgm(col) && (SELECT trgm($q))` (an
InitPlan, computed once) narrows through the index, `similarity(…) >= t`
decides. Plan test: `fuzzy` uses the index with `trigram = "off"`.

### 1.4 Engine selection and switching

`TrigramEngine::of(dialect, pg_trgm)` picks the engine; DDL reads it from
the catalog snapshot (`CatalogSnapshot::pg_trgm`, set from
`Feature::Trigram` when the snapshot is loaded), queries from the service.
Switching `trigram` between `pg_trgm` and portable needs no conversion:
both answer correctly on either kind of index; the index of the other
engine is not used (a sequential scan) until the index is re-created
(drop + create). `_forgebase_indexes.kind` keeps saying `btree` for a
trigram index (FB-166); `definition.kind` says `trigram`.

**Changed API.** `index_kind_unsupported` and `fuzzy_unsupported` are no
longer returned (the codes stay reserved). `no_trigram_index`,
`invalid_fuzzy_threshold` and `invalid_index` are unchanged.

## 2. Portable openCypher

### 2.1 The subset

Parsed by `forgebase_query::cypher` (a tokenizer with spans and a
recursive-descent parser; no regular expressions):

- one or more `MATCH pattern, … [WHERE expr]`, then one `RETURN [DISTINCT]
  item [AS alias], … [ORDER BY expr [ASC|DESC], …] [SKIP n] [LIMIT n]`; a
  bare `RETURN` too;
- node patterns `(v:Label {key: expr})`, relationship patterns with
  variable, types (`:A|B`), direction (`->`, `<-`, `-`, and `-->`, `<--`,
  `--`), property maps, and bounded lengths `*n`, `*min..max`, `*..max`
  (at most 10 hops);
- expressions: literals (null, booleans, integers, floats, strings,
  lists), `$parameters`, variables, property access (`n.a.b`), `=` `<>`
  `<` `<=` `>` `>=`, `AND` `OR` `NOT`, `IN`, `IS [NOT] NULL`, `STARTS WITH`,
  `ENDS WITH`, `CONTAINS`, unary minus;
- functions: `count` (also `count(*)`, `DISTINCT`), `collect`, `min`,
  `max`, `sum`, `avg`, `type`, `labels`, `size`, `coalesce`, `toLower`,
  `toUpper`.

Everything else answers `cypher_unsupported` with the construct and
position: `WITH`, `OPTIONAL MATCH`, `UNWIND`, `UNION`, `CASE`, arithmetic,
path variables, `shortestPath`, unbounded `*`, pattern predicates and
comprehensions, map literals, list indexing, `=~`, `XOR`, label
expressions, `EXISTS`, `id()` and other functions.

### 2.2 Compilation

One `SELECT` per query (`forgebase-graph/src/portable/compile.rs`): a node
variable is a row of `_forgebase_graph_nodes`, a relationship one of
`_forgebase_graph_edges`; an anonymous unconstrained node at the end of a
chain gets no row (the foreign key proves it exists) unless a rule
applies. A variable-length relationship is a recursive CTE that enumerates
paths (`start`, `node`, `depth`, `path`), started at the more constrained
end of the pattern (property map, then label), with the start node's
labels, constant properties, rules and the `WHERE` conjuncts about it
alone pushed into the anchor. Each step checks the depth, edge type, edge
property map, edge-type rule, the next node's label rule, and that the
path has not used the edge. One row per path, as on AGE.

Values are compiled with a static type: SQL booleans, numbers and text
where the type is known (literals, parameters, ids, aggregates), JSON
where it is not (properties: JSON text on SQLite, `jsonb` on PostgreSQL).
Comparisons follow openCypher: different types are not equal (`=` false,
`<>` true, ordering null), `null` propagates; a JSON value compared with a
typed one is guarded by its JSON type. Results are JSON in ForgeBase's
shapes (nodes `{id, label, properties}`, edges `{id, type, source, target,
directed, properties}`, variable-length relationships as edge lists in
pattern order), exactly the AGE engine's. Aggregation groups by the other
`RETURN` items (nodes and edges by their key); `ORDER BY` after
`DISTINCT` or aggregation may use returned columns and expressions over
returned variables; nulls sort last ascending.

### 2.3 Security and limits

- Same permission (`engine:graph_cypher`, not implied by `graph_read`),
  same rules (`GraphRead` `list`): every node row and every node a walk
  enters passes the label rules, every edge its edge-type rules, as in the
  REST API (the AGE engine enforces them with RLS; the differential test
  compares both for a caller under rules).
- Tenant isolation: every alias filters on the caller's tenant, bound
  once; edges join nodes of their own tenant.
- Read-only by construction (one `SELECT` on the read pool); write clauses
  are refused by the parser.
- Every value is a bound parameter; SQL text comes from fixed fragments,
  validated names and compiled rules.
- Caps: query 64 KiB, 32 node and relationship patterns, expression
  nesting 64, 100 columns, 10 hops, `limit` ≤ 1000 rows (more =
  `truncated`), and at most 100 000 paths per variable-length relationship,
  counted by a preflight statement before the query runs
  (`cypher_too_complex`). `timeout_ms` is `statement_timeout` on
  PostgreSQL; on SQLite the request stops waiting (`cypher_timeout`) while
  the statement finishes on its connection.

### 2.4 Where the engines differ

The differential test (`tests/cypher_differential.rs`, PostgreSQL with
AGE) runs 46 queries as an administrator and as a caller under rules, and
two with parameters, and compares columns and rows: all 94 agree. Known differences outside that set:

- AGE 1.8 does not accept `:A|B` type alternatives; the portable engine
  does.
- The portable engine matches labels AGE cannot name directly (``(n:`my-label`)``);
  on AGE they sit in the catch-all label `_fbnode` with `__label`.
- `labels(n)` of a node with the empty label: `[""]` portable, AGE's
  catch-all label on AGE.
- `min`/`max` over mixed types: the portable engine takes numbers when
  there are any, else strings.
- `toLower`/`toUpper` on SQLite change ASCII only.

### 2.5 Engine selection

`GraphService::cypher` runs AGE when `feature_active(Feature::Graph)`
(PostgreSQL with AGE usable and `graph` not `off`), else the portable
engine; `GraphService::cypher_engine()` says which, the response's
`engine` too. `GraphService::cypher_portable` runs the portable engine
regardless (tools, the differential test). `POST /graph/cypher/drop`, the
`graph.materialise` job and `stats.cypher` exist for AGE only (`400
cypher_unsupported` elsewhere, unchanged).

## 3. Capability matrix

`GET /api/system/capabilities` → `{backend, features: [{feature, label,
engine, accelerated, state, mode, extension, reason, limits}]}` for
`vector`, `graph` (REST traversal), `opencypher`, `fulltext`, `fuzzy`,
`queue`, `cron`, `kv`, `realtime`, `read_replicas`, `sharding`. `state` is
`active` (on its extension), `portable` (SQLite, or the extension is not
usable: `reason`), `off` (the mode is `off`), `native` (no accelerator) or
`unavailable` (not on this backend). The accelerated rows read the
extension report of ADR-0029 §1.1 (the setup's, else a detection with the
configured modes). The same list is `database.capabilities` of
`/api/system/info`. Behind the ops gate like every `/api/system/*` route.

## Consequences

- Fuzzy search and openCypher are no longer PostgreSQL-only; projects can
  move between backends (and between engines on PostgreSQL) without losing
  them.
- SQLite trigram indexes cost disk (one row per distinct trigram per value
  and key) and write time (the trigger walks the value); a value's trigram
  rows are rewritten only when a key value changes.
- `forgebase db convert` still refuses trigram indexes from PostgreSQL to
  SQLite, and its table pairing does not know the SQLite trigram tables
  yet (see Follow-ups).

## Follow-ups (other areas)

- `forgebase-convert`: skip `_forgebase_trgm_<index>` tables like FTS5
  shadow tables, drop the PostgreSQL → SQLite trigram refusal, and refill
  the SQLite trigram tables after the copy (`fuzzy::rebuild_sql`).
- Dashboard: the index kind selector offers `trigram` on every backend;
  `fuzzy_unsupported` / `index_kind_unsupported` texts in `errors.ts` are
  stale; show `engine` on openCypher results and the capability matrix in
  the System studio.
- SDKs (after the integrator regenerates OpenAPI): `CypherResult.engine`,
  `GET /api/system/capabilities`.
- SQLite query interruption for `timeout_ms` needs `sqlite3_interrupt`,
  which sqlx does not expose safely.

## Files

`forgebase-query`: `src/cypher/{mod,ast,lexer,parser}.rs`,
`src/trigram.rs`, `src/trigram_fold.rs`. `forgebase-data`: `src/fuzzy.rs`;
small edits in `indexes.rs`, `list.rs`, `catalog.rs`, `collections.rs`,
`lib.rs`; tests `trigram.rs` (every engine), `fuzzy_parity.rs`.
`forgebase-graph`: `src/portable/{mod,compile}.rs`; edits in `cypher.rs`
(engine selection, shared request checks, `engine`, AGE null cells),
`error.rs`, `lib.rs`, `http.rs`; tests `cypher_portable.rs`,
`cypher_differential.rs`, `common/fixture.rs`, updated `cypher_off.rs`,
`cypher.rs`. `forgebase-db`: `placement.rs` (two per-set tables),
`sqlite_backend.rs` (the writer's statement cache after DDL, found by the
schema-change test). `forgebase-api`: `src/capabilities.rs`, the route,
`database.capabilities`, OpenAPI. Migrations `0056_portable_trigram` (both
backends), registered in `forgebase-migrations`.
