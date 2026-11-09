# ADR-0015: Realm-scoped identity, phone/SMS, LDAP, Turnstile, account lifecycle, outbound HTTPS

- Status: accepted
- Date: 2026-11-09
- Work package: FB-25 (MDF-45)
- Builds on: ADR-0004 (auth core), ADR-0008 (realms, tenancy layer), ADR-0009
  (social login, passkeys; §9 outbound HTTPS), ADR-0014 (impersonation),
  requirements §E/§F.
- Out of scope: ForgeBase as identity provider (FB-26), identity studio UI
  for realms/providers (FB-27), hosted login pages and transactional email
  templates (FB-74).

## 1. Layout

All in `forgebase-auth`: `net/` (HTTP/1.1 framing, rustls client),
`realm.rs`, `phone.rs` + `sms.rs`, `ldap/` (BER, RFC 4515 filters, client),
`turnstile.rs`, `lifecycle.rs`, routes in `identity_http.rs`. Migration
`0014_identity.sql`. `forgebase-policy` reads home realms from the auth
service's `RealmDirectory`; `forgebase-server` wires it.

## 2. Outbound HTTPS (decision of ADR-0009 §9: option b)

`net::HttpsClient` implements `social::HttpClient` with rustls 0.23 on the
**ring** provider (no aws-lc, no OpenSSL, no system libraries) and trusts the
Mozilla CA set from `webpki-roots`, plus PEM anchors from
`AuthConfig.tls_ca_file` (corporate CA, egress proxy). It is installed by
default in `AuthService::open` (cargo feature `https`, on by default), so the
seven OAuth providers, Turnstile siteverify and SMS gateways reach real
`https://` endpoints. Certificates are always verified (chain, validity,
name); there is no switch to turn verification off. TLS 1.2/1.3, ALPN
`http/1.1`. Plain `http://` stays available for loopback mocks and an
operator's TLS-terminating egress proxy.

`net::http1` frames responses by `Transfer-Encoding: chunked`,
`Content-Length` or close (a server that ignores `Connection: close` cannot
stall a request), caps headers at 64 KiB and bodies at 1 MiB, refuses header
injection, does not follow redirects, and treats a TLS close without
`close_notify` as end of stream. The whole request has one timeout (10 s).

Licences: rustls `Apache-2.0 OR ISC OR MIT`, ring `Apache-2.0 AND ISC`,
tokio-rustls `MIT OR Apache-2.0`, rustls-webpki `ISC`, untrusted `ISC`,
webpki-roots `CDLA-Permissive-2.0` (the CA list is data; allowed in
`deny.toml` as an exception for that crate only).

## 3. Realm-scoped identity

- **A realm is a user pool.** `_forgebase_realm_identity (realm_id,
  collection UNIQUE, config)` names the auth collection of each realm;
  `default` → `users` (or the configured default collection). Auth
  collections no realm names belong to `default`. Every auth row is keyed by
  collection (sessions, refresh tokens, one-time tokens, MFA, PATs, provider
  identities, passkeys, phones, SMS codes, lockout keys), so they are all
  realm-scoped without new columns; `_forgebase_oauth_identities`,
  `_forgebase_webauthn_credentials` and `_forgebase_oauth_states` also record
  `realm` explicitly. The same email (or phone number, or provider account)
  in two realms is two users.
- **Selection.** `X-ForgeBase-Realm` (validated slug; anything else means "no
  such realm") or `?realm=` on the browser OAuth start. Sign-in flows use the
  realm's collection; a `collection` of another realm is `404`. OAuth state
  rows carry the realm, so the provider callback needs no header.
- **Home realm.** `RealmDirectory` (collection → realm, service account →
  realm; one shared map, updated on writes) is attached to the policy engine
  (`PolicyService::attach_realms`). `engine::home_realm(snap, ctx)` is the
  identity's realm for users and service accounts, and the selected realm for
  anonymous callers; without a directory every identity is `default` (the old
  behaviour). The auth layer puts a signed-in caller in its home realm; the
  tenancy layer still refuses a foreign `X-ForgeBase-Realm` (ADR-0008 §3).
- **Service accounts** get `realm` (default `default`, set from the header at
  creation); names stay unique platform-wide.
- **Configuration** (`PUT /api/auth/realms/{realm}`, administrators): the
  realm must exist (`POST /api/v1/realms`); the collection (default
  `<realm>_users`) is created when missing, must be empty or new (so default
  users never move realms), and is fixed once set (`409`). Config: `signup`,
  `methods {password, magic_link, email_otp, oauth, passkey, phone, ldap}`
  (phone and LDAP off by default), `require_verified_email` override,
  `phone.signup`, `ldap`, `turnstile`. Secrets (LDAP bind password, Turnstile
  secret) are write-only: shown as `********`, kept when the mask is sent
  back, cleared by `""`, sealed with XChaCha20-Poly1305 under the master key
  (AAD = realm + path) at rest. `GET /api/auth/realm` is the public view for
  login pages (methods, Turnstile site key, providers). The audit log gets a
  `realm` column.
- Providers: the registry was already keyed by realm; entries of
  `oauth-providers.json` may now carry `"realm"`. A DB store for providers is
  FB-27's.

## 4. Phone number + SMS codes

`phone/request {phone}` → 6-digit code by SMS when the number belongs to an
account of the realm, or when the realm allows phone signup; the answer is
`202` either way. `phone/verify {phone, code}` signs in (`auth_method = sms`,
MFA challenge when enabled), creating the account on first use with a
placeholder email `phone.<keyed hash>@phone.invalid` (the number cannot be
read back from it). Signed-in users manage their number with `phone/add`,
`phone/confirm` (the code proves possession; a number taken in the realm is
`409`), `GET phone`, `DELETE phone` (refused when it is the only way in).
Numbers are E.164, unique per user pool (`_forgebase_auth_phones`). Codes:
HMAC under the pepper bound to collection + number + purpose, single use,
`email_otp_ttl`, burnt after `otp_max_attempts`, failures count towards the
per-number lockout; sends limited per IP (`ip_mail_per_hour`) and per number
(5/hour).

`SmsProvider` trait; `DevSmsProvider` (memory + 0600 files in
`<data_dir>/sms/`, the default), `TwilioSms` (Messages API, basic auth) and
`WebhookSms` (JSON POST, bearer token), both over the HTTPS client. Chosen by
`<data_dir>/auth/sms.json` or `AuthService::set_sms_provider`. Providers never
log the text; logs mask the number.

## 5. LDAP

Pure-Rust LDAPv3 client (own BER subset, ~600 lines with tests; the `ldap3`
crate would bring native-tls/OpenSSL defaults and a larger tree): connect
(`ldap://`, StartTLS, or `ldaps://`; TLS verified against webpki-roots +
`tls_ca_file` + the realm's `ca_pem`), optional service-account bind, search
`base_dn` with `user_filter` (`{username}` RFC 4515-escaped before
substitution: no filter injection), exactly one entry required (size limit 2),
then bind as the entry's DN with the user's password. An empty password is
refused before any traffic (an LDAP "unauthenticated bind" would succeed).
Unknown user, ambiguous match and wrong password are one `invalid_credentials`
(lockout per realm + username); network/TLS/protocol/service-bind failures are
`502 provider_error`. `ldap://` without StartTLS is refused unless
`insecure_plaintext`. The directory account links through
`_forgebase_oauth_identities` (provider `ldap`, subject `id_attribute` such as
`entryUUID`, else the DN) with the ADR-0009 §5 rules: an existing local
account with the same email is linked only when the directory email is trusted
(`trust_email`), the local email verified and `auto_link_verified_email` on;
otherwise `409`. New accounts use the directory email (verified when trusted)
or a placeholder `@ldap.invalid`. Referrals are not followed; no SASL.

## 6. Cloudflare Turnstile

Off unless a realm enables it (`site_key`, `secret`, `actions`, `hostnames`,
`verify_url`). Protected actions: `signup`, `login` (password and LDAP),
`password_reset` (request), `otp` (email code, magic link, SMS code). The
token comes in `cf-turnstile-response` or `x-turnstile-token`; siteverify gets
`secret`, `response`, `remoteip`, `idempotency_key`. Refused (`403
turnstile_failed`, audited `turnstile`): missing token, `success:false`, a
hostname outside `hostnames`, a widget `action` naming another action.
Siteverify unreachable or non-200: `502` (fail closed). The check runs before
any account lookup.

## 7. Account lifecycle

| State | Where | Effect |
|---|---|---|
| suspended | `_forgebase_suspensions` (shared with the policy engine and `/api/v1/suspensions`) | sessions revoked; sign-in, refresh, PATs refused (`403 account_suspended`); policy step 1 denies; `until` honoured |
| deactivated | `_forgebase_auth_account_status` | sessions revoked; sign-in, refresh, PATs refused (`403 account_deactivated`); administrator reactivates |
| anonymised | `_forgebase_auth_account_status` | email replaced (`anonymised.<random>@anonymised.invalid`), password and `token_key` random, optional custom fields nulled (required ones reported as `kept_fields`), every credential and auth row removed, audit IP/user agent cleared; id kept so references stay valid; never signs in again |
| deleted | — | record deleted, every auth row removed, audit IP/user agent cleared (events kept) |

The check sits in `issue_session` (every sign-in method, impersonation),
`complete_first_factor`, `refresh` (revokes the session) and API-key/PAT
authentication. Suspensions written through the auth API run a change hook
that reloads the policy model. Self-service (`account/deactivate`,
`account/delete {anonymise?}`, `account/export`) needs the current password
or a sign-in within 10 minutes; administrators use
`users/{collection}/{id}/{status|suspend|unsuspend|deactivate|reactivate|anonymise|delete|export}`.
Export (`forgebase.account-export.v1`): record (no password/token_key),
status, phone, linked identities, passkeys, sessions, PATs, MFA state, audit.

## 8. Impersonation (ADR-0014)

Refused in an impersonated session (`403 forbidden`, `impersonated_session:`
message, like the FB-23 guards): OAuth link/unlink, passkey
register/delete, phone add/confirm/remove, account deactivate/delete/
anonymise/export. Administrator lifecycle routes need an administrator
session, which an impersonation never is.

## 9. Routes (`/api/auth`)

`realm`, `realms`, `realms/{realm}` (GET, PUT), `phone/request`,
`phone/verify`, `phone` (GET, DELETE), `phone/add`, `phone/confirm`,
`ldap/login`, `account/deactivate`, `account/delete`, `account/export`,
`users/{collection}/{id}/…` (above). All in the OpenAPI fragment. Admin delete
is `POST …/delete` (FB-23 owns `GET /users/{collection}/{id}`).

## 10. Alternatives considered

- Realm column on every auth table and per-realm unique indexes in user
  collections: touches the data core's auth-collection schema and every query;
  one collection per realm gives the same isolation with no change there.
- `reqwest` + rustls: hyper, tower and a larger tree for six request shapes.
- `ldap3`: defaults to native-tls; StartTLS/LDAPS through rustls would still
  need glue; the needed subset is small and fully tested against a mock.
- Separate auth-level suspension table: two sources of truth with the policy
  engine; reusing `_forgebase_suspensions` keeps one.

## Consequences

- A realm's user pool cannot move to another collection; deleting a realm
  with an identity is refused by the foreign key.
- Policy bindings/memberships of a deleted user are not removed (principal
  keys hold no personal data); follow-up for FB-23/FB-27.
- Provider configuration is still process-local (file or API); FB-27 adds the
  store and UI.
