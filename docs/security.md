# ForgeBase security

This page is the threat model of ForgeBase: what we protect, from whom, where
the trust boundaries are, and, for every subsystem, the attacks we considered,
the control that stops each one and the test that proves it. It was audited in
work package FB-80 (phase 9). The bypass suite in §6 runs the attacks against a
real server on every `cargo test`.

Sources: the ADRs linked in each section. Where this page and an ADR disagree,
the ADR is the design and this page is wrong. Please fix this page.

To report a vulnerability, see [`SECURITY.md`](../SECURITY.md).

## 1. What we protect

| Asset | Why it matters |
|---|---|
| Records and collections | Application data, possibly personal. |
| Credentials | Passwords, sessions, refresh tokens, API keys, PATs, MFA secrets, recovery codes, OAuth client secrets. |
| Signing keys | Sign access tokens, OIDC tokens and signed storage URLs. |
| Master key (`<data_dir>/auth/master.key`) | Root for the pepper and the encryption keys. Losing it or leaking it breaks the stored secrets. |
| Billing and provider secrets | Payment-provider keys, webhook secrets, store credentials. |
| Function code and secrets | Tenant code and its secrets. |
| Storage objects | Uploaded files. |
| The host and its network | The server process can reach the database file, the data directory, loopback services and the cloud metadata endpoint. |

## 2. Who we defend against

- Anonymous internet clients, and clients with a valid but low-privilege account.
- Authenticated users trying to reach another tenant, organization or realm.
- Holders of a narrow API key or PAT trying to act beyond their scope.
- Tenant-supplied server functions trying to reach the host, the database or the network.
- Anyone who can make the server send a request (webhook URLs, OAuth discovery documents, function `fetch`) trying to reach internal addresses (SSRF).
- Forged or replayed payment-provider webhooks and store notifications.
- Browsers on other origins trying to use a signed-in user's cookie (CSRF) or read responses (CORS).
- Someone who gets a copy of the database file alone.
- Someone with a shell on the host (out of scope for the software; see §7).

## 3. Trust boundaries and controls

| Boundary | Control | Decision |
|---|---|---|
| Network to HTTP API | TLS terminated by a reverse proxy you run. The binary speaks plain HTTP. Default bind is `127.0.0.1`. | [deployment.md](deployment.md#5-tls) |
| Credential to identity | Argon2id passwords (m = 19 MiB, t = 2, p = 1). Access tokens are 15-minute HS256 JWTs, checked against their session on each use. Refresh tokens rotate; reuse revokes the session. Alg must be `HS256` with a known `kid`. | ADR-0004 §§3, 6 |
| Stored secrets | Tokens, codes, API-key secrets and cookies are HMAC-SHA256 under a pepper derived from the master key. Signing keys, TOTP seeds and provider secrets are XChaCha20-Poly1305 encrypted, bound to their row. Secrets are shown once. | ADR-0004 §6 |
| Brute force | Per-IP fixed-window limits: 30 failed credential attempts per minute (a successful sign-in gives its attempt back), 20 mail-sending requests per hour. Admin sign-in has its own per-IP budget. Per-account lockout (5 failures in 15 minutes) persisted in the database. A known-device cookie lets an administrator's own browser past both (§4.1). | ADR-0004 §8; FB-80 |
| First-run setup | While no administrator exists, `POST /api/setup/admin` needs a one-time setup token: 256 random bits, kept in memory only as a SHA-256 digest, compared in constant time, gone once any administrator exists, new on every restart. It is printed to standard output only, never to the log. 10 attempts per client address per minute; completions are serialised, so only one administrator can be created this way. | FB-90; [deployment.md §2.4](deployment.md#24-first-run) |
| Client address | The socket address. `[auth] trust_forwarded_for = true` takes the **last** `X-Forwarded-For` entry (the one your proxy appended); entries the client wrote are ignored. | FB-80; [deployment.md §5](deployment.md#5-tls) |
| Sessions | Revocation is immediate: logout, device list, password change, password reset, account deletion, key revoke. CSRF token required on cookie writes. Cookies are `HttpOnly` and `SameSite`. | ADR-0004 §§4, 9 |
| Scope | API keys and PATs carry scopes, enforced centrally from the request path for every route; a scoped credential cannot mint a broader PAT. | ADR-0004 §10 |
| Authorization | Six-step pipeline on every protected operation: authentication, tenant, permission, entitlement, resource, quota. Deny wins. Each denial is audited with its step. | ADR-0008 §§5, 8 |
| Tenancy | Realm, org and tenant are resolved and checked for membership before the handler. A realm other than the caller's is refused at step 2. The data core adds the tenant to every statement on a tenant-scoped collection. | ADR-0008 §§2–4 |
| SQL | User filters compile to parameterised SQL. Never string concatenation. | ADR-0003, ADR-0001 §2 |
| Server functions | WebAssembly with no ambient authority: no WASI, filesystem, clock, network or processes. Only granted host capabilities. | ADR-0017 §§2–3 |
| Outbound HTTP | One address table ([`forgebase_auth::net::blocked_reason`](../crates/forgebase-auth/src/net/guard.rs)) for every client that a tenant or a remote document can steer: functions `fetch`, queue push webhooks, and the auth service's HTTPS client (OAuth/OIDC, SMS, Turnstile). Loopback, private, CGNAT, link-local (including 169.254.169.254), unique-local, multicast, documentation and IPv4-in-IPv6 forms are refused **after** DNS resolution, and the connection goes to the vetted address only (no DNS rebinding). | ADR-0017 §8; FB-80 |
| Object storage | Authorization per bucket and object; signed URLs; storage quota; declared types checked against the bytes; active content served as an inert attachment. | ADR-0018 §§6–8; FB-80 |
| Payment webhooks | Signature checked in constant time; Stripe timestamp tolerance 300 s; event ids are deduplicated; out-of-order events ignored. Store notifications are verified JWS chains (Apple Root CA G3) or provider-signed. Only test-mode provider keys accepted. | ADR-0021 §§6, 9 |
| Operational endpoints | `/metrics` and `/api/system/*` follow `[server] ops_access` (default `loopback`, §5). | FB-80 |
| Logs and audit | Credentials are never logged. Audit detail is scrubbed of keys such as `password`, `token`, `code`, `secret`. Request spans log the path only (query strings may carry tokens). Internal errors reach clients without SQL, paths or stack traces. | ADR-0004 §6, ADR-0001 §7 |
| Redis protocol | Off by default. Every connection must `AUTH` with an API key or PAT first; `kv:` scopes apply per namespace. No TLS: keep it on loopback or behind a tunnel. | ADR-0013 |
| Process | Panics return a 500 envelope, not a crash of the listener. Request timeouts. Body size limits. Graceful shutdown. | ADR-0001 §3 |
| Container and host | Image runs as uid 10001, no shell login. Compose: read-only root, `no-new-privileges`, all capabilities dropped. systemd unit: `ProtectSystem=strict`, `NoNewPrivileges`, empty capability set, system-call filter, `MemoryDenyWriteExecute`. | [deployment.md](deployment.md) |
| Supply chain | Dependencies are reviewed (ADR-0002). CI runs `cargo deny` (advisories, licences, bans, sources). Release binaries come with SHA-256 checksums. | ADR-0002; `deny.toml` |

## 4. Threat model per subsystem

Each row: the attack, the control, and the test that tries it. Tests named
`security_bypass::…` are in
[`crates/forgebase-server/tests/security_bypass.rs`](../crates/forgebase-server/tests/security_bypass.rs)
and run against a live server; the others are crate tests.

### 4.1 Authentication (`forgebase-auth`, ADR-0004, 0009, 0015, 0019)

| Threat | Control | Proof |
|---|---|---|
| Password guessing, credential stuffing | 30 failed attempts/min per IP, account lockout after 5 failures (persisted), same cost for unknown accounts | `security_bypass::brute_force_is_locked_out_and_rate_limited_without_trusting_client_headers`; `auth_flows` |
| Locking the administrator out from a shared address (NAT, proxy): flooding sign-in from that address, or failing on purpose with the admin's email (FB-81 finding) | **FB-80:** admin sign-in has its own per-IP budget, so end-user traffic cannot spend it; budgets count failures only (a success gives its attempt back, counted first so parallel guesses stay bounded); a successful admin sign-in sets `fb_admin_device` (`HttpOnly`, `SameSite=Strict`, path `/api/auth/admins/login`, 90 days, HMAC-bound to the email and issue time), and a browser presenting it skips the per-IP budget and the account lockout, with its own budget of 5 failures per 15 minutes | `security_bypass::an_attacker_on_the_admins_address_cannot_lock_the_admin_out`; `limits::refunds_give_back_one_event` |
| Spoofing `X-Forwarded-For` to reset the IP budget | Header ignored unless `trust_forwarded_for`; then only the last (proxy) entry counts | `security_bypass::brute_force_…`; `forgebase-auth` unit test `forwarded_for_uses_the_entry_the_proxy_appended` |
| Forged or `alg: none` tokens, foreign keys | HS256 only, known `kid`, session row checked on each use; a bad credential is `401`, never anonymous | `security_bypass::administrative_endpoints_refuse_anonymous_and_ordinary_users`; `auth_flows` |
| Refresh-token theft | Rotation; reuse revokes the session | `auth_flows` |
| CSRF on cookie sessions | `X-CSRF-Token` required on every non-GET cookie request, constant-time compare; `HttpOnly`, `SameSite` cookie; tokens not returned in cookie mode | `security_bypass::cors_is_closed_and_cookie_writes_need_the_csrf_token` |
| Cross-origin reads | No CORS headers unless origins are configured | same test |
| MFA bypass, recovery-code reuse | MFA challenge token, rate-limited verification with lockout, single-use recovery codes | `auth_flows` |
| OAuth state/nonce/PKCE abuse, ID-token forgery | State and nonce bound to the flow, PKCE, JWKS-verified RS256/ES256 | `social_login` (ADR-0009) |
| SSRF through an OIDC discovery document, SMS gateway or Turnstile URL | Auth HTTPS client refuses blocked ranges unless `allow_private_outbound` | `https_client::the_default_auth_client_refuses_private_addresses`; `security_bypass::outbound_clients_never_reach_internal_addresses` |
| Impersonation abuse | Admin only, audited, impersonated sessions cannot mint PATs | `auth_flows` |
| Realm OAuth providers and SMS gateways set through the admin API (FIX-4): misuse or secret disclosure | Administrators only; client secrets and gateway tokens sealed per realm (row-bound AAD) and never returned; their URLs go through the same SSRF-guarded client | `security_bypass::administrative_endpoints_…`, `secrets_never_reach_logs_or_api_output`, `outbound_clients_never_reach_internal_addresses`; `identity_admin` |
| OIDC signing keys exposed through `/api/auth/realms/{realm}/keys` | Lists `kid`, algorithm and status only | `security_bypass::secrets_never_reach_logs_or_api_output` |

### 4.2 Policy and tenancy (`forgebase-policy`, ADR-0008)

| Threat | Control | Proof |
|---|---|---|
| Selecting another realm (`X-ForgeBase-Realm`) | Step 2 refuses any realm but the identity's home realm | `security_bypass::realm_org_and_tenant_boundaries_hold_over_http` |
| Selecting another org or tenant, or adding oneself to it | Active membership required; member management needs `org` permissions | same test |
| Reading or writing a known record id of another tenant | Data core adds `_tenant = ctx.tenant` to every statement; `_tenant` is read-only and never returned | same test; `forgebase-policy` `isolation` tests |
| An allow somewhere overriding a deny | Deny wins at every level (role, role set, override, ACL) | `security_bypass::a_deny_beats_every_allow`; `rbac` tests |
| Rule injection widening access | Rules compile to SQL with parameters; random rule fuzzing never widens | `isolation::random_rules_never_widen_access`, `injection_and_existence_oracles_fail` |
| A scoped admin key reading other collections through `expand` or a relation filter (`ref.title = …`) | **FB-80:** scopes bind privileged callers in the engine and in the relation-path field guard | `security_bypass::scoped_keys_and_pats_cannot_escalate`; `isolation::scopes_bind_privileged_callers_across_relations` |

### 4.3 Records and collections (`forgebase-data`, `forgebase-query`, ADR-0003)

| Threat | Control | Proof |
|---|---|---|
| SQL injection through filters, sort, fields, ids, collection names | Parser to parameterised SQL; identifiers validated against the schema | `security_bypass::malicious_inputs_are_refused_without_errors_or_leaks`; `forgebase-query` tests |
| Reading system tables (`_forgebase_*`, `sqlite_master`) | Not collections; refused | same test |
| Hidden fields leaking through filters or `expand` | Field rules apply to filters, sort and expansion | `isolation::hidden_fields_do_not_leak` |
| Oversized or deeply nested bodies | 4 MiB JSON limit (`413`), serde depth limit (`400`) | `security_bypass::malicious_inputs_…` |
| Error messages leaking SQL or paths | Internal errors are generic envelopes | same test |

### 4.4 Object storage (`forgebase-storage`, ADR-0018)

| Threat | Control | Proof |
|---|---|---|
| Path traversal in object keys (`../`, encoded, backslashes) | Keys are opaque names in the database; blobs are content-addressed (`sha256`), driver keys are internal and reject `.`/`..`/`\` segments | `security_bypass::storage_keys_cannot_escape_and_uploads_are_validated` |
| Traversal in bucket names | `[a-z0-9._-]`, starting alphanumeric | same test |
| Oversized uploads | Bucket and instance limits checked while streaming (`413`), also without `Content-Length` | same test; `storage::bucket_limits_and_content_types_are_checked` |
| Type confusion: HTML or a PDF uploaded as `image/png` into an `image/*` bucket | **FB-80:** a declared PNG, JPEG, GIF, WebP or PDF must carry that file signature (`400 content_type_mismatch`); the allow-list is checked against the resolved type | same test; `util::declared_types_with_a_signature_must_match_the_bytes` |
| Stored XSS through downloads | `Content-Security-Policy: sandbox`, `X-Content-Type-Options: nosniff`, HTML/SVG/XML served as attachments | same test |
| Signed URL tampering | HMAC-SHA256 over method, bucket, key, expiry, version, file name, content type and subject (FB-140: and tenant and uploader, length-prefixed) | `storage.rs` signed-URL tests |
| Signed URL forgery from a database copy (G11) | **FB-140:** the signing key is sealed with the auth keyring; the plain key of older installs is sealed unchanged at start | `security_fb140::g11_the_signing_key_is_not_readable_through_sql`; `storage/tests/fb140.rs` |
| A signed URL outlives the signer's access (G15) | By design (checked when issued, as S3 and Supabase): 15-minute default lifetime, 7-day cap, `rotate-signing-key` revokes all | `security_fb140::g15_rotating_the_signing_key_revokes_every_signed_url` |
| Prefix ACL entry used as a bucket grant (G1) | **FB-140:** bucket operations match only entries with the empty prefix | `security_fb140::g1_a_prefix_grant_is_not_a_bucket_grant` |
| Cross-tenant buckets; unchecked object owners (G14) | **FB-140:** buckets per tenant (`<tenant>/<name>` stored, never addressable); bucket rules over object fields compiled into the object SQL | `security_fb140::g14_*` |

### 4.5 Server functions (`forgebase-functions`, ADR-0017)

| Threat | Control | Proof |
|---|---|---|
| Escaping the sandbox (files, clock, processes, raw sockets) | Wasmtime without WASI; only granted host functions | `forgebase-functions/tests/sandbox.rs` |
| Resource exhaustion | Fuel, memory and wall-time limits per call | `sandbox.rs` |
| SSRF through `fetch` | Blocked ranges after resolution, vetted address only, redirects re-checked, per-function host allow-list, credentials dropped across origins | `ssrf` tests (5); `security_bypass::outbound_clients_never_reach_internal_addresses` |
| Reading another function's secrets | Secrets sealed per function; values never returned by the API | `security_bypass::secrets_never_reach_logs_or_api_output` |
| Deploying code without permission | `Action::ManageFunctions` through the policy engine (administrators by default) | `security_bypass::administrative_endpoints_…` |
| JavaScript escaping the engine (ADR-0027) | QuickJS runs **inside** the same Wasmtime sandbox; its only WASI imports are shims (clock, CSPRNG, log; no files, env, sockets, exit); a fresh instance per execution from a pre-user-code snapshot | `js::tests::engine_imports_only_the_shimmed_functions`; `tests/js.rs` limits |
| JavaScript `ctx` services widening access | Every service runs **as the caller** (rules, field rules, ACLs, tenancy, bucket ACLs, policies); application KV namespaces need an admin-managed rule (deny by default) | `tests/js.rs`; `forgebase-server/tests/functions_js.rs` |
| A non-admin granting the `service_role` bypass | Only administrators deploy versions with `service_role` (403 otherwise); each such deploy is in `_forgebase_auth_audit` | `tests/js.rs::service_role_is_granted_only_by_administrators_and_bypasses_rules` |

### 4.6 Realtime (`forgebase-realtime`, ADR-0011)

| Threat | Control | Proof |
|---|---|---|
| Subscribing to records one may not read | Subscriptions authorised when made; **every event** re-checked against the subscriber's current access | `forgebase-realtime/tests/realtime.rs`, `realtime_e2e` |
| Keeping a socket after revocation | Revoked or expired credentials close the socket (`4001`) | `realtime.rs` |
| Cross-tenant events | The per-event check uses the subscriber's own context (realm, org, tenant), so the data core's tenant rules apply | `realtime.rs` |
| Flooding | Per-connection and per-principal limits, bounded queues, slow consumers closed (`4008`) | `realtime.rs` |

### 4.7 Queues and topics (`forgebase-queue`, ADR-0022)

| Threat | Control | Proof |
|---|---|---|
| SSRF through a push webhook URL | `https://` only (unless `allow_http_push`), no credentials in URLs, literal blocked addresses refused at creation; names resolved and vetted at **delivery** by the functions fetcher | `security_bypass::outbound_clients_never_reach_internal_addresses` |
| Forged deliveries to the subscriber | Each delivery is HMAC-signed with the subscription's secret | `forgebase-queue` tests |
| Reading another tenant's messages | `engine:queue_*` permissions through the policy engine | `forgebase-queue` tests |

### 4.8 Jobs (`forgebase-jobs`, ADR-0007, 0023)

| Threat | Control | Proof |
|---|---|---|
| Enqueueing or reading jobs without permission | Every route asks the authorizer first (`Action::EnqueueJob` and friends) before reading the body | `forgebase-jobs/tests/http.rs`; `security_bypass::administrative_endpoints_…` |
| Using job progress channels to spy | `jobs:<id>` channels need permission to read jobs; only the system publishes | `forgebase-jobs/tests/long_tasks.rs` |

Accepted risk: a job runs with the system's authority. Grant
`engine:jobs_enqueue` only to principals you would let run any registered job
type (for example `function.invoke`).

### 4.9 Billing webhooks and store receipts (`forgebase-billing`, ADR-0021)

| Threat | Control | Proof |
|---|---|---|
| Forged webhook bodies | Constant-time HMAC (Stripe, Razorpay, Creem), platform-certificate signatures (WeChat Pay), JWS chain to Apple Root CA G3 (App Store), Google-signed OIDC token on the Pub/Sub push (Google Play) | `forgebase-billing/tests/webhooks.rs`, `stores.rs`, `wechat_pay.rs` |
| Replays and reordering | Timestamp tolerance, event-id dedup, out-of-order events ignored; a store transaction id is unique, so a replay is a no-op | `webhooks.rs`, `stores.rs` |
| Live keys in a test system | Only test-mode keys accepted; provider URLs must be `https://` | `forgebase-billing/tests/http.rs`, `payments.rs` |
| Forged traffic filling the disk through the webhook inspector (ADR-0032) | Unverified deliveries are stored only within a per-provider budget (60 a minute, 1 MiB of body and headers a minute; unknown provider names share one budget) and under total caps (10 000 rows, 64 MiB; the oldest unverified rows are evicted first); verified deliveries are authenticated by the provider; every row is deleted after `billing.webhook_log_retention_days`. The answer to the caller is the same whether or not the delivery was stored, and an unverified payload is never processed | `forgebase-billing/tests/inspector.rs` |
| Secrets in the webhook log | Headers are an allowlist (content type, user agent, forwarding headers, provider signature headers): never `authorization` (Google Play's push token), cookies or API keys. A refusal reason is the stable code and the driver's fixed message, never a secret or the expected signature; upstream error text is reduced to its code. Metric labels come from fixed lists | `inspector.rs`, `stores.rs` |
| Personal data in stored webhook bodies | Provider payloads carry customer names, emails, addresses, card brand and last digits, and store receipts. They are readable by platform administrators only (`GET /billing/webhook-deliveries/{id}`; the list never returns bodies), kept for `billing.webhook_log_retention_days` (default 30), and are part of database backups like the rest of billing | `inspector.rs` |
| Injecting a payload through a stored delivery (replay, re-verify) | Replay and re-verification verify the stored body again with the current secrets, as of the time it was received; only Google Play pushes, whose token is not stored, are re-read, and their purchase state always comes from the Play API. Re-verification processes a delivery at most once (one operator action at a time, a claim on the row, the event dedup inside the write transaction); Google Play deliveries cannot be re-verified. Both actions are platform-administrator only and audit-logged (`_forgebase_billing_audit`) | `inspector.rs`, `stores.rs`, `wechat_pay.rs` |

### 4.10 KV and the Redis protocol (`forgebase-kv`, `forgebase-resp`, ADR-0005, 0013)

| Threat | Control | Proof |
|---|---|---|
| Unauthenticated RESP commands | Every connection must `AUTH` with an API key or PAT first; nothing else is accepted before | `forgebase-resp/tests/real_auth.rs`; server `resp` test |
| A `kv:read` key writing | Namespace scopes checked per command | `real_auth.rs` |
| Namespace hopping over HTTP | `kv:<access>:<ns>` scopes and policy per namespace | `forgebase-kv/tests/api.rs`; `security_bypass::scoped_keys_and_pats_cannot_escalate` |
| Sniffing RESP traffic | No TLS: loopback or a tunnel only | documented (§8) |

### 4.11 Vector and graph (`forgebase-vector`, `forgebase-graph`, ADR-0006, 0012)

Both are built on the data service: every call asks the authorizer first
(`VectorRead`/`VectorWrite`, `GraphRead`/`GraphWrite`) and rows are
tenant-scoped like records. They contain no authentication or policy code of
their own, so §4.2 and §4.3 apply. Proof: `forgebase-vector/tests/service.rs`,
`http.rs` and `forgebase-graph/tests/graph.rs`.

### 4.12 Admin console and administrative APIs (ADR-0014)

| Threat | Control | Proof |
|---|---|---|
| Ordinary users or anonymous callers using admin APIs, including the FIX-4 identity, billing, storage and KV admin endpoints | `require_admin` / privileged context or policy permission on every admin route | `security_bypass::administrative_endpoints_refuse_anonymous_and_ordinary_users` (54 routes, billing on) |
| Path traversal in `/admin/{*path}` | Assets are embedded in the binary (no filesystem lookup) | `security_bypass::storage_keys_…` |
| Backups (FB-83, `/api/v1/system/backups`): reading or deleting snapshots, restoring a crafted archive that writes outside the data directory | Administrators only; restore verifies the manifest first and refuses any path that is absolute or has an empty, `.`, `..` or `\` segment | `security_bypass::administrative_endpoints_…`; `forgebase-backup` tests |
| XSS in the console | React escapes output; admin sessions use an `HttpOnly` cookie plus CSRF token | dashboard tests |

### 4.13 `/metrics` and `/api/system/*`

| Threat | Control | Proof |
|---|---|---|
| Anonymous remote reads of metrics, data directory and database path | **FB-80:** `ops_access` gate (§5); `/metrics` needs scope `system:read` for scoped keys | `security_bypass::ops_endpoints_need_an_administrator_unless_called_directly_on_loopback`; `forgebase-api` `ops_endpoints_refuse_proxied_and_remote_anonymous_callers` |

## 5. Admin and operational access

The dashboard at `/admin/` and the admin sign-in at `POST /api/auth/admins/login`
are part of the product (ADR-0014). The server still binds to loopback by
default, and the shipped Docker and compose files publish the port on
loopback only. Keep it that way unless a reverse proxy with TLS sits in front.

`/metrics` and `/api/system/*` are operational endpoints. FB-80 decided:

- **Administrators always.** An admin session (the dashboard's cookie), an
  admin access token, or an admin service account's API key. A scoped key
  needs `system:read`.
- **Unauthenticated callers only on the same host, directly** (default
  `[server] ops_access = "loopback"`): the socket peer is a loopback address
  and the request has no `Forwarded`, `X-Forwarded-For` or `X-Real-IP`
  header. A Prometheus on the host scrapes as before; a client coming through
  a reverse proxy on the host is not "local".
- `ops_access = "admin"` drops the loopback exemption; `"public"` restores the
  old behaviour (only behind a proxy that restricts these paths itself).
- `/health` and `/health/ready` stay public for load balancers. They reveal
  only `ok`, or which check failed.

## 6. The bypass suite

```sh
CARGO_INCREMENTAL=0 cargo test -p forgebase-server --test security_bypass
```

| Test | What it tries |
|---|---|
| `ops_endpoints_need_an_administrator_unless_called_directly_on_loopback` | `/metrics` and `/api/system/info` through a proxy, as a user, with a key lacking `system:read`; `ops_access = "admin"` |
| `realm_org_and_tenant_boundaries_hold_over_http` | Foreign realm header, foreign org header and membership edit, a known record id in another tenant (read, update, delete, list), smuggling `_tenant`, anonymous reads |
| `a_deny_beats_every_allow` | `reports:*` allow plus `reports:export` deny, in any role order, plus the org `admin` role |
| `scoped_keys_and_pats_cannot_escalate` | A `collections:read:notes` admin key on other collections, through `?expand=` and a relation filter, writes, auth, KV, storage, realtime; a scoped PAT minting `*`, `collections:read`, `auth:*`, `collections:*`; malformed scopes |
| `administrative_endpoints_refuse_anonymous_and_ordinary_users` | 54 admin routes (auth, policy, functions, storage, queues, jobs, FB-83 backups, and FIX-4's realm keys, consents, OAuth providers, SMS, billing admin, bucket usage and metadata, KV lists, streams and channels) as anonymous and as a user, with billing on; forged JWTs and keys |
| `outbound_clients_never_reach_internal_addresses` | Queue push to loopback, metadata, IPv4-mapped, private, unique-local, `file:`/`gopher:`, credentials in URLs, and a name resolving to loopback; functions `fetch` to loopback and metadata; OAuth providers set through the admin API with a loopback issuer, and with a loopback token URL hit by the callback; an OIDC provider from the provider file. A canary listener proves nothing connected. |
| `storage_keys_cannot_escape_and_uploads_are_validated` | Five traversal key forms, bad bucket names, `/admin` traversal, HTML and PDF under `image/png`, HTML into an `image/*` bucket, oversize, inert download headers |
| `brute_force_is_locked_out_and_rate_limited_without_trusting_client_headers` | 30 wrong passwords with a fresh `X-Forwarded-For` each; then the right password; an unknown account |
| `an_attacker_on_the_admins_address_cannot_lock_the_admin_out` | 40 successful colleague sign-ins (no budget spent); an end-user sign-in flood; 6 deliberate failures with the admin's email; an admin sign-in flood; forged, altered and foreign device tokens; 5 failures through the device itself |
| `cors_is_closed_and_cookie_writes_need_the_csrf_token` | Cross-origin GET and preflight; cookie writes without and with a wrong CSRF token |
| `secrets_never_reach_logs_or_api_output` | Captures every log line at TRACE while signing in, failing a sign-in, refreshing, minting keys, PATs, a webhook secret, a function secret, a realm SMS token and an OAuth client secret; then reads every listing endpoint (including realm SMS, providers and signing keys) and checks no secret or private key appears in a log line or a response |
| `malicious_inputs_are_refused_without_errors_or_leaks` | SQL injection in filter, sort, fields, ids and collection names; system tables; NoSQL-style operators; 5 MiB and 100 000-deep bodies; truncated JSON. No `5xx`, no SQL or paths in errors. |

Crate-level security tests that complement it: `forgebase-functions/tests/ssrf.rs`
(rebinding, redirects, allow-lists), `forgebase-auth/tests/https_client.rs`,
`forgebase-policy/tests/isolation.rs` and `rbac.rs`, `forgebase-storage/tests/storage.rs`,
the billing and store webhook tests, and `forgebase-resp` tests.

## 7. Out of scope

- A user with shell access to the host, or root, can read the database file and
  the master key. Protect the host and the data directory (`0700`).
- Upstream providers (payment providers, app stores, identity providers, SMTP, SMS).
- Denial of service at network scale. Put a proxy or WAF in front.
- LDAP servers and S3 endpoints are configured by the operator and commonly
  live on a private network; their clients are not behind the SSRF guard.

## 8. Accepted risks and follow-ups

1. **Argon2 in the write transaction.** Records written through general API
   paths hash `password` fields inside the single writer's transaction. Auth
   flows avoid this (ADR-0004 §6). Follow-up for the data core.
2. **Rate limits are per process.** Fixed-window limits live in memory. A
   shared limiter (Redis) is a later phase (ADR-0004 §8). Multiple instances
   multiply the limit; the per-account lockout is shared (database).
   Known-device budgets are per process too.
2a. **Admin lockout for new browsers.** An attacker who knows an
   administrator's email can still lock it for browsers without a
   known-device cookie (the CLI, a new machine) for 15 minutes. Keep a second
   administrator, use API keys for automation, and sign in from the
   dashboard once so the browser is known. End users have no device bypass.
3. **Auth routes in OpenAPI.** ADR-0004 lists `/api/auth/*` as not fully
   annotated. Check the live `/api/openapi.json` before you rely on it.
4. **Mail.** The server's default mail provider logs the message envelope only.
   An embedding application must install SMTP (ADR-0020). The auth dev mailer
   writes messages as JSON files under `<data_dir>/mail/` (ADR-0004 §12), so
   a deployment that uses it leaves one-time links on disk.
5. **RESP listener has no TLS.** Keep it on loopback, or tunnel it.
6. **SVG uploads.** An `image/*` allow-list admits `image/svg+xml`. Downloads
   are inert (attachment, `CSP: sandbox`, `nosniff`); list exact types such as
   `image/png` if you embed uploads elsewhere.
7. **Jobs run as the system** (§4.8).
8. **Push webhooks may redirect to `http://`** public addresses even when
   `allow_http_push` is off; internal addresses stay refused on every hop.
9. **Bucket names are not secret.** Anonymous callers get `404` for a missing
   bucket and `401`/`403` for an existing private one, so bucket names can be
   probed. Object keys and contents stay protected. Do not put secrets in
   bucket names.
10. **Some admin routes parse the body before the admin check**, so a
    non-admin with a malformed body gets `400` instead of `403`. Nothing is
    read or changed; it only tells the caller the expected shape, which the
    OpenAPI document publishes anyway.
11. **Billing webhook bodies are personal data at rest** (ADR-0032). They
    stay in the database (and its backups) for
    `billing.webhook_log_retention_days`; lower it if your privacy policy
    needs a shorter period. The unverified-delivery budget is per process
    and resets on restart.

Resolved by FB-80: the bypass suite (§6), path traversal and upload
validation (§4.4), forwarded client addresses (`[auth] trust_forwarded_for`,
last entry only), the unauthenticated `/metrics` (§5), SSRF through the auth
HTTPS client, scope bypass by scoped admin keys through relations, and the
missing disclosure policy (`SECURITY.md`).

## 9. Behaviour changed by FB-80

Each is a security fix; nothing else changed.

| Change | Before | After |
|---|---|---|
| `/metrics`, `/api/system/info` | Anyone | Admins; unauthenticated only from a direct loopback peer (`ops_access`) |
| `/metrics` with a scoped key | No scope needed | `system:read` |
| Admin service-account key with resource scopes (`collections:read:notes`) | Could read other collections through `?expand=` and relation filters | Refused (`insufficient_scope`; a relation path into another collection is an unknown field) |
| Trusted `X-Forwarded-For` | First (client-written) entry | Last (proxy-appended) entry; settable as `[auth] trust_forwarded_for` |
| Auth outbound HTTPS (OAuth/OIDC, SMS, Turnstile) | Any address | Loopback, private, link-local and metadata refused unless `[auth] allow_private_outbound` |
| Upload declared as PNG/JPEG/GIF/WebP/PDF | Accepted with any bytes | `400 content_type_mismatch` unless the bytes match |
| Upload with no type and a signature-type extension (`x.png`) whose bytes differ | Stored as `image/png` | Stored as `application/octet-stream` (so an `image/*` allow-list refuses it) |
| Sign-in rate limit | Every attempt counted; admin and end-user sign-ins share one per-IP budget | Failures counted; admin sign-in has its own budget; `fb_admin_device` cookie set on admin sign-in, bypassing the per-IP budget and the lockout for that browser |
