# ADR-0012: Vector search

- Status: accepted
- Date: 2026-11-09
- Work package: FB-50 (MDF-30). Migration `0012_vector.sql`. Crate `forgebase-vector`.
- Scope: canonical vector storage, the derived ANN index and its rebuilds, kNN with metadata filters, hybrid search, REST under `/api/v1/vector`.
- Builds on: ADR-0001 (canonical vs derived data, one writer, error model), ADR-0003 (filter language, `AccessContext` / `Authorizer`), ADR-0007 (`ApiExtension` mounting).

## Context

SPEC §6 asks for an integrated vector engine: collections with dimensions and metadata, cosine / Euclidean / dot product, top-k ANN and exact search, filtering, persistence, rebuilds, collection isolation and hybrid (keyword + vector) search, with "canonical vectors and metadata durable, ANN indexes recoverable and rebuildable" (also SPEC §4.5). SPEC §27 #12, #13 and #23 are the acceptance scenarios.

## 1. Data model: vector collections, separate from record collections

A vector collection belongs to one tenant and has 0 to 16 **vector fields** (`dimension` 1-4096, `metric` `cosine`/`l2`/`dot`, index element type `f32`/`f16`/`i8`, HNSW `connectivity`, `expansion_add`, `expansion_search`). An **item** has an id, one optional vector per field, a JSON metadata object and an optional text for keyword search.

Four tables (migration 0012): `_forgebase_vector_collections (id, tenant, name, …, change_seq)`, `_forgebase_vector_fields`, `_forgebase_vector_items (key, collection_id, id, _data, content, _version, created, updated)` and `_forgebase_vector_values (collection_id, field, key, vector BLOB)`, plus the FTS5 table `_forgebase_vector_fts` kept in sync by triggers.

- Vectors are stored as little-endian float32 BLOBs (4 bytes per component), validated on write: exact dimension, finite as f32, not all-zero for cosine. The vector stored is the vector returned.
- `key` (`INTEGER PRIMARY KEY AUTOINCREMENT`) is the ANN key. AUTOINCREMENT means a key is never reused, so an index entry can never point at another item than the one it was built from.
- Items use the data core's column names (`id`, `created`, `updated`, `_version`, `_data`), so the filter language compiles against the items table unchanged (§4).
- Composite foreign keys keep every vector inside its item's collection and its field's collection.

**Why not ANN over the data core's `vector` field type?** ADR-0003 already stores `vector` fields on records (JSON arrays, "ANN is Phase 6"). Indexing those would need change notifications from the data core (every record write, table rebuild and raw SQL statement would have to reach the index), which do not exist until the realtime outbox (FB-40) and would mean editing `forgebase-data`. A self-contained store gives one write path, so the index can be kept exactly in step with the canonical data. Feeding record `vector` fields into a vector collection from outbox events is a follow-up.

## 2. Tenancy and authorisation

- Every collection row carries the tenant (`AccessContext::tenant`, `''` when none). The service resolves `(tenant, name)` to the collection's integer id and every item, vector, FTS and index-file access goes through that id; no id is ever taken from input. Another tenant's collection of the same name is a different collection with its own index, so an ANN search cannot even see another tenant's vectors (stronger than a post-filter).
- New `Action` variants: `VectorRead` (list, get, search, hybrid, stats), `VectorWrite` (upsert, delete items), `VectorAdmin` (collections, fields, rebuild, recall). Each call asks `DataService::authorize_action` first; the HTTP layer asks before reading the body. A `Decision::Filter` is treated as a denial (no row-level vector policies yet). The default authorizer allows the system and administrators; anonymous gets 401. FB-21's engine plugs in behind the same trait.

## 3. The derived ANN index

- **Engine**: USearch 2.26 (HNSW, C++, Apache-2.0) behind two traits, `AnnEngine` (create, load) and `AnnIndex` (upsert, remove, search with an optional key filter, save, reserve, stats), so another engine can replace it without touching storage, strategies or the API. One index per (collection, field). Built without the optional `numkong` SIMD library (no further native code).
- **Files**: `<data_dir>/indexes/vector/<collection id>/<field>.usearch` + `<field>.manifest.json` (format, engine and version, build parameters, vector count, the `change_seq` it reflects, file size and SHA-256). Written to a temporary name and renamed, manifest last. A file is loaded only if format, engine, engine major.minor, parameters, sequence, count, size and checksum all match; the checksum is verified before the engine parses the file. Otherwise it is rebuilt, with the reason recorded (`missing`, `corrupt`, `stale`, `version_mismatch`, `params_changed`).
- **No fsync on index files.** On macOS `sync_all` is a full device flush (measured 3.9 s for a 200-vector index on the dev SSD). Derived data needs detection, not durability: every torn or out-of-order state a crash can leave fails the size/checksum/sequence check and is rebuilt from SQLite, which is durable (ADR-0001 `synchronous = FULL`).
- **Consistency**: a write takes the collection's write lock, commits to SQLite (bumping `change_seq` in the same transaction), then applies the change to the in-memory index. Saves, loads and rebuild hand-overs take the same lock, so a saved file reflects exactly the sequence it records. If applying fails, the index is dropped (searches become exact) and rebuilt.
- **When files are written**: at shutdown (the server calls `VectorService::shutdown` before closing the database), after each rebuild, and every `save_interval` (60 s) for changed indexes. After a crash the file is stale (sequence behind) and is rebuilt on next use; a clean restart loads it.
- **Background rebuild with progress**: registered under the write lock (writes from then on are recorded as pending keys), then the canonical vectors are read in key order in batches without blocking writes; progress (`done`/`total`) is in the stats. At the end, under the lock, the pending keys are re-read from SQLite and applied, and the new index replaces the old one. A manual rebuild keeps the old index serving meanwhile; a rebuild after a bad file serves exact results meanwhile. A newer rebuild, a dropped field or collection, or shutdown supersedes a running one.
- **Loading**: lazily on first use of a collection, and by a warm-up task started with the server.

## 4. Search

`POST …/search {field, vector, k ≤ 1000, filter, params, filter_mode, exact}`; results carry `distance` (cosine `1 − cos`, Euclidean distance, `1 − a·b` for dot) and `score` (cosine similarity, `1/(1+d)`, dot product), plus the `strategy` used.

Filters are the data core's filter language (ADR-0003 §5) over the metadata (`category = 'news' && year >= 2020`, `{:params}`, `id`, `created`, `updated`). The items table is described to the compiler as a flexible collection with no declared fields, so paths compile to `json_extract(_data, …)`; paths starting with `_` are refused, so a filter cannot name `collection_id` or `key`; every value is a bound parameter. The compiled condition is ANDed in parentheses with `collection_id = ?`.

| Situation | Strategy |
|---|---|
| `exact: true` | brute force over the (filtered) vectors, up to `max_exact_scan` (200 000) |
| no serving index (rebuilding after a bad file) | `exact_fallback` up to `max_exact_scan`, else `503 vector_index_not_ready` |
| no filter, at most `exact_threshold` (default 1000, per collection) vectors | `exact` |
| no filter | `ann` |
| filter, `pre`/`auto`: matching keys collected first; ≤ `exact_threshold` | `exact` over the matches |
| filter, more matches | `filtered_ann` (HNSW walk that only accepts matching keys); if it returns fewer than `k` while more exist, exact over the matches |
| filter, `post`, or more than `max_prefilter` (500 000) matches | `post_filter`: ANN with 4×k (≥ 64) candidates, filter checked in SQL, widened ×4 until `k` match or the index is exhausted |

Recall check: `POST …/recall {samples, k}` uses stored vectors as queries and compares ANN with exact top-k (ties at the k-th distance count as correct); the last result is in the stats. Measured in the acceptance test: recall@10 ≥ 0.95 on 10 000 random 32-dimensional cosine vectors with the defaults (M = 16, ef_add = ef_search = 128).

## 5. Hybrid search

`POST …/hybrid {vector?, text?, k, candidates, rrf_k (default 60), weights {vector, text} (default 1, 1), text_operator or|and, filter}`. The keyword ranking is FTS5 BM25 over item texts (`unicode61 remove_diacritics 2`; every word is quoted, so FTS5 operators in input are literal), restricted to the collection and the filter; the vector ranking is §4 with `candidates` results (default `max(5k, 50)`, ≤ 1000). Fusion: `score = Σ weight / (rrf_k + rank)` over the rankings an item appears in; ties by key. Either side may be omitted (keyword-only or vector-only).

## 6. API

Under `/api/v1/vector` (an `ApiExtension`, OpenAPI fragment merged): `GET /stats`; `GET|POST /collections`; `GET|DELETE /collections/{name}`; `GET|POST /collections/{name}/fields`, `DELETE …/fields/{field}`; `POST …/upsert` (1-1000 items, one transaction, replace semantics), `POST …/delete`; `GET|DELETE …/items/{id}`; `POST …/search`, `POST …/hybrid`; `POST …/rebuild` (202); `GET …/stats` (state, progress, counts, memory, last rebuild/save/recall, search counters and mean latency); `POST …/recall`. Body limit 16 MiB (1000 items of 384 dimensions fit).

Metrics: `forgebase_vector_searches_total{strategy}`, `forgebase_vector_search_duration_seconds`, `forgebase_vector_upserts_total`, `forgebase_vector_deletes_total`, `forgebase_vector_rebuilds_total{reason}`, `forgebase_vector_rebuilds_running`.

## Limits and follow-ups

- No `[vector]` keys in `forgebase.toml` yet: the server runs on `VectorConfig::default()`.
- Canonical storage is float32 only; `f16`/`i8` apply to the index. Binary vectors (Hamming) are not supported.
- One writer per collection (the write lock) on top of SQLite's single writer. Saves hold the lock (fast without fsync).
- Recovery after a crash is a full rebuild of the touched collections (no incremental catch-up log).
- The FTS table is shared by all collections; a keyword query scans matches of every collection before the join restricts them (correct, slower with many tenants).
- `expansion_search` cannot be changed after creation (no field PATCH yet).
- Row-level policies on vector metadata (`Decision::Filter`) are denied, not applied.
- No embeddings generation (out of scope); the dashboard Vector Studio is FB-52.
