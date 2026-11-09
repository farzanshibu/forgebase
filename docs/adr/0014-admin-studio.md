# ADR-0014: Admin sign-in, Auth Studio, Access Studio and impersonation

- Status: accepted
- Date: 2026-11-09
- Work package: FB-23 (MDF-23)
- Builds on: ADR-0004 (auth core, cookie sessions + CSRF), ADR-0008 (policy
  engine, `/api/v1` REST), FB-12 dashboard conventions.
- Out of scope: billing UI (FB-71), realm identity providers and branding
  (FB-27).

## 1. Sign-in

The dashboard signs administrators in with `POST /api/auth/admins/login
{email, password, cookie: true, device_name}`. The server answers with an
HttpOnly `fb_session` cookie plus a readable `fb_csrf` cookie and
`csrf_token` in the body. Every unsafe request carries `X-CSRF-Token` (value
of the `fb_csrf` cookie / body field); `request()` in `dashboard/src/api` adds
it. If the account has MFA the answer is `{mfa_required, mfa_token,
expires_in}` and the dashboard asks for a TOTP or recovery code
(`POST /api/auth/mfa/verify {mfa_token, code}`). Sign-out is `POST
/api/auth/logout` (clears the cookies). A 401 anywhere returns the user to the
sign-in screen with a "session expired" notice; the router state remembers the
page. `setTokenSource` (FB-12 hook) stays the single place a *bearer* token is
injected: the dashboard uses it only while an impersonation is active (§4).

## 2. Auth Studio — endpoints it uses

Existing (ADR-0004): `GET /api/auth/me`, `/admins`, `/service-accounts[/{id}[/keys]]`,
`/tokens`, `/api-keys/{id}` (DELETE), `/audit`, `DELETE /sessions/{id}` (an
administrator may revoke any session). Users are records of auth collections
(`CollectionDef.type == "auth"`), listed and searched through the data API
(`/api/v1/collections/{c}/records?search=&filter=`).

New admin-only routes (this card), all `{items:[…]}` lists unless stated:

| Route | Result |
|---|---|
| `GET /api/auth/users/{collection}/{id}` | `{user, mfa_enabled, active_sessions, last_login_at}` |
| `GET /api/auth/users/{collection}/{id}/sessions` | `SessionView[]` (`id, auth_method, mfa, cookie, ip, user_agent, device_name, created_at, last_seen_at, expires_at, impersonator`) |
| `POST /api/auth/users/{collection}/{id}/sessions/revoke` | `{revoked: n}` (all sessions) |
| `GET /api/auth/users/{collection}/{id}/login-history?limit=` | `AuditEntry[]` |
| `GET /api/auth/api-keys` | every API key and PAT (`ApiKeyView`, includes `revoked`) |
| `GET /api/auth/audit?event=&subject_id=&before=&limit=` | `AuditEntry` now has `impersonator: string \| null` |
| `POST /api/auth/impersonate` | see §4 |
| `GET /api/auth/impersonations` | `{items: [{session_id, admin_id, target, reason, created_at, expires_at, ended_at, active}]}` |
| `POST /api/auth/impersonations/{session_id}/end` | 204 |

Suspension is FB-21: `POST /api/v1/suspensions {principal, reason, until}` and
`/suspensions/lift`; new `GET /api/v1/suspensions` lists active ones
(`{principal, reason, until, created_at, created_by}`).

The authorization audit viewer reads `GET /api/v1/authz/audit?limit=` rows
`{at, realm_id, principal, tenant, org_id, permission, resource, step,
reason, request_id, impersonator}`; `step` is the pipeline step that denied
(`authentication`, `tenant`, `permission`, `entitlement`, `resource`, `quota`).

## 3. Access Studio — endpoints it uses

All of FB-21's REST (`/api/v1/realms`, `orgs…`, `projects…`, members, teams,
roles, role-sets, realm-bindings, overrides, acl, policies, invitations,
`authz/me`, `authz/check`), plus new read endpoints the editors need (all
read-only, same permission as the matching write):

| Route | Result |
|---|---|
| `GET /api/v1/role-sets`, `GET /api/v1/orgs/{org}/role-sets` | `[{id, realm_id, org_id, key, name, roles}]` |
| `GET /api/v1/realm-bindings` | `[{principal, roles, role_sets, …}]` (super-admins) |
| `GET /api/v1/overrides?org=` | `[{id, realm_id, org_id, principal, permission, effect, …}]` |
| `GET /api/v1/suspensions` | see §2 |
| `GET /api/v1/orgs/{org}/invitations` | `[{id, email, roles, status, expires_at, …}]` (never the token) |

`POST /api/v1/authz/check` accepts an optional `principal`
(`user:<collection>/<id>`; super-admins only) so the rule/permission tester can
answer "would *this user* be allowed?"; the answer is
`{allowed, step?, reason?}`.

The permission-matrix editor edits a role's `allow` / `deny` pattern lists over
the catalogue of `resource:action` keys (the dashboard ships the catalogue
from ADR-0008 §5; patterns outside it are kept and shown as "custom").

## 4. Impersonation

`POST /api/auth/impersonate {principal: "user:<collection>/<id>", reason,
ttl_minutes?}` (default 15, 1–60):

- caller must be a human administrator signed in with an unrestricted
  session (not an API key or PAT, not itself impersonated, MFA-passed when the
  admin has MFA); anything else is `403`;
- `reason` is required (3–500 chars);
- the target must be an existing user record; administrators and service
  accounts (`admin:…`, `service:…`) are refused with `403
  cannot_impersonate_admin` — one super-admin never acts as another;
- the result is a normal user session row with `impersonator = <admin id>`, a
  short expiry, no refresh token, no cookie, `mfa=false`; the response is
  `{access_token, token_type, expires_in, expires_at, session_id,
  impersonator, subject, reason}`; the bearer token is the only credential;
- `AccessContext.impersonator` (new field, `forgebase-data`) carries the admin
  id; every auth audit row (`_forgebase_auth_audit.impersonator`) and every
  authorization audit row (`_forgebase_authz_audit.impersonator`) written
  while it is set records it; `impersonation.start` / `impersonation.end`
  are audited with the reason;
- no escalation: the context is the target user's (not privileged); an
  impersonated session cannot change passwords, manage MFA, create PATs or API
  keys, impersonate, or be refreshed; it ends at expiry, by
  `POST /api/auth/logout` (the dashboard's "End impersonation") or by an admin
  (`…/impersonations/{id}/end`), and revocation is immediate.

The dashboard keeps the impersonation token in memory only, shows a
non-dismissable banner (target, reason, time left, **End impersonation**)
and uses it as the bearer credential until ended.

## 5. Rust changes (list for god)

| Crate | Change |
|---|---|
| `forgebase-data` | `AccessContext.impersonator: Option<String>` + `with_impersonator()` (additive; no struct literal in the workspace) |
| `forgebase-auth` | migration **0013** (`sessions.impersonator`, `audit.impersonator`, `authz_audit.impersonator`, `_forgebase_impersonations`); `impersonation.rs` (service + routes); admin user routes (§2); `ClientInfo.impersonator`; `Authenticated.impersonator`; guards on self-service credential changes |
| `forgebase-policy` | read endpoints (§3), `authz/check` `principal`, authz audit rows carry `impersonator` |
| `forgebase-api` | OpenAPI entries of the new policy routes |

## Consequences

- The cookie session means the browser never holds an access token except
  during an impersonation, which is deliberately short-lived and in memory.
- Revoking an impersonation or an admin session is immediate (sessions are
  checked on every request).
- Follow-ups: per-user suspension from the user detail uses FB-21's global
  suspension only; org-membership suspension stays in the Access Studio.
