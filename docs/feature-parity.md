# Feature parity across backends

ForgeBase runs on PostgreSQL (bundled or your own server) and on SQLite.
PostgreSQL extensions are **accelerators**: where one is installed and
allowed, the feature runs on it; everywhere else a **portable engine**
answers the same API ([ADR-0029](adr/0029-postgres-extensions-replicas-sharding.md)
decision 1, [ADR-0034](adr/0034-portable-fuzzy-and-cypher.md)). This page
lists, per feature, what runs where and what differs.

A running server says what it uses: `GET /api/system/capabilities` (also
`database.capabilities` of `GET /api/system/info`) lists every feature with
its `engine`, `accelerated`, `state` (`active`, `portable`, `off`,
`native`, `unavailable`), configured `mode`, `reason` and `limits`. The
modes are set in `[database.postgres.extensions]` (`auto`, `on`, `off`;
[deployment guide](deployment.md)).

## Matrix

| Feature | PostgreSQL with the extension | PostgreSQL without it (`off`, or not installed) | SQLite | Same API everywhere |
|---|---|---|---|---|
| Vector search | pgvector (`vector`) | portable: canonical vectors in the database, USearch HNSW index files | portable (same) | yes |
| Graph REST (nodes, edges, neighbours, traversal, shortest path) | — (native) | recursive CTEs over the graph tables | same | yes |
| openCypher (`POST /graph/cypher`) | Apache AGE (`graph`): derived per-tenant graph, full read-only openCypher | portable: a read-only subset compiled to SQL | portable (same) | yes; `engine` in the response says which |
| Full-text search (`search`) | — (native) | `tsvector` + GIN, `plainto_tsquery` | FTS5 (`unicode61`) | yes |
| Fuzzy search (`fuzzy`, `trigram` indexes) | pg_trgm (`trigram`) | portable: GIN over `forgebase._forgebase_trgm` | portable: trigram tables kept by triggers | yes |
| Queues (subscription deliveries) | pgmq (`queue`) | portable delivery tables | portable (same) | yes |
| Scheduled jobs | pg_cron (`cron`) for expressions it can express | the server's scheduler tick | same | yes |
| Key-value (Redis protocol) | — | in memory, written behind to the database | same | yes |
| Realtime | — | change outbox in each write's transaction | same | yes |
| Read replicas | — | streaming replicas, managed or external | not available | PostgreSQL only |
| Tenant sharding | — | control database plus shard sets | not available | PostgreSQL only |

Apache AGE has no Windows build: Windows engines always run openCypher on
the portable engine. Hosted PostgreSQL services often lack AGE and pgmq;
`auto` then falls back to the portable engines and the capability matrix
says why (`reason`).

## Fuzzy search: tolerance of the portable engines

All engines use `pg_trgm`'s model: lowercase, words padded with two spaces
in front and one behind, trigram sets, similarity = shared / union,
threshold 0.3 by default, best match first, then by id.

| | pg_trgm | PostgreSQL portable | SQLite portable |
|---|---|---|---|
| Character classes and case | the server's locale | the server's locale (same functions as `pg_trgm`) | ASCII exactly; Latin, Greek, Cyrillic, Armenian, Hebrew, general punctuation, currency, CJK punctuation and full-width forms as `pg_trgm` under `C.UTF-8`; other characters are letters, compared as they are |
| Value length | unlimited | unlimited | first 4096 characters |
| Score precision | `real` | `real` | double |
| Index | GIN `gin_trgm_ops` (also serves `~`) | GIN over a `text[]` of trigrams | trigram table `_forgebase_trgm_<index>` |
| `~` (contains) filter | index-assisted | not index-assisted | not index-assisted |
| Threshold below 0.3 | not index-assisted | index-assisted (above 0) | index-assisted (above 0) |

An index is built by the engine in use when it is created. After switching
`trigram` between pg_trgm and portable, answers stay the same; re-create
the index (drop and create) to make it fast for the new engine.

## openCypher: the portable subset

The portable engine accepts:

- `MATCH` (one or more), comma-separated patterns, `WHERE`;
- node patterns with variable, labels and property maps; relationship
  patterns with variable, types (`:A|B`), direction (`->`, `<-`, `-`;
  `-->`, `<--`, `--`), property maps and bounded variable-length paths
  (`*2`, `*1..3`, `*..5`, at most 10 hops);
- `RETURN` with aliases, `DISTINCT`, `ORDER BY … ASC|DESC`, `SKIP`, `LIMIT`;
- expressions: literals, lists, `$parameters`, property access,
  comparisons, `AND`/`OR`/`NOT`, `IN`, `IS [NOT] NULL`, `STARTS WITH`,
  `ENDS WITH`, `CONTAINS`, unary minus;
- functions: `count` (`count(*)`, `DISTINCT`), `collect`, `min`, `max`,
  `sum`, `avg`, `type`, `labels`, `size`, `coalesce`, `toLower`, `toUpper`.

Anything else answers `400 cypher_unsupported` with the construct and its
line and column (for example `WITH`, `OPTIONAL MATCH`, `UNWIND`, `UNION`,
`CASE`, arithmetic, path variables, `shortestPath`, unbounded `*`). Writes
answer `400 cypher_read_only` on both engines.

Semantics follow the AGE engine: `->` follows an edge's stored direction
(an undirected edge is stored once; use `-[]-` to walk it both ways);
`n.__id`, `r.__id`, `r.__source`, `r.__target` read ForgeBase ids; a
variable-length relationship returns one row per path and never repeats an
edge within its path. Results have the same JSON shapes on both engines.
The differential test runs the subset on both engines, with and without
label rules, and compares the rows.

| | Apache AGE | Portable |
|---|---|---|
| Language | read-only openCypher (AGE 1.8) | the subset above |
| Data | derived graph, built on first use (`409 graph_materialising`), kept in step by every write | the canonical tables: every write is visible at once, nothing to build |
| Rules | row-level security inside the query | compiled into the SQL, as in the REST API |
| Limits | 1000 rows, 30 s, 64 KiB | the same, plus 10 hops, 32 patterns and at most 100 000 paths per variable-length relationship (`cypher_too_complex`) |
| Timeout | `statement_timeout` | PostgreSQL: `statement_timeout`; SQLite: the request stops waiting |
| Type alternatives `:A\|B` | not supported by AGE 1.8 | supported |

## Other differences between the backends

These are decided in their own ADRs and listed here for completeness:

- Full-text matching differs in tokenisation (FTS5 `unicode61` versus
  `to_tsvector('simple', unaccent(…))`), not in API ([ADR-0029 §1.8](adr/0029-postgres-extensions-replicas-sharding.md)).
- Read replicas, sharding, `forgebase db psql` and the SQL editor's
  PostgreSQL-only statements need PostgreSQL ([ADR-0028](adr/0028-bundled-postgres.md),
  [ADR-0029](adr/0029-postgres-extensions-replicas-sharding.md)).
- `forgebase db convert` moves a project between backends; it does not yet
  carry SQLite trigram indexes to PostgreSQL or back (re-create them after
  converting).
