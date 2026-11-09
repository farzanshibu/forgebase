# ForgeBase security

> **Status: draft.** This summarises the security decisions in the ADRs and
> lists the gaps we know about. Work package FB-80 (security audit and fixes)
> finishes it: it adds the full threat model, a bypass test suite, and fixes for
> the open items in §6. Until then, treat §6 as the list of things not yet proven.

Sources: the ADRs linked in each section. Where this page and an ADR disagree,
the ADR is the design and this page is wrong. Please fix this page.

## 1. What we protect

| Asset | Why it matters |
|---|---|
| Records and collections | Application data, possibly personal. |
| Credentials | Passwords, sessions, refresh tokens, API keys, PATs, MFA secrets, recovery codes, OAuth client secrets. |
| Signing keys | Sign access tokens and OIDC tokens. |
| Master key (`<data_dir>/auth/master.key`) | Root for the pepper and the encryption keys. Losing it or leaking it breaks the stored secrets. |
| Billing and provider secrets | Stripe and Razorpay keys, webhook secrets. |
| Function code and secrets | Tenant code and its secrets. |
| Storage objects | Uploaded files. |

## 2. Who we defend against

- Anonymous internet clients, and clients with a valid but low-privilege account.
- Authenticated users trying to reach another tenant, organization or realm.
- Holders of a narrow API key or PAT trying to act beyond their scope.
- Tenant-supplied server functions trying to reach the host, the database or the network.
- Forged or replayed payment-provider webhooks.
- Someone who gets a copy of the database file alone.
- Someone with a shell on the host (out of scope for the software; see §5).

## 3. Trust boundaries and controls

| Boundary | Control | Decision |
|---|---|---|
| Network to HTTP API | TLS terminated by a reverse proxy you run. The binary speaks plain HTTP. Default bind is `127.0.0.1`. | [deployment.md](deployment.md#5-tls) |
| Credential to identity | Argon2id passwords (m = 19 MiB, t = 2, p = 1). Access tokens are 15-minute HS256 JWTs, checked against their session on each use. Refresh tokens rotate; reuse revokes the session. Alg must be `HS256` with a known `kid`. | ADR-0004 §§3, 6 |
| Stored secrets | Tokens, codes, API-key secrets and cookies are HMAC-SHA256 under a pepper derived from the master key. Signing keys and TOTP seeds are XChaCha20-Poly1305 encrypted, bound to their row. Secrets are shown once. | ADR-0004 §6 |
| Brute force | Per-IP fixed-window limits: 30 credential attempts per minute, 20 mail-sending requests per hour. Per-account lockout persisted in the database. | ADR-0004 §8 |
| Sessions | Revocation is immediate: logout, device list, password change, password reset, account deletion, key revoke. CSRF token required on cookie writes. | ADR-0004 §§4, 9 |
| Scope | API keys and PATs carry scopes; a scoped credential cannot mint a broader PAT. | ADR-0004 §10 |
| Authorization | Six-step pipeline on every protected operation: authentication, tenant, permission, entitlement, resource, quota. Deny wins. Each denial is audited with its step. | ADR-0008 §§5, 8 |
| Tenancy | Realm, org and tenant are resolved and checked for membership before the handler. A realm other than the caller's is refused at step 2. | ADR-0008 §§2–4 |
| SQL | User filters compile to parameterised SQL. Never string concatenation. | ADR-0003, ADR-0001 §2 |
| Server functions | WebAssembly with no ambient authority: no WASI, filesystem, clock, network or processes. Only granted host capabilities. | ADR-0017 §§2–3 |
| Outbound HTTP from functions | SSRF-safe fetch: scheme and host allow-list, refusal of private and link-local address ranges (including 169.254.169.254), connect to the vetted address only (no DNS rebinding), at most 5 redirects with credentials dropped on origin change, size and time limits. | ADR-0017 §8 |
| Object storage | Authorization per bucket and object; signed URLs; storage quota. | ADR-0018 §§6–8 |
| Payment webhooks | Signature checked in constant time; Stripe timestamp tolerance 300 s; event ids are deduplicated; out-of-order events ignored. Only test-mode provider keys accepted. | ADR-0021 §6 |
| Logs and audit | Credentials are never logged. Audit detail is scrubbed of keys such as `password`, `token`, `code`, `secret`. Internal errors reach clients without SQL, paths or stack traces. | ADR-0004 §6, ADR-0001 §7 |
| Redis protocol | Off by default. Every connection must `AUTH` with an API key or PAT first. No TLS: keep it on loopback or behind a tunnel. | ADR-0013; [deployment.md](deployment.md#7-configuration-keys) |
| Process | Panics return a 500 envelope, not a crash of the listener. Request timeouts. Graceful shutdown. | ADR-0001 §3; [architecture.md §4](architecture.md#4-how-a-request-flows) |
| Container and host | Image runs as uid 10001, no shell login. Compose: read-only root, `no-new-privileges`, all capabilities dropped. systemd unit: `ProtectSystem=strict`, `NoNewPrivileges`, empty capability set, system-call filter, `MemoryDenyWriteExecute`. | [deployment.md](deployment.md) |
| Supply chain | Dependencies are reviewed (ADR-0002). CI runs `cargo deny` (advisories, licences, sources). Release binaries come with SHA-256 checksums. | ADR-0002; `.github/workflows/ci.yml`; `release.yml` |

## 4. Admin access

The dashboard at `/admin/` and the admin sign-in at `POST /api/auth/admins/login`
are part of the product (ADR-0014). The server still binds to loopback by
default, and the shipped Docker and compose files publish the port on
loopback only. Keep it that way unless a reverse proxy with TLS sits in front.

## 5. Out of scope

- A user with shell access to the host, or root, can read the database file and
  the master key. Protect the host and the data directory (`0700`).
- Upstream providers (Stripe, Razorpay, identity providers, SMTP, SMS).
- Denial of service at network scale. Put a proxy or WAF in front.

## 6. Open items

These are known gaps. Each one is either tracked for FB-80 or is a follow-up
named in its ADR. Nothing here has been verified by a bypass test suite yet.

1. **Threat model review.** This page is a draft. FB-80 owns the full model.
2. **Bypass suite.** No end-to-end suite that tries each denial path yet
   (FB-80).
3. **Path traversal and upload validation.** Named in FB-80; not yet audited
   in this document.
4. **Argon2 in the write transaction.** Records written through general API
   paths hash `password` fields inside the single writer's transaction. Auth flows
   avoid this (ADR-0004 §6). Follow-up for the data core.
5. **Rate limits are per process.** Fixed-window limits live in memory. A shared
   limiter (Redis) is a later phase (ADR-0004 §8). Multiple instances multiply the limit.
6. **Forwarded client addresses.** The client IP is the socket address.
   `X-Forwarded-For` is trusted only when `trust_forwarded_for` is set, but there
   is no `[auth]` section in `forgebase.toml` yet to set it (ADR-0004 §13).
   Behind a proxy, every request looks like it comes from the proxy's address.
7. **Auth routes in OpenAPI.** ADR-0004 lists `/api/auth/*` as not fully
   annotated. Check the live `/api/openapi.json` before you rely on it.
8. **Mail.** The server's default mail provider logs the message envelope only.
   An embedding application must install SMTP (ADR-0020). The auth dev mailer
   writes messages as JSON files under `<data_dir>/mail/` (ADR-0004 §12), so
   a deployment that uses it leaves one-time links on disk.
9. **No disclosure policy in the repository.** There is no `SECURITY.md`.
   Add one with a contact route before public release.
10. **RESP listener has no TLS.** Keep it on loopback, or tunnel it.
