# ADR-0009: Social login (OAuth 2.0 / OpenID Connect) and passkeys (WebAuthn)

- Status: accepted
- Date: 2026-11-09
- Work package: FB-22 (MDF-22)
- Builds on: ADR-0004 (auth core: `AuthService::issue_session`, MFA, audit,
  rate limits), ADR-0008 (realms), requirements §E/§F.
- Out of scope: realm-scoped identity, phone/LDAP (FB-25); ForgeBase as an
  identity provider (FB-26); the provider-management UI (FB-27).

## 1. Layout

Inside `forgebase-auth`: `social/` (registry, flow, ID-token validation,
signatures, outbound HTTP, routes) and `webauthn/` (ceremonies, a strict CBOR
reader, routes), plus `openapi.rs`. Migration `0008_oauth_webauthn.sql` adds
`_forgebase_oauth_states`, `_forgebase_oauth_identities`,
`_forgebase_webauthn_credentials`, `_forgebase_webauthn_challenges`. Every row
carries `realm` (default `default`).

## 2. Provider registry

`ProviderRegistry` is keyed by `(realm, provider id)`. FB-25 registers the same
ids under other realms; flows already read and write `realm`, so nothing else
changes. Providers come from `AuthService::providers().register(..)` or from
`<data_dir>/auth/oauth-providers.json` (`{"providers":[...]}`, loaded at start;
keep it 0600). `ProviderConfig` is `Deserialize` only and its secrets are
`Secret` values whose `Debug` is `[redacted]`; logs carry provider id and HTTP
status, never a body, code or token.

| Kind | Protocol | Identity |
|---|---|---|
| `google`, `apple`, `oidc` | code + PKCE (Apple: no PKCE, `form_post`, ES256 client secret from the `.p8` key), ID token verified | `sub` |
| `github` | code + PKCE, `/user`, `/user/emails` | numeric id; only a **primary, verified** address counts |
| `facebook` | code + PKCE, Graph `/me` | `id`; email never treated as verified |
| `discord` | code + PKCE, `/users/@me` | `id`; `verified` honoured |
| `wechat` | website QR login, GET token exchange returning `errcode` with HTTP 200, no PKCE | `unionid` if present, else `openid` |

Generic OIDC takes an issuer (discovery, issuer must match) or explicit
endpoints. Endpoints of the named kinds are built in and can be overridden
(also how tests reach the mock IdP).

## 3. The flow

`start`: random `state` (32 B), `nonce`, PKCE verifier; the state is stored as
HMAC under the pepper, the verifier sealed (XChaCha20-Poly1305, bound to the
row); 10 minute lifetime. `callback`: the state is claimed with `UPDATE ..
WHERE used_at IS NULL` (single use even under concurrency, and burnt by a
failed attempt), must belong to the provider named in the URL, then the code is
exchanged (with `redirect_uri`, `code_verifier`), the ID token validated, the
profile read, the account resolved, and `complete_first_factor` runs, so TOTP
and passkey-MFA users still get an MFA challenge and sessions come from
`issue_session` (`auth_method = oauth`). The callback accepts GET (browser),
POST JSON (SPA) and POST form (Apple). There is no cookie binding of `state`
to the browser (login-CSRF is limited to signing the victim into an account
the attacker controls; document if your app needs it).

## 4. ID token validation

`RS256` and `ES256` only (never `none`, never HMAC). Key by `kid` from the
JWKS, cached one hour; an unknown `kid` refetches at most every 30 s. Checks:
signature, `iss` in the provider's accepted set, `aud` contains our client id
(and `azp` equals it when `aud` has several), `exp` (60 s leeway), `iat` not in
the future, `nbf`, `nonce` equal, `sub` present. RSA keys below 2048 bits are
refused.

## 5. Account linking rules

1. Provider account already linked: sign in as that user.
2. Not linked, a user has the same email: link **only if** the provider says
   the email is verified **and** the local account's email is verified **and**
   `auto_link_verified_email` is on; otherwise `409 conflict` and the user must
   sign in and link explicitly. (A pre-registered unverified account cannot be
   taken over through a provider.)
3. Else sign up (if `allow_signup`): a verified provider email becomes the
   account email; anything else becomes the placeholder
   `<provider>.<hash>@oauth.invalid` (verified = false), so nobody can squat
   an address the provider did not prove. The random password is unknown to
   everyone; password reset works once a real email is added.
4. Explicit link (`POST /oauth/{p}/link`, signed-in user) never merges by
   email; a provider account bound to another user is `409`.
5. Unlink is refused when the account has a placeholder email and nothing else
   linked (no passkey, no other provider).

## 6. WebAuthn

Pure Rust. ES256 and RS256 credentials; `attestation: "none"`; the statement
is **not** verified (so authenticator models are not trusted; AAGUID is
ignored). Ceremony ids are random, stored as HMAC, single use, 5 minutes. JSON
shapes follow `...OptionsJSON` / `...ResponseJSON` (base64url).
Checks: `clientDataJSON` type, challenge, origin (from `public_url`; RP id =
its host) and `crossOrigin`; `rpIdHash`; user presence; credential id equals
the one in the attested data; COSE key parses (P-256, or RSA >= 2048 bit).
Authentication also verifies the signature over `authData || SHA-256(clientData)`,
the user handle, and the **sign count**: if either counter is non-zero the new
one must be greater (clone suspicion: rejected and audited
`webauthn.clone_suspected`); the update is compare-and-set so two concurrent
assertions with one counter cannot both pass. Synced passkeys that always send
0 are accepted.

- **First factor**: usernameless (discoverable credentials) or with `email`
  (an unknown email gets a stable fake credential id, so options do not reveal
  accounts). User verification is required, and the session is `mfa = true`
  (possession + inherence/knowledge), so TOTP users are not asked again.
- **Second factor**: credentials registered with `use_as_mfa` make
  `mfa_enabled` true; after a first factor the client calls
  `/webauthn/mfa/options` and `/webauthn/mfa/verify` with the `mfa_token`;
  the ceremony is bound to that challenge (HMAC in its detail); user
  presence suffices.
- Passkeys are listed and deleted by their owner; deleting the last way in of
  a placeholder-email account is refused.

## 7. Routes (`/api/auth`)

`oauth/providers`, `oauth/{provider}/start` (GET redirect, POST JSON),
`oauth/{provider}/link`, `oauth/{provider}/callback` (GET, POST),
`oauth/identities` (+ `DELETE /{id}`), `webauthn/register/{options,verify}`,
`webauthn/login/{options,verify}`, `webauthn/mfa/{options,verify}`,
`webauthn/credentials` (+ `DELETE /{id}`). All in the OpenAPI document when
auth is on (`forgebase_auth::openapi()`, merged by `forgebase-api`). Per-IP
attempt limits of ADR-0004 apply to start, callback and every ceremony step.

## 8. Audit

`oauth.linked` (how: `verified_email` / `signup` / `explicit`), `oauth.unlinked`,
`oauth.login` (failures: reason code only), `webauthn.registered|deleted`,
`webauthn.login` (failure reasons), `webauthn.clone_suspected`, plus the
existing `login`, `signup`, `mfa.verify` (factor `passkey`).

## 9. Outbound HTTPS: decision needed

The tree has no HTTP client and no TLS stack (ADR-0002 deliberately avoided
reqwest: rustls pulls aws-lc-rs or ring, C code). Provider calls go through
the `HttpClient` trait. The built-in `PlainHttpClient` speaks HTTP/1.1 over
TCP with timeouts and a 1 MiB cap and **refuses `https://`** with a clear
error (never a silent downgrade). Real providers are HTTPS, so production
needs one of: (a) an egress proxy that terminates TLS, with provider URLs
overridden to it; (b) an `HttpClient` backed by `rustls` + `ring` (needs
god's approval for the C/asm dependency) injected with
`AuthService::set_http_client`. Recommendation: (b) in a small feature-gated
crate, as `rustls` with `ring` is MIT/Apache/ISC licensed and builds without
system libraries. Until then everything is proven against a mock IdP on
plain HTTP.

## 10. Alternatives considered

- `webauthn-rs`: pulls OpenSSL; rejected (the ceremonies are ~400 tested lines).
- `jsonwebtoken` / `rsa`: `ring`/`aws-lc` and the RUSTSEC-2023-0071 advisory
  respectively; verification-only RS256 is ~60 lines over `num-bigint`.
- `openidconnect` / `oauth2` crates: bring a `reqwest` client and TLS.

## Consequences

- Provider config is process-local until FB-25/27 give it a store and a UI.
- Attestation is not verified; deployments that need device attestation
  (FIDO MDS) need a later card.
- WeChat `openid` -> `unionid` upgrades change the identity key (a new
  account); configure the WeChat app under an Open Platform account.
