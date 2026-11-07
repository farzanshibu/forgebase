# ADR-0003: Phase 2 data core

- Status: accepted
- Date: 2026-11-07
- Work package: FB-10 (MDF-17)
- Scope: storage abstraction, collection and field model, user DDL and its
  history, filter language, SQL editor backend, authorisation hook, index model.
- Builds on: ADR-0001 (one writer + read pool, crate direction, error model),
  ADR-0002 (dependency policy).

## Context

Phase 2 needs SQL tables, PocketBase-style collections and JSON documents with
first-class index management (SPEC §4.1–4.5, §8), on the embedded SQLite
database of ADR-0001, without locking the code base to SQLite (a PostgreSQL
backend comes later) and without HTTP routes (FB-11 builds REST on the service
API defined here). Correctness first (SPEC §20): every change is transactional
and nothing is reported before it is committed.

## 1. Crates

```text
forgebase-data ──► forgebase-query ──► forgebase-db ──► forgebase-migrations ──► forgebase-core
      │                  │                  ▲                                          ▲
      └──────────────────┴──► forgebase-schema ────────────────────────────────────────┘
```

| Crate | Owns | Why separate |
|---|---|---|
| `forgebase-db` (extended) | `Backend` / `WriteTx` traits, `Dialect`, `Value`/`Statement`/`Rows`, the SQLite implementation, slow-query log | keeps `sqlx` and SQLite types behind one boundary (ADR-0001 §2) |
| `forgebase-schema` (new) | `Collection`, `Field`, `FieldKind`, definition checks, value validation and normalisation | pure model shared by the compiler, the services, FB-11 (OpenAPI types), FB-21 (rules name fields) and SDK codegen; no I/O |
| `forgebase-query` (new) | filter language (lexer, parser, AST), compiler to parameterised SQL, sort keys, raw-SQL tokenizer/classifier | security-critical and pure: unit-tested in isolation, including injection shapes; FB-21 reuses the compiler for rule predicates, FB-40 for subscription filters |
| `forgebase-data` (new) | `DataService`: collections, records/documents, queries, indexes, SQL editor backend, `AccessContext` / `Authorizer` | the stateful service layer FB-11 calls; index management lives here because table rebuilds must recreate indexes inside the same transaction |

No other crate imports `sqlx`. `forgebase-data` talks to the database only
through `Arc<dyn Backend>`.

## 2. Storage abstraction

```rust
pub trait Backend: Send + Sync {
    fn dialect(&self) -> &dyn Dialect;
    fn fetch_all(&self, stmt: &Statement) -> BoxFuture<Result<Rows, DbError>>;   // read pool
    fn begin(&self, mode: TxMode) -> BoxFuture<Result<Box<dyn WriteTx>, DbError>>; // single writer
    fn explain(&self, stmt: &Statement) -> BoxFuture<Result<Vec<PlanStep>, DbError>>;
    fn statement_writes(&self, stmt: &Statement) -> BoxFuture<Result<bool, DbError>>;
    fn slow_queries(&self) -> Vec<SlowQuery>;  // + threshold get/set, clear
}
pub trait WriteTx: Send {
    fn execute(&mut self, stmt) -> BoxFuture<Result<ExecResult, DbError>>;
    fn fetch_all(&mut self, stmt) -> BoxFuture<Result<Rows, DbError>>;
    fn commit(self: Box<Self>) -> BoxFuture<Result<(), DbError>>;
    fn rollback(self: Box<Self>) -> BoxFuture<Result<(), DbError>>;
}
```

- A `Statement` is SQL text plus bound `Value`s (`Null | Integer | Real | Text
  | Blob`), an optional slow-query label and a statement-cache flag. Callers
  render SQL through the backend's `Dialect` (identifier quoting, placeholders
  `?N` / `$N`, NULL-safe equality, JSON extraction and array iteration,
  `LIKE` escaping, column types, literal quoting for index DDL, plan prefix).
- Object-safe async without extra crates: methods return boxed futures, so the
  backend is chosen at runtime (`Arc<dyn Backend>`), embedded or server mode.
- **SQLite implementation** (`impl Backend for Database`): reads run on the
  `query_only` read pool; `begin` takes the single writer and issues
  `BEGIN IMMEDIATE`. A `WriteTx` dropped without commit (error path, cancelled
  request) marks its connection close-on-drop: SQLite rolls the transaction
  back and the pool opens a fresh writer, so a half-done transaction can never
  be reused. `TxMode::SchemaChange` sets `PRAGMA foreign_keys = OFF` before
  `BEGIN` (it is a no-op inside a transaction), runs `PRAGMA foreign_key_check`
  before `COMMIT` (any violation aborts the commit) and restores the pragma.
- Driver errors are classified into `DbError::Constraint {kind}` (unique,
  foreign key, check, not null → HTTP 409), `ReadOnly`, `Statement` (syntax,
  unknown table/column → 400), `Busy`, `Corrupt`, ... (ADR-0001 §7).
- **What is still SQLite-specific** (and where a PostgreSQL backend plugs in):
  catalog introspection (`sqlite_master`, `PRAGMA table_info / index_list /
  index_xinfo`, `sqlite_stat1`, `dbstat`) in `forgebase-data::{ddl, indexes,
  sql}`; `STRICT` tables; FTS5 full-text tables; EXPLAIN opcode inspection in
  `statement_writes`; `legacy_alter_table` during rebuilds. These are isolated
  in a handful of helpers; the PostgreSQL work package moves them behind
  `Dialect`/`Backend` methods (`introspect_*`, `fulltext_*`). Callers of
  `DataService` do not change.

## 3. Collection and field model

- A collection is one table named after the collection (views: one SQL view).
  Types: `base`, `auth` (base + system fields `email` (unique), `email_visibility`,
  `verified`, `password`, `token_key`; logic is FB-20), `view` (read-only,
  `SELECT` with an `id` column; fields inferred from the view's columns), plus
  the `system` flag (created and changed only by `AccessContext::system()`;
  names may then start with `_`).
- Names: `[A-Za-z][A-Za-z0-9_]{0,62}`; `_forgebase*` and `sqlite_*` are always
  reserved; field names may not be `id`, `created`, `updated`, `expand`,
  `rowid`/`oid`, or start with `_`. Names are unique case-insensitively.
- System columns of every table: `id TEXT PRIMARY KEY` (15 random `[a-z0-9]`, or
  a client id of 1–64 `[A-Za-z0-9_-]`), `created`, `updated` (fixed-width UTC
  `YYYY-MM-DDTHH:MM:SS.mmmZ`, so text order is time order), `_version`
  (incremented on every update; optimistic concurrency), `_data` (JSON document
  of a flexible collection). Tables are `STRICT`.

| Field type | Column | Validation |
|---|---|---|
| `text` | TEXT | min/max length (chars, default max 1 000 000) |
| `number` | REAL, or INTEGER with `integer: true` | finite, min/max, integral |
| `bool` | INTEGER `CHECK (x IN (0,1))` | boolean |
| `date` | TEXT | RFC 3339 / `YYYY-MM-DD[ HH:MM:SS]`, normalised to UTC |
| `email`, `url` | TEXT | address syntax; `http`/`https` URL with host |
| `select` (enum) | TEXT, or JSON array if `max_select > 1` | value in list, unique, count |
| `json` | TEXT `CHECK (json_valid)` | size (default 1 MiB) |
| `array` | JSON | item type (text/number/bool/json), min/max items |
| `relation` | TEXT `REFERENCES target(id) ON DELETE CASCADE / RESTRICT (required) / SET NULL`, or JSON array if `max_select > 1` | id syntax; existence checked in the write transaction |
| `file` | TEXT or JSON array | file names only (blobs are Phase 4) |
| `password` | TEXT (hash) | min length; hashed by a `SecretHasher` (FB-20: Argon2id); never returned, never filterable; without a hasher writes fail closed |
| `geo` | JSON `{lat, lon}` | WGS 84 ranges |
| `vector` | JSON array | exact dimensions, finite (stored data only; ANN is Phase 6) |

Common options: `required` (null, `""` and `[]` are blank), `unique` (a unique
index), `default` (validated at definition time, applied on create), `hidden`
(omitted for non-administrators and not filterable by them).

**Documents.** `schema_mode: flexible` keeps undeclared top-level keys in `_data`
(dynamic schema); declared fields are still validated (optional schema
validation). A record is one JSON object: system keys, fields, document keys,
and `expand` when requested. Updates are RFC 7386 merge patches applied to the
whole document, which is then re-validated (`replace: true` for PUT).
`versioned: true` stores every version (and a delete tombstone) in
`_forgebase_record_versions` in the same transaction.

## 4. How user DDL is applied and versioned

Catalog (migration `0002_data_core.sql`): `_forgebase_collections` (definition
JSON, schema `version`), `_forgebase_schema_history` (append-only: action,
DDL statements, before/after definitions, actor, time), `_forgebase_indexes`
(managed index definitions), `_forgebase_record_versions`.

- **Create**: `CREATE TABLE` (or `VIEW`), automatic indexes, catalog row and
  history row in one transaction.
- **Alter** (`update_collection` with the full new definition; fields matched
  by stable field `id`): collection rename → `ALTER TABLE ... RENAME TO`
  (foreign keys and triggers of other tables follow); field renames →
  `RENAME COLUMN` (indexes and triggers follow; managed index definitions are
  rewritten); additions only → `ADD COLUMN`; any dropped field or changed column
  definition → **table rebuild** in a `SchemaChange` transaction: create
  `_forgebase_tmp_<id>` with the new definition, copy rows *with their rowids*
  (full-text indexes stay valid), drop the old table, rename (with
  `legacy_alter_table` so views are not re-checked mid-rebuild), recreate
  automatic, managed and raw-SQL indexes and triggers. Data that does not fit a
  new type fails the copy (`incompatible_data`), duplicates fail a new unique
  index (`duplicate_values`).
- **Drop**: refused while relation fields point to the collection; drops the
  table/view, full-text tables, catalog rows and versions.
- **Raw SQL DDL** (SQL editor) runs in the same transactional way and is
  recorded with action `sql`.
- After every schema change, still inside the transaction, every collection is
  verified (`PRAGMA table_info` columns, views compile). A change that would
  break a collection or a view built on it — including raw `DROP TABLE` or
  `RENAME COLUMN` on a collection table — rolls back (`schema_conflict`).
  (Columns are compared through `table_info`, not by selecting them: SQLite
  reads an unknown double-quoted identifier as a string literal.)
- **Rollback strategy**: every change is one transaction — on any failure the
  table, catalog, indexes and history are exactly as before (tested). System
  migrations keep ADR-0001's forward-only + pre-migration snapshot rule.
  `schema_history` holds the before/after definitions to revert a change
  forward if needed.

## 5. Filter language

PocketBase-style, compiled to parameterised SQL:

```text
filter     := or
or         := and ( "||" and )*
and        := unary ( "&&" unary )*
unary      := "!" unary | "(" or ")" | comparison
comparison := operand op operand
op         := ["?"] ( "=" | "!=" | ">" | ">=" | "<" | "<=" | "~" | "!~" )
operand    := path | string | number | true | false | null | "{:" name "}" | "@now"
path       := ident ( "." ( ident | digits ) )* [ ":length" | ":lower" ]
ident      := [A-Za-z_][A-Za-z0-9_]*
string     := '...' | "..."   (escapes \' \" \\ \n \t)
number     := -?digits[.digits][(e|E)[+-]digits]
```

Semantics: `=` / `!=` are NULL-safe (`x != 'a'` includes NULLs; `= null`,
`!= null` test for NULL); `~` / `!~` are case-insensitive "contains" with `%`,
`_` and `\` escaped (literal match); `?op` = some element of a multi-value
field matches, plain `op` on a multi-value field = every element matches;
`:length` (array length or text length), `:lower`. Paths resolve against the
schema: system columns, fields, JSON paths inside `json`/`geo` fields and
flexible documents (`metadata.specifications.ram`), array elements
(`tags.0`), relations (`author.name`, up to 3 hops, as correlated subqueries;
multi relations through `json_each`). Limits: 4 KiB, 512 tokens, depth 32,
16 path segments.

Safety rules (all tested): values (literals, `{:params}`, `@now`) are always
bound parameters; identifiers come from the schema, never from input, and are
quoted by the dialect; JSON path segments are validated identifiers/integers
rendered as SQL literals (so JSON-path indexes match); there are no comments,
semicolons, subqueries or function calls in the language. Index definitions
are the only place literals are inlined (SQLite cannot bind in `CREATE INDEX`):
numbers are formatted by Rust, strings quoted by the dialect, and parameters,
`@now`, relations and multi-value fields are refused there.

Queries (`ListQuery`): filter, `sort` (`-created,name`, a final `id` tiebreak
is always added), projection (`fields`, nested paths), `expand` (relations, up
to 4 levels, targets filtered by the caller's own access), offset pagination
(`page`, `per_page` ≤ 500, `total_items` via `COUNT(*)`) and keyset cursors
(`next_cursor`: base64url JSON of the last row's sort values and the sort
signature; NULL-aware; rejected for a different sort), full-text `search`
(every word quoted, so FTS5 operators in input are literal), `count_records`,
`aggregate` (`count/sum/avg/min/max` with group-by paths).

## 6. SQL editor backend

`execute_sql` (one statement) and `execute_sql_batch` (one all-or-nothing
transaction). A tokenizer (not a parser) splits statements (strings, comments
and `CREATE TRIGGER ... BEGIN ... END` aware), finds the leading keyword and
rejects transaction control, `ATTACH`/`DETACH`, `VACUUM`, every pragma not on a
read-only allow list or with `=`, `TEMP` objects, unknown virtual-table modules
and `load_extension`/`fts3_tokenizer`. The engine then decides read vs write by
compiling the statement (`EXPLAIN` opcodes: `Transaction` with write flag,
`OpenWrite`, schema ops) — `WITH ... DELETE` is a write. Reads run on the read
pool (which cannot write even if misclassified) and return at most 10 000
rows; writes and DDL run on the writer. `_forgebase_*` tables are readable by
administrators, writable only by the system principal. Raw SQL writes to
collection tables bypass field validation (administrator tool); `STRICT`,
`CHECK` and foreign keys still hold.

## 7. AccessContext and the authorisation hook

Every `DataService` method takes `&AccessContext { principal, tenant,
request_id }` (`Principal::{System, Admin, User, Anonymous}`, non-exhaustive)
and asks `Authorizer::authorize(ctx, action, collection) -> Decision` first:

- `Allow`; `Deny` (→ `Unauthenticated` for anonymous callers, else `Forbidden`);
  `Filter(Predicate)` — a filter-language expression with parameters. The
  predicate is compiled into the *same* SQL statement as the caller's filter
  (`AND`), so rows outside it are never read; get/update/delete of such a row
  is `RecordNotFound`; create/update re-check the written row inside the
  transaction and roll back with `Forbidden` if it leaves the predicate.
- `DefaultAuthorizer` fails closed: only system and administrators.
  FB-21 implements `Authorizer` with its policy engine (RBAC/ABAC, tenant
  predicates) without any signature change; `Action` and `Decision` are
  `#[non_exhaustive]` so it can add field-level rules.
- Hidden fields: returned and filterable only for privileged callers;
  predicates may use them. Passwords: never returned or filterable.

## 8. Index model

- Native engine indexes only (SPEC §4.5): B-tree single, composite (with
  per-key direction; covering when all selected columns are keys), unique,
  partial (`partial` filter expression), expression (`name:lower`), JSON-path
  (`metadata.brand` → `json_extract(_data, '$.metadata.brand')`), and FTS5
  full-text (contentless table `_forgebase_fts_<name>` with
  `contentless_delete=1`, kept in sync by insert/update/delete triggers,
  `unicode61 remove_diacritics 2`).
- Definitions are field paths in filter syntax and are compiled by the same
  compiler as queries, so planner expression matching works.
- Automatic indexes from the schema: one unique index per `unique` field, one
  index per single relation (`auto_<collection id>_<field id>_{u,r}`).
- Managed definitions live in `_forgebase_indexes` (origin `auto`/`manual`);
  raw-SQL indexes are reported with origin `sql`, engine ones with `engine`.
- Operations: create (unique over duplicates fails cleanly), drop (manual only),
  list/inspect (columns, unique, partial, DDL, `sqlite_stat1` statistics,
  size from `dbstat`), rebuild (`REINDEX` + `ANALYZE`, or full-text
  re-population), `analyze`, `explain_list` / `explain_sql` (`EXPLAIN QUERY
  PLAN`, with `indexes_used` and `full_scans`; `SCAN x USING INDEX` counts as a
  full scan).
- Slow-query log: bounded in-memory ring (256 entries) in the backend; every
  statement slower than the threshold (default 200 ms, adjustable at runtime,
  `0` logs all) is recorded with its SQL (never its parameter values), duration,
  rows and a label (collection, equality/range/sort keys) and logged at WARN.
- Recommendations: for labelled slow queries whose current plan scans the
  collection, suggest an index of the equality keys + first range key (or the
  sort keys), skipping keys already covered by an existing index prefix.

## 9. Concurrency

ADR-0001's single writer serialises every write transaction in-process;
schema changes additionally take a service-level lock so DDL and catalog
reloads cannot interleave. Readers use WAL snapshots and never block the
writer. A list page and its total count are two read statements (two
snapshots): under concurrent writes the count may differ from the page by the
writes in between. Each statement on its own is consistent (tested: readers
never see part of a bulk-insert transaction, counts never go backwards).

## Consequences

- One write path keeps integrity simple; write throughput is bounded by one
  connection (measured in FB-81).
- Raw SQL is powerful by design (administrators only by default) but cannot
  corrupt the catalog: system tables are protected and collection-breaking DDL
  rolls back.
- Projection is applied after fetching full rows; total counts cost a second
  query (`skip_total` avoids it).
- Multi-value relation cleanup on delete runs in the service (cascade or
  removal from arrays); single relations rely on engine foreign keys, so rows
  deleted by an engine cascade get no version tombstone.
- Follow-ups: FB-11 maps `DataError` codes to HTTP and exposes these types in
  OpenAPI; FB-21 implements `Authorizer`; FB-20 provides `SecretHasher`; the
  PostgreSQL backend moves the introspection helpers listed in §2 behind the
  traits.
