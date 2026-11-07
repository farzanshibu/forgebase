# ForgeBase — Implementation Plan

Spec: `docs/SPEC.md` (the master prompt, verbatim — the contract). This file is the execution plan.
Owner of the plan, merges and phase gates: **god (Michael)**, the hive orchestrator.
Work is done by short-lived temp agents, each on its own git worktree/branch.

## 1. Model tiers

| Tier | Model | Use for |
|---|---|---|
| High | `claude-opus-5-5` | Architecture, storage/consistency, security, auth/authz, realtime outbox, vector durability, WASM sandbox, billing webhooks, audits, phase QA |
| Normal | `claude-sonnet-5-5` | Feature crates on an agreed design, REST endpoints, dashboard studios, SDKs, CLI commands, benchmarks |
| Low | `claude-haiku-5-5` | Docs, README, example config, Docker/compose/systemd, CI workflow, release scripts |

## 2. Rules every work package follows

1. Read `docs/SPEC.md` §28 (development rules) and every ADR in `docs/adr/` before writing code.
2. Stay inside the paths your package owns (listed in its card). Need a change elsewhere? Note it in your report; god routes it.
3. Green before done: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release`. Dashboard: `npm run build` and `npm run lint` in `dashboard/`.
4. Done = the acceptance tests listed for the package exist, run, and pass against real persistence. HTTP 200 alone is not done.
5. Verify crate versions, licenses (MIT/Apache-2.0/BSD/ISC/Zlib/MPL-2.0 OK; ask before anything else) and advisories before adding a dependency; record new deps in the report.
6. Commit on your branch with clear messages. Never push, never rewrite `main`. god merges.
7. Report: what was built, test command + pass counts, files/crates touched, new deps, known limitations, follow-ups.

## 3. Work packages

Phase gate: god merges every package of a phase, re-runs the full test suite and the phase acceptance tests, and only then dispatches the next phase. Test numbers refer to SPEC §27.

| WP | Phase | Tier | Title | Deps | Acceptance |
|---|---|---|---|---|---|
| FB-01 | 1 | High | ADR-0001 + Cargo workspace + core/config/db/server/cli crates, SQLite WAL + migrations, health/ready/metrics, embedded dashboard serving, single binary | — | init→serve→health; restart keeps data (#21 base) |
| FB-02 | 1 | Normal | Admin dashboard shell: Vite+React+TS+Tailwind, studio navigation, live System page | — | `npm run build` → `dashboard/dist`, served by binary at `/admin/` |
| FB-03 | 1 | Low | README, `forgebase.example.toml`, Dockerfile, docker-compose, systemd unit, CI workflow, cargo-deny config | FB-01, FB-02 | `docker build` works; CI file runs the §2 checks |
| FB-10 | 2 | High | Storage abstraction (Postgres-ready trait), collection schema model + field types, record/document engine, safe filter language → parameterised SQL, index manager (create/inspect/rebuild/EXPLAIN), slow-query log | FB-01 | #2 #3 #4 #5 #6 #24 |
| FB-11 | 2 | Normal | REST: collections, records, documents, SQL endpoint, pagination/sort/filter/projection, import/export, OpenAPI (utoipa) | FB-10 | API tests for #2–#6 |
| FB-12 | 2 | Normal | Database Studio: collection browser, table editor, JSON editor, schema designer, indexes, SQL editor, explain | FB-11 | build + e2e smoke |
| FB-20 | 3 | High | Auth core: Argon2id, sessions, rotating refresh tokens, JWT, verify/reset/magic link/OTP (pluggable mailer), TOTP + recovery codes, API keys, service accounts, PATs, rate limit, lockout, audit log | FB-10 | #7 #8 |
| FB-21 | 3 | High | Policy engine: rule language compiled to SQL predicates, fail-closed, RBAC/ABAC, field-level, tenant isolation; orgs/projects/environments/teams/invitations. **Addendum (docs/requirements/authz-entitlements-billing.md §C):** 10 default roles, `resource:action` + `org:<feature>:<permission>` permission keys, custom roles, role sets, team hierarchy, inheritance, explicit deny wins, resource ACL, platform super-admin (not an org role), suspension, cache invalidation, 6-step evaluation pipeline with hook points for steps 4 and 6 | FB-20 | #1 #9 #25 + pipeline order and deny-precedence tests |
| FB-22 | 3 | Normal | OAuth2/OIDC (Google, GitHub, Apple, generic) + WebAuthn/passkeys | FB-20 | provider flow tests with mock IdP |
| FB-23 | 3 | Normal | Admin login + Auth Studio (users, sessions, roles, policies editor, API keys, audit). **Addendum:** permission matrix editor, custom roles, role sets, team hierarchy, resource ACL, user overrides, suspension, controlled and audited admin impersonation | FB-21 | admin routes reject anonymous; impersonation audited and super-admin only |
| FB-24 | 3 | High | **New (docs/requirements/authz-entitlements-billing.md):** entitlement and quota engine: plans, features, plan_features, add-ons, trials, user and org entitlements (B2C + B2B), user_overrides, quota_policies, immutable idempotent usage_events, usage_counters, atomic reservation/release, concurrency leases, hourly/daily/monthly/lifetime/storage periods, per user/team/resource scopes, soft/hard limits, 70/90/100 % alerts, rollover, usage packs, enterprise limits, plan-based API rate limits, resets via jobs, `has({permission})` / `has({feature})` / quota APIs; plugs into FB-21 pipeline steps 4 and 6 | FB-21, FB-61 | entitlement ≠ quota tests, concurrent reservation never over-grants, idempotent replay, period reset, alerts |
| FB-30 | 4 | Normal | Object storage: trait, local FS + S3 driver, streaming, multipart/resumable, checksums, dedup, versions, signed URLs, bucket policies, quotas, staging + reconciler | FB-21; FB-24 for storage quota | #10 #11 |
| FB-31 | 4 | Normal | KV engine: strings/hashes/lists/sets/zsets/streams, TTL, LRU + memory limit, namespaces, tenant isolation, pub/sub, REST | FB-21 | KV unit + API tests |
| FB-32 | 4 | Normal | RESP listener subset + compatibility matrix + conformance tests; Redis/Valkey adapter | FB-31 | redis-cli conformance suite |
| FB-33 | 4 | Normal | Storage Studio + Redis Studio | FB-30, FB-31 | build + e2e smoke |
| FB-40 | 5 | High | Transactional outbox, event bus, WebSocket + SSE, authz-filtered subscriptions, cursors/replay, presence, rooms, backpressure, heartbeats, limits, optional Redis fan-out | FB-21 | #16 #17 |
| FB-41 | 5 | Normal | TypeScript SDK v1 (collections, auth, files, realtime, retries, pagination, OpenAPI types) | FB-40 | SDK integration tests vs live server |
| FB-50 | 6 | High | Vector: canonical vectors in SQLite, USearch ANN as derived index, rebuild, metrics, filters, hybrid search (FTS5 + vector, RRF) | FB-21 | #12 #13 #23 |
| FB-51 | 6 | Normal | Graph: node/edge tables, recursive-CTE traversal, neighbours, shortest path, API | FB-21 | #14 #15 |
| FB-52 | 6 | Normal | Vector Studio + Graph Studio (graph explorer) | FB-50, FB-51 | build + e2e smoke |
| FB-60 | 7 | High | Wasmtime functions: capability host API, fuel/memory/timeouts, versions/rollback, CRUD hooks, HTTP functions, secrets, SSRF-safe fetch | FB-40 | #18 |
| FB-61 | 7 | Normal | Durable jobs: delayed, cron, retries/backoff, DLQ, priority, heartbeats, external workers, cancellation | FB-21 | #19 + restart survival |
| FB-62 | 7 | Normal | Functions Studio, jobs view, `forgebase functions deploy/logs` | FB-60, FB-61 | build + CLI tests |
| FB-70 | 8 | High | Billing: plans, subscriptions, metering, quotas, invoices, provider trait, Stripe + Razorpay, signed idempotent webhooks; off by default. **Addendum:** a proper billing application: catalog (products, prices: recurring monthly/yearly, one-time, usage-based/metered, tiered), subscriptions for users and orgs with lifecycle (trial, active, past_due, paused, cancelled), one-time purchases/orders, usage billing from FB-24 usage_events, overage and usage packs, proration on plan change, coupons/discounts, tax fields, invoices + credit notes/refunds, dunning, customer portal API, entitlement sync to FB-24 | FB-24, FB-61 | #20 + lifecycle, proration, metered invoice, webhook replay |
| FB-71 | 8 | Normal | Billing Studio, a proper billing app: catalog (plans, prices, features, add-ons, usage packs), customers (users and orgs), subscriptions with lifecycle actions, one-time orders, usage meters (e.g. 750 / 1,000 with 70/90/100 % states), invoices, payments, refunds, coupons, dunning queue, revenue metrics (MRR, ARR, churn) | FB-70 | build + e2e smoke over every screen |
| FB-80 | 9 | High | Security audit + fixes: threat model, bypass suite, SSRF, path traversal, upload validation, cargo-audit/deny | all of 1–8 | #25 full |
| FB-81 | 9 | Normal | Benchmarks (Criterion + HTTP load), documented hardware, recovery-time tests | all of 1–8 | report in `docs/benchmarks.md` |
| FB-82 | 9 | Normal | Rust SDK + Dart/Flutter SDK | FB-41 | SDK tests vs live server |
| FB-83 | 9 | Normal | Backup/restore/export/import/snapshots, full `doctor` | all of 1–8 | #21 #22 |
| FB-84 | 9 | Low | Docs set (architecture, api, database, security, deployment, sdk), release automation for 5 targets, Docker image | FB-80 | docs reviewed by god |
| FB-85 | 9 | High | Final QA: all 25 SPEC §27 scenarios end-to-end on a clean machine | all | 25/25 pass |

## 4. Parallelism

- Phase 1: FB-01 ∥ FB-02 (disjoint paths), then FB-03.
- Phase 3: FB-22 ∥ FB-21 once FB-20 lands; FB-23 ∥ FB-24 once FB-21 lands (FB-24 also needs FB-61).
- Addendum docs/requirements/authz-entitlements-billing.md (2026-11-07, human): B2C + B2B tenancy, 6-step evaluation pipeline, FB-24 entitlement/quota engine, full billing application in FB-70/FB-71.
- Phases 4, 6, 7: most packages run in parallel once FB-21 is merged.
- Dashboard studios always follow their API package.

## 5. Phase 1 interface contract (fixed so FB-01 and FB-02 can work in parallel)

- Default bind `127.0.0.1:8090`. Dashboard SPA at `/admin/` (Vite `base: "/admin/"`), built into `dashboard/dist/`, embedded in the binary at build time; the build must still succeed when `dashboard/dist/` is missing (serve a short fallback page).
- `GET /health` → `200 {"status":"ok"}`
- `GET /health/ready` → `200 {"status":"ready","checks":{"database":"ok","storage":"ok"}}`, `503` with the failing check when not ready.
- `GET /metrics` → Prometheus text format.
- `GET /api/system/info` → `{"name":"forgebase","version":"0.1.0","mode":"embedded","data_dir":"…","started_at":"RFC3339","uptime_secs":0,"database":{"path":"…","size_bytes":0,"journal_mode":"wal","schema_version":0}}`
- Errors everywhere: `{"error":{"code":"snake_case","message":"…","request_id":"…"}}`.
- Until FB-23 lands, admin data endpoints are read-only system info, and the server binds to loopback by default.
