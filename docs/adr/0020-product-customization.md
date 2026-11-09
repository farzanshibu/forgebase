# ADR-0020: Product customization

- Status: accepted
- Date: 2026-11-09
- Work package: FB-74 (MDF-48). Migration `0019_customize.sql`. Crate `forgebase-customize`.
- Scope: docs/requirements/authz-entitlements-billing.md §F: per-realm custom domains, white-label
  branding, transactional email, versioned legal agreements with acceptance records, hosted API docs.
- Builds on: ADR-0015 (realms, `X-ForgeBase-Realm`, outbound TLS client), ADR-0008 (policy pipeline),
  ADR-0007 (jobs queue, `ApiExtension`), ADR-0001.
- Leaves to later cards: the Identity & Tenant Studio screens (FB-27), hosted login/signup/consent
  pages beyond the public data they read (FB-26/FB-27), routing FB-25's own mails (verify, reset,
  magic link, OTP) through these templates (needs the realm on `forgebase_auth::Mail`).

## 1. Crate and changes elsewhere

`forgebase-customize` owns its tables, REST surface (an `ApiExtension` under `/api/v1/customize`), a
job handler, a request gate and a Host middleware. Changes outside it: migration 19 registered in
`forgebase-migrations`; `forgebase-server` opens the service, registers the job handler and the
extension, adds `AgreementGate` to the gates and wraps the final router in `host_router`.
No other crate changed. Migrations 16-18 are reserved for other packages (versions only need to
increase).

## 2. Custom domains

A realm admin claims `login.example.com` (`POST /domains`). The service stores a random token (two
UUIDv4, 244 bits) and returns the record to publish: **TXT `_forgebase-verify.<host>` =
`forgebase-verify=<token>`**. `POST /domains/{id}/verify` resolves the name through a pluggable
`DnsResolver` and marks the domain verified when any TXT string equals the value.

- Token life: `verify_ttl_ms` (7 days). After it, verify answers `409 token_expired`; `reissue`
  mints a new token (the old record no longer matches).
- A host is unique across realms. A *pending* claim whose token expired can be taken over by another
  realm, so squatting cannot block the real owner; a *verified* host is held until removed.
- Host names are validated (LDH labels, at least two, no IP, port, scheme, wildcard, `.local`,
  `.invalid`, `localhost`).
- Resolvers: `DohResolver` (DNS-over-HTTPS JSON, default Cloudflare) over `forgebase-auth`'s HTTPS
  client, so no DNS library is added and it works behind an egress proxy; `StaticResolver` for tests.
- **Host routing.** `host_router` is the outermost layer of the server. For a verified host it
  *sets* `X-ForgeBase-Realm` to the domain's realm (replacing the client's value) and adds a
  `ResolvedHost` extension, so tenancy, auth and policy treat the request as acting in that realm and
  the client cannot cross realms through a custom domain. Other hosts pass unchanged (the platform
  host serves every realm through the header). The lookup is an in-memory map reloaded after every
  change. `X-Forwarded-Host` is read only with `trust_forwarded_host` (set it only behind a proxy
  that overwrites the header).
- **TLS is not terminated by ForgeBase.** The reverse proxy in front holds the certificates of the
  custom hosts (for example Caddy on-demand TLS asking `GET /api/v1/customize/public/branding` with
  that Host, or certificates issued per verified domain from `GET /domains`) and forwards the
  original `Host`.

## 3. Branding

One row per realm: display name, four colours, sign-in title/subtitle/footer, support email; logo
and favicon as blobs. Everything is data: text is plain (pages escape it), colours must be
`#rgb`/`#rrggbb` (so they are safe in CSS), images are sniffed (PNG, JPEG, GIF, WebP, ICO, SVG) and
served with `nosniff` and `Content-Security-Policy: default-src 'none'; sandbox` so an SVG cannot
run script. Max image 256 KiB. `PUT` replaces the row (fields left out are cleared).

Public reads (no credentials): `/public/branding` (JSON), `/public/branding/{logo|favicon}`,
`/public/theme.css` (CSS variables `--fb-primary|background|text|accent`), `/public/agreements`,
and `/docs`. The realm is, in order: the verified custom domain of the request, `?realm=`, the
request's realm.

## 4. Email

Templates are per realm and key (`verify_email`, `password_reset`, `magic_link`, `email_otp`,
`invitation` ship built in; any other key is custom). A realm's row overrides the built-in text;
deleting it reverts. Subject, text and HTML are templates with `{{ name }}` / `{{ a.b }}`
substitution only (no logic, no raw output).

- HTML body: values are HTML-escaped; a variable named `link`, `url` or `*_link`/`*_url` must be an
  `http(s)://` or `mailto:` URL, else it renders `#` (no `javascript:`).
- Text body: control characters removed (newline and tab kept). Subject: control characters become
  spaces (no header injection). Values are never re-expanded.
- `app_name`, `realm`, `support_email` come from the realm and cannot be overridden by the caller.
- `POST /email/preview` renders a stored key or a draft without sending and lists missing variables.

**Sending.** `send` renders, writes a `queued` row to `_forgebase_email_deliveries` and enqueues a
`customize.email.send` job (`email_max_attempts` = 5, exponential backoff from
`email_backoff_base_ms`). The handler calls `CustomizeService::deliver`, which hands the message to
the `MailProvider` and records `sent`, `retrying` or `failed` with the attempt count and last error.
Provider errors are `Transient` (connection trouble, SMTP 4xx: retry) or `Permanent` (SMTP 5xx, bad
address, STARTTLS missing: give up at once). The delivery log never returns bodies. The rendered
body is kept only while a message may still be sent: cleared when sent, and from failed messages
after `failed_body_retention_ms` (7 days, swept on the next send); `retry` re-queues a failed
delivery while its body is stored.

**Providers.** `LogProvider` logs the envelope only (never the body); `SmtpProvider` is an own SMTP
client over `tokio` (plain, STARTTLS that refuses to continue in the clear, implicit TLS via rustls
through `forgebase-auth`'s `TlsSettings`; AUTH PLAIN/LOGIN; base64 MIME, `multipart/alternative`;
RFC 2047 subjects; address validation against command/header injection). No mail crate was added.
The server defaults to the log provider; an embedding application installs SMTP with
`set_mail_provider`.

## 5. Legal agreements

Per realm, per kind (`terms`, `privacy`, any key): versions 1, 2, ... published by `POST /agreements`.
Versions are immutable (trigger); the highest is current. Users accept the **current** version
(`POST /agreements/accept`; an older one is `409 agreement_outdated`); the acceptance (principal,
kind, version, time, client IP, user agent) goes to an append-only table (triggers refuse update and
delete). `AgreementGate` (a `RequestGate`) answers `403 agreement_required` with the pending versions
in `details.pending` to signed-in *users* who have not accepted every current version of their realm,
so publishing a new version forces re-acceptance at the next request. Exempt: `/api/auth/*`,
`/api/v1/customize/*`, health, metrics, `/api/openapi.json`; administrators, service accounts,
impersonation sessions and anonymous requests are not gated. The gate is synchronous and reads an
in-memory copy (current versions per realm; highest accepted version per realm, principal and kind)
loaded at open and updated on publish and accept: memory is one small entry per user and kind.
Client IP comes from the socket, or from the first `X-Forwarded-For` entry with
`trust_forwarded_host`. Erasure of a user (FB-25 anonymise) does not touch acceptance rows: they are
the legal record; the principal key is an opaque id.

## 6. Authorization

Privileged callers (system, administrators, admin service accounts) act on any realm (`?realm=`,
`realm` in bodies). Others act on the realm of the request and need the realm-level permission
`customization:write` (changes, sending) or `customization:read` (domain list, delivery log,
acceptance records, all versions, previews; write implies read). Grant them with a realm role such
as `{"allow": ["customization:*"]}`. Sending mail is a write permission: it is an abuse vector.

## 7. Limits and follow-ups

- FB-25's own mails still use `forgebase_auth::Mailer`; a `QueuedMailer` adapter belongs with the
  change that puts the realm on `Mail`.
- Domains are verified once; re-verification of live domains is not scheduled.
- SMTP STARTTLS/implicit TLS are implemented but exercised only against the plain mock (no
  certificate fixtures in this crate); the handshake code is the same rustls path FB-25 uses for LDAPS.
- The docs page renders `/api/openapi.json` in the browser (inline script, strict CSP); it is a
  reader, not an API console.
