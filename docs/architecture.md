# ForgeBase architecture

This page explains how ForgeBase is built: the crates, how a request moves
through the server, and where each feature lives. The design decisions behind
it are in the [ADRs](adr/). This page does not repeat them; it links to them.

- Product scope: [SPEC.md](SPEC.md)
- Work packages and status: [PLAN.md](PLAN.md)
- HTTP reference: [api.md](api.md)
- Schema and storage: [database.md](database.md)

## 1. One binary, one database file

ForgeBase ships as one executable, `forgebase`, with the admin dashboard
compiled in. The default storage is one SQLite file in WAL mode, inside one
data directory. One server process owns one data directory (ADR-0001 §3).
Object blobs live beside the database in `storage/` (ADR-0018), and derived
indexes live in `indexes/` (ADR-0012).

Database access is isolated: only `forgebase-db` imports SQLite
(ADR-0001 §2). A single writer serialises every write. Readers use a separate
pool and cannot write (ADR-0001 §3).

## 2. Crates

The workspace is `crates/`. The table lists the main job of each crate. The
authoritative description of each crate is its `lib.rs` module docs.

| Layer | Crate | Job |
|---|---|---|
| Foundations | `forgebase-core` | Config model and precedence, error codes, data-directory layout. Depends on no other ForgeBase crate. |
| | `forgebase-observability` | `tracing` subscriber, Prometheus recorder, W3C trace context and OTLP export (ADR-0035). |
| Storage | `forgebase-migrations` | Embedded system migrations, SHA-256 checksums, atomic apply. |
| | `forgebase-db` | SQLite pools, pragmas, WAL, integrity and stats, pre-migration snapshots. |
| Data model | `forgebase-schema` | Collection and field model; validates and normalises values. No I/O. |
| | `forgebase-query` | Filter language and its compiler to parameterised SQL. |
| | `forgebase-data` | Data service: collections, records, versions, indexes. Owns the `Authorizer` hook. |
| Identity and access | `forgebase-auth` | Passwords, sessions, tokens, MFA, OAuth and OIDC clients, WebAuthn, API keys, service accounts, identity providers. |
| | `forgebase-policy` | Realms, orgs, projects, environments (tenants), roles, role sets, overrides, resource ACLs, suspensions, and the evaluation pipeline. |
| | `forgebase-entitlements` | Plans, features, quotas, metering, plan-based rate limits. Entitlement is not quota (ADR-0010). |
| Engines | `forgebase-kv` | Built-in key-value engine: strings, hashes, lists, sets, sorted sets, streams, TTL, namespaces. |
| | `forgebase-resp` | Redis protocol listener (RESP2/RESP3) over the KV engine. Off by default (ADR-0013). |
| | `forgebase-graph` | Nodes and typed edges on SQLite. |
| | `forgebase-vector` | Vector collections, items, metadata; similarity search. |
| | `forgebase-storage` | Buckets, objects, versions, uploads, signed URLs; local and S3-compatible drivers. |
| | `forgebase-functions` | WebAssembly server functions on Wasmtime, with capability-scoped host calls (ADR-0017). |
| Background work | `forgebase-jobs` | Durable jobs, schedules, leases, retries, dead-letter queue, queues, workflows (ADR-0007, ADR-0023). |
| | `forgebase-realtime` | Transactional outbox, WebSocket and SSE delivery, channels, presence (ADR-0011). |
| Operations | `forgebase-alerts` | Alert rules over metrics and health signals, alert history, silences, notifiers, SLOs with error budgets and burn-rate alerts (ADR-0035). |
| Money | `forgebase-credits` | Append-only credit ledger per subject; top-ups, spend, expiry (ADR-0016). |
| | `forgebase-billing` | Catalog, subscriptions, invoices, payments, provider webhooks. On by default (ADR-0021); `[billing] enabled = false` turns it off. |
| Product | `forgebase-customize` | Custom domains, branding, transactional email, legal agreements (ADR-0020). |
| Surface | `forgebase-api` | Axum router, error envelope, request ids, middleware, OpenAPI document, embedded dashboard. |
| | `forgebase-server` | Startup sequence, data-directory lock, listeners, graceful shutdown. |
| | `forgebase-cli` | The `forgebase` binary. |

Dependencies point one way toward `forgebase-core`. A crate uses another
crate only through its public API (ADR-0001 §1).

## 3. Process start

`forgebase serve` runs the startup sequence in `forgebase-server`:

1. Load config: flags, then `FORGEBASE_*` environment, then the TOML file,
   then defaults (see [deployment.md](deployment.md#3-configuration)).
2. Create the data-directory layout.
3. Take the data-directory lock (`.forgebase.lock`). A second server on the
   same directory refuses to start.
4. Open the database, check the migration checksums, write a snapshot to
   `backups/` if migrations are pending, and apply them.
5. Start the services: auth, policy, entitlements, data, KV, jobs, credits,
   and each switchable module that is on (storage, functions, queues,
   realtime, vector, billing; see §6).
6. Bind the HTTP listener, and the RESP listener if enabled.

On SIGTERM or Ctrl-C the server stops accepting work, drains in-flight
requests for `shutdown_timeout_secs`, then runs `wal_checkpoint(TRUNCATE)` so the
data directory is one self-contained database file (ADR-0001 §3).

## 4. How a request flows

The HTTP middleware runs outermost first, as documented in
`crates/forgebase-api/src/lib.rs`:

1. **Request context**: request id (`x-request-id`), error envelope, security
   headers.
   **Feature gate**: a route of a switched-off module answers
   `404 feature_disabled` here, before authentication (§6).
2. **Tracing span** and **metrics** for the route.
3. **Panic catcher**. A panic returns a 500 envelope, not a dropped connection.
4. **Timeout**: `request_timeout_secs`.
5. **Authentication**: the credential (bearer token, API key, PAT, or session
   cookie) becomes an `AccessContext`. Anonymous requests pass through; the
   authorizer decides (ADR-0004 §3, §10).
6. **Tenancy**: realm, organization, tenant (environment), and roles, from the
   `X-Forgebase-Realm`, `X-Forgebase-Org` and `X-Forgebase-Tenant` headers
   (ADR-0008 §10).
7. **Gates**: plan-based API rate limits and entitlements (ADR-0010).
8. **Handler**: the route in `forgebase-api`, which calls a service.

Inside a service, every protected operation runs the same authorization
pipeline, in order. The first failure denies. The denial is written to
`_forgebase_authz_audit` with the step that failed (ADR-0008 §8):

| Step | Decides |
|---|---|
| 1. Authentication | Caller present where required; account not suspended |
| 2. Tenant | Realm, organization, tenant ownership, active membership |
| 3. Permission | Effective permissions; deny wins; API-key scopes |
| 4. Entitlement | Plan includes the feature |
| 5. Resource | Resource ACL, collection rules, record-level ACL |
| 6. Quota | Usage left in the plan |

Platform super-admins skip the pipeline.

A write then runs as one transaction on the single writer. The change, its
audit or outbox row, and any usage metering commit together:

- **Data writes** go through `forgebase-data` and `forgebase-schema`. Each
  record change writes an outbox row in the same transaction
  (ADR-0011 §2), so an event exists exactly when its change committed.
- **Realtime**: the dispatcher reads the outbox and sends events to
  subscribers over WebSocket or SSE, with replay from the outbox cursor.
- **Jobs**: a job is a row. Workers claim it with a lease. A lost lease is
  re-queued, so the handler must be idempotent (ADR-0007 §3).
- **Functions**: a function runs with only the host capabilities its version was
  granted. It has no filesystem, clock, or network access, except the
  SSRF-checked fetch (ADR-0017 §§2, 8).
- **Billing webhooks** check the provider signature, then record the event
  and its state change in one transaction (ADR-0021 §6).

## 5. Feature map

| Feature | Where to read | Surface |
|---|---|---|
| Auth, sessions, MFA, OAuth, OIDC, WebAuthn | ADR-0004, ADR-0009, ADR-0015 | `/api/auth/*` |
| Identity provider for your apps | ADR-0019 | `/api/auth/oidc/{realm}/*` |
| Realms, orgs, roles, ACLs | ADR-0008 | `/api/v1/orgs`, `/api/v1/roles`, `/api/v1/acl`, `/api/v1/authz/*` |
| Collections and records | ADR-0003 | `/api/v1/collections/*` |
| Key-value and Redis protocol | ADR-0005, ADR-0013 | `/api/v1/kv/*`; RESP port |
| Graph | ADR-0006 | `/api/v1/graph` |
| Vector search | ADR-0012 | `/api/v1/vector/*` |
| Object storage | ADR-0018 | `/api/v1/storage/*` |
| Functions | ADR-0017 | `/api/v1/functions/*` |
| Jobs and long-running tasks | ADR-0007, ADR-0023 | `/api/v1/jobs/*` |
| Realtime | ADR-0011 | `/api/v1/realtime/*` |
| Entitlements and quotas | ADR-0010 | `/api/v1/entitlements`, `/api/v1/plans`, `/api/v1/features`, `/api/v1/quota-policies` |
| Credits | ADR-0016 | `/api/v1/credits/*` |
| Billing | ADR-0021 | `/api/v1/billing/*` (unless `[billing] enabled = false`) |
| Product customization | ADR-0020 | `/api/v1/customize/*` |
| Tracing, alerts and SLOs | ADR-0035, [observability.md](observability.md) | `/api/v1/alerts/*`, `/api/v1/slos/*` |
| Dashboard | ADR-0014 | `/admin/` |

The exact paths are in the OpenAPI document served by the running server,
at `GET /api/openapi.json`. See [api.md](api.md#10-openapi).

## 6. Feature flags

Every module is in one registry, `forgebase_core::features::FEATURES`
(ADR-0026): id, name, whether it can be switched off, the HTTP routes it owns
and its typed settings. `GET /api/system/features` (administrators) returns it
with each setting's value, default and source (`default`, `file`, `env`,
`flag`); `PATCH /api/system/config` writes settings to the config file.

Switchable modules, each `[<section>] enabled` (default `true`) and
`FORGEBASE_<SECTION>_ENABLED`:

| Feature id | Switch | When off |
|---|---|---|
| `storage` | `storage.enabled` | Not opened; no reconcile job; its schedule paused. |
| `functions` | `functions.enabled` | Not opened: no record hooks, no after-hook dispatcher, no function jobs. |
| `queues` | `queue.enabled` | Not opened; no maintenance or push task. |
| `realtime` | `realtime.enabled` | No routes, no dispatcher. The record-change outbox is still written (functions and queues read it). |
| `vector` | `vector.enabled` | Not opened; no index warm-up or saver. |
| `graph` | `graph.enabled` | Not mounted. |
| `identity` | `identity.enabled` | OIDC provider and realm management off; existing realms still sign in. |
| `access` | `access.enabled` | Policy, role, org, team, project, ACL and suspension APIs off; stored rules still enforced. |
| `resp` | `resp.enabled` (default `false`) | Listener not bound. |
| `billing` | `billing.enabled` | Not opened; maintenance schedule paused. |

A disabled module's routes answer
`404 {"error":{"code":"feature_disabled","message":"<Name> is disabled on this server"}}`
and leave the OpenAPI document. `server`, `database`, `auth`, `kv`, `jobs`,
`entitlements`, `credits`, `customize` and `backups` are always on; ADR-0026 §2
says why for each.

Changes written through the API take effect after a restart
(`restart_required: true`, `restart_pending` until then). Precedence is
unchanged: a setting from a flag or the environment cannot be changed in the
file (`409 setting_overridden`), and secrets are never listed or written.

To add a module: give it a `[<section>] enabled` switch in the config model,
an entry with its route patterns in `FEATURES`, and skip opening it in
`forgebase-server` when it is off.

## 7. Where to change things

- A new REST operation: the handler in `forgebase-api` calls a service method.
  The service method enforces the pipeline. Add the path to the OpenAPI
  document through `utoipa` annotations.
- A new table: a new migration under `migrations/system/`. See
  [database.md](database.md#4-system-migrations).
- A new configuration key: `forgebase-core` config model, the feature
  registry (`features.rs`; a test fails otherwise), the example file, and
  [deployment.md](deployment.md#3-configuration).
- A new decision: a new ADR under `docs/adr/`. ADRs are not edited after
  acceptance except to add addenda.
