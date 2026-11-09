# ADR-0019: ForgeBase as identity provider (OAuth 2.0 authorization server, OpenID Connect)

- Status: accepted
- Date: 2026-11-09
- Work package: FB-26 (MDF-47)
- Builds on: ADR-0004 (sessions, refresh-token families, key management,
  CSRF, audit), ADR-0009 (ID-token validation on the client side), ADR-0014
  (impersonation), ADR-0015 (realms as user pools), requirements §E/§F.
- Out of scope: the identity studio UI for Client Apps (FB-27), hosted login
  and consent pages with branding (FB-74), front- and back-channel logout,
  dynamic client registration, pushed authorization requests, JWT-secured
  authorization requests, `private_key_jwt` client authentication.

## 1. Layout

All in `forgebase-auth::idp`: `clients.rs` (Client Apps, client
authentication), `keys.rs` (ES256 signing keys, JWKS), `flow.rs` (authorization
endpoint, single sign-on session, consent, device verification, RP-initiated
logout), `tokens.rs` (token endpoint, ID tokens, userinfo, introspection,
revocation), `http.rs` (routes and the built-in pages). Migration
`0018_oidc_provider.sql`. Settings in `AuthConfig.idp` (`IdpConfig`).

## 2. Issuer and endpoints (per realm)

The issuer of realm `R` is `<public_url>/api/auth/oidc/R`. Each realm is its
own OpenID Provider: its own clients, user pool (ADR-0015), signing keys and
session.

| Endpoint | Path under the issuer |
|---|---|
| Discovery (OIDC Discovery 1.0) | `/.well-known/openid-configuration` |
| JWKS | `/jwks` |
| Authorization (GET, POST) | `/authorize` |
| Consent decision (POST) | `/consent` |
| Token | `/token` |
| UserInfo (GET, POST) | `/userinfo` |
| Introspection (RFC 7662) | `/introspect` |
| Revocation (RFC 7009) | `/revoke` |
| Device authorization (RFC 8628) | `/device_authorization` |
| Device verification page (GET, POST) | `/device` |
| RP-initiated logout (GET, POST) | `/logout` |

Management: `/api/auth/clients` (GET, POST), `/api/auth/clients/{id}` (GET,
PATCH, DELETE), `/api/auth/clients/{id}/secret` (POST; administrators),
`/api/auth/consents` (GET), `/api/auth/consents/{id}` (DELETE; the signed-in
user), `/api/auth/keys/oidc/rotate` (POST; administrators). All in the
OpenAPI fragment.

**The auth layer resolves no credential on `/api/auth/oidc/*`.** Each
protocol endpoint authenticates on its own terms: the token-style endpoints
take client credentials (`Authorization: Basic` is not a ForgeBase bearer
token), userinfo takes an OAuth access token, and the browser endpoints read
the `fb_session` cookie and check a CSRF token sent as a form field (an HTML
form cannot set `X-CSRF-Token`).

## 3. Client Apps

Per realm, `confidential` (server; secret) or `public` (SPA, native, device;
no secret, PKCE required). `client_id` is `fbc_<24 random [a-z0-9]>`.

- **Secrets**: `fbcs_<32 random bytes, base64url>`, shown once, stored as
  HMAC-SHA256 under the pepper (high-entropy secret: no Argon2 needed),
  compared in constant time. **Rotation** issues a new secret; the previous
  one keeps working for `grace_seconds` (0 to 30 days, default 0).
- **Redirect URIs**: an allow list compared **byte for byte** (no
  normalisation, no wildcard, no port or path variance). Registration
  requires an absolute URI without fragment or user info: `https`, `http` on
  `localhost`/`127.0.0.1`/`[::1]` only, or a private reverse-domain scheme
  (`com.example.app:/cb`, RFC 8252) for public clients. Same rules for
  `post_logout_redirect_uris`.
- **Grants**: a subset of `authorization_code`, `refresh_token`,
  `client_credentials` (confidential only), and
  `urn:ietf:params:oauth:grant-type:device_code`.
- **Scopes**: `openid`, `profile`, `email`, `phone`, `offline_access`, and
  ForgeBase API scopes (`<area>:read|write[:<name>]`, ADR-0004 §10).
- `skip_consent` marks first-party apps; per-client token lifetimes; a
  disabled client gets no new authorization or token; deleting a client
  removes its consents, codes and token families.
- Client authentication: `client_secret_basic` (form-decoded, RFC 6749
  §2.3.1) or `client_secret_post`, one method per request; public clients send
  only `client_id`. Unknown client, client of another realm, disabled client
  and wrong secret are one `invalid_client` (401, audited
  `oauth.client_auth`), rate limited per IP like other credential attempts.

## 4. Authorization endpoint

`response_type=code` only, `response_mode=query`. Parameters may not repeat.

1. `client_id` must name an enabled client **of this realm** and
   `redirect_uri` must be registered (exact match). Otherwise an error page
   (HTML, or JSON with `Accept: application/json`); **never a redirect**.
2. Every later error redirects to the client with `error`,
   `error_description`, `state` and `iss` (RFC 9207).
3. PKCE (RFC 7636): only `S256`; required for public clients; optional for
   confidential ones (a verifier without a challenge is refused at the token
   endpoint). `plain` is refused. `request` / `request_uri` objects are
   refused (`request_not_supported`).
4. **Session**: the realm's browser session is the ForgeBase cookie session
   of a user of that realm (`fb_session`). Sessions of another realm,
   administrators, revoked/expired/idle sessions and inactive accounts are no
   session. When there is none, or `prompt=login`, or the sign-in is older
   than `max_age`: with `prompt=none` → `login_required`; otherwise the
   request is saved (`_forgebase_oauth_requests`, 10 minutes) and the browser
   goes to `login_url?realm=R&return_to=<issuer>/authorize?request_id=…`
   (default `<public_url>/auth/login`; FB-74 hosts it). A resumed request
   needs a sign-in **newer than the request** when `prompt=login` or
   `max_age` asked for one.
5. **Consent** is needed unless the client is first-party (`skip_consent`)
   or a remembered consent covers every requested scope and
   `prompt≠consent`. With `prompt=none` → `consent_required`. In an
   **impersonated session** (ADR-0014) consent cannot be granted:
   `access_denied` (and the consent POST is `403 impersonated_session`).
   Otherwise the consent screen: built-in HTML (CSP `frame-ancestors 'none'`,
   `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`) or the same data
   as JSON for a custom page. The decision is a POST of `request_id`,
   `decision=allow|deny` and `csrf_token` (= the session's CSRF token, ADR-0004
   §9), bound to the session the request was shown to.
6. The code: 32 random bytes, HMAC-stored, single use, 60 s, bound to client,
   redirect URI, scopes, nonce, PKCE challenge, user, session, `auth_time`,
   `acr`, `amr`.

**Cross-app SSO** follows: every client of a realm reuses the one session, so
the user signs in once; each client still needs its own consent.

## 5. Token endpoint and token families

Every grant creates a **token family** (`_forgebase_oauth_families`): realm,
client, user (none for client credentials), scopes, browser session,
`auth_time`, `acr`, `amr`, impersonator, absolute expiry, revocation.

- **Authorization code**: the code is claimed with `UPDATE … WHERE used_at
  IS NULL`; a second redemption is refused **and revokes the family the first
  redemption created** (`code_replay`, audited). After the claim the code is
  burnt whatever the outcome (expired, wrong `redirect_uri`, failed PKCE).
- **Refresh tokens**: opaque, HMAC-stored, rotate on every use; the family's
  absolute lifetime does not move (30 days default). Presenting a used token
  revokes the family (`refresh_token_reuse`, audited). `scope` may narrow,
  never widen (checked before the token is used up).
- **Session binding**: a family bound to a browser session ends with that
  session (logout, revocation, password reset…) unless `offline_access` was
  granted; checked at refresh and introspection, and RP-initiated logout
  revokes them at once.
- **Client credentials**: confidential clients, ForgeBase API scopes only, no
  refresh token, no ID token, `sub` = `client_id`.
- **Device code**: see §7.

Answers carry `Cache-Control: no-store` and `Pragma: no-cache`; errors are
RFC 6749 §5.2 JSON (`invalid_client` adds `WWW-Authenticate`).

## 6. Tokens and keys

- **Signing**: ES256 (P-256, RFC 6979 deterministic signatures, `p256`
  already a dependency for WebAuthn) — smaller and faster than RSA and no new
  crate. One active key per realm, created on first use, rotated after 90
  days (or `POST /api/auth/keys/oidc/rotate`); retired keys stay in the JWKS
  until `max(access, id token lifetime) + 5 min`, then are deleted. Private
  scalars sealed with XChaCha20-Poly1305 under the master key (AAD
  `oidc:<kid>`), like HS256 keys (ADR-0004 §5). An unknown `kid` reloads the
  realm's keys once.
- **Access token**: JWT `typ: at+jwt` (RFC 9068) — `iss`, `sub`, `aud` =
  `client_id`, `client_id`, `scope`, `iat`, `exp`, `jti`, `fid` (family),
  `realm`, `auth_time`, `acr`, `sid`, and `act: {sub}` from an impersonated
  session. Lifetime 15 minutes (per-client override). Resource servers verify
  it with the JWKS; introspection additionally checks family revocation,
  single revocation (RFC 7009, `_forgebase_oauth_revoked_tokens` until
  expiry), the session binding and the account status.
- **ID token**: JWT `typ: JWT` — `iss`, `sub` (user id; public subject
  type), `aud`, `azp`, `iat`, `exp` (1 hour), `auth_time`, `nonce` (code
  flow), `acr` (`urn:forgebase:sfa` | `urn:forgebase:mfa`), `amr` (RFC 8176
  values where one fits: `pwd`, `otp`, `sms`, `hwk`, `fed`, plus `mfa`),
  `at_hash`, `sid`, and the claims of the granted scopes.
- `sid` is a keyed hash of the ForgeBase session id (the session id itself
  never leaves the server).
- **UserInfo**: needs an active access token with `openid`; returns `sub` and
  the claims of its scopes: `email`, `email_verified` (email); `name`,
  `preferred_username`, `picture`, `updated_at` from the user record fields
  `name`, `username`, `avatar`, `updated` (profile); `phone_number`,
  `phone_number_verified` (phone, ADR-0015 §4).
- **Introspection**: confidential clients of the realm (resource servers
  register as confidential clients); unknown, expired, revoked, other-realm
  tokens are `{"active": false}`.
- **Revocation**: always `200`; a refresh token revokes its family, an access
  token is revoked alone; tokens of other clients are ignored.
- OAuth access tokens are **not** accepted by the ForgeBase REST API itself
  (`/api/v1`); they are for the client's own resource servers. Follow-up if
  wanted: map API scopes in OAuth access tokens to `Authenticated.scopes` in
  the auth layer.

## 7. Device authorization (RFC 8628)

`device_authorization` (client authentication as at the token endpoint;
grant must be allowed; `scope` within the client's) answers `device_code`
(32 random bytes), `user_code` (8 characters from `BCDFGHJKLMNPQRSTVWXZ`,
shown `XXXX-XXXX`, ~2.6·10¹⁰ values, both HMAC-stored), `verification_uri`
(`<issuer>/device`), `verification_uri_complete`, `expires_in` (600 s),
`interval` (5 s).

Polling: `authorization_pending` until the user answers; a poll sooner than
the interval is `slow_down` and adds 5 s to the interval (persisted);
`access_denied` after a denial; `expired_token` after expiry; the approved
code works once (then `invalid_grant`).

The verification page needs the realm session (else login redirect with
`return_to`), accepts the code with or without the dash and in any case,
shows client and scopes, and takes the decision as a CSRF-checked POST.
Lookups and decisions are rate limited per IP. The approval is also the
user's consent (remembered). Impersonated sessions cannot approve.

## 8. Consent and remembered grants

`_forgebase_oauth_consents (client, user) → scopes`, extended (union) on each
grant. `GET /api/auth/consents` lists the caller's; `DELETE
/api/auth/consents/{id}` removes one **and revokes every token family the
client holds for the user**, so the next authorization asks again.

## 9. Logout

`/logout` (OpenID Connect RP-Initiated Logout 1.0): `id_token_hint`
(signature verified with the realm's keys, expiry ignored), `client_id`,
`post_logout_redirect_uri` (exact match against the client's list; requires
the client to be known from the hint or `client_id`), `state`. A hint whose
`sid` is the current session ends it at once; otherwise the user confirms
(POST with the CSRF token). Ending revokes the ForgeBase session (every app
of the realm is signed out), revokes its non-`offline_access` families,
clears the cookies and redirects (with `state`) or shows a page. The plain
`/api/auth/logout` also ends the session; bound families then fail at refresh
and introspection.

## 10. Audit

`oauth.client.created|updated|deleted|secret_rotated`, `oauth.client_auth`
(failures), `oauth.authorize`, `oauth.consent.granted|denied|revoked`,
`oauth.token` (success and failure, with grant type), `oauth.code_replay`,
`oauth.refresh.reuse_detected`, `oauth.revoke`, `oauth.device.approved|denied`,
`oidc.logout`, `oidc.key_rotated`, `oidc.key_rotate_requested`. No secret in
any detail (the ADR-0004 scrubber also runs).

## 11. Alternatives considered

- **RS256**: universally supported, but needs an RSA signing implementation
  (the crate only verifies RSA) or a new dependency; ES256 is supported by
  every mainstream OIDC library and uses `p256`, already in the tree.
- **Opaque access tokens only**: simpler revocation, but every resource
  server would have to call introspection; JWT plus introspection for
  revocation-sensitive servers is the common middle.
- **Reusing `_forgebase_auth_sessions` as token families**: those rows are
  browser/device sessions with cookies and CSRF; OAuth families have a client,
  scopes and different lifetimes. A separate table with a session link keeps
  both models clear.
- **`plain` PKCE**: allowed by RFC 7636 but dropped by OAuth 2.1 and the
  security BCP (RFC 9700); refused.
- **Wildcard or normalised redirect URIs**: a source of open redirects and
  code theft; exact match only.

## Consequences

- An app that signs users in through ForgeBase needs a Client App row; the
  identity studio (FB-27) gets the management API to build on.
- A login page at `login_url` that signs in with `"cookie": true` and returns
  to `return_to` is needed for browser flows without a session (FB-74 hosts
  one; until then apps or operators provide it).
- The data model keeps all secrets hashed or sealed; a database leak alone
  cannot mint tokens or use codes, refresh tokens or client secrets.
