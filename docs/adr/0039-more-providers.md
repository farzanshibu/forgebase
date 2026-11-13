# ADR-0039: More email, SMS and login providers

- Status: accepted
- Date: 2026-11-14
- Scope: transactional email transports beside SMTP (Resend, Postmark,
  SendGrid, Mailgun, Amazon SES v2) behind the one `MailProvider` trait,
  chosen per realm or per instance with sealed settings; SMS gateways beside
  Twilio and the generic webhook (Vonage, MessageBird, Bird, Amazon SNS,
  Plivo); login provider presets on the existing OAuth/OIDC machinery
  (Microsoft Entra ID, GitLab, Slack, LinkedIn, Twitch, Bitbucket, Spotify,
  Keycloak) and Apple's first-login name.
- Builds on: [ADR-0004](0004-auth.md), [ADR-0009](0009-oauth-webauthn.md)
  (provider flows, account linking), [ADR-0015](0015-identity-extensions.md)
  (§2 outbound HTTPS and the SSRF guard, §4 SMS),
  [ADR-0020](0020-product-customization.md) (§4 mail templates, providers,
  the delivery log).
- Migrations: `0065_oauth_identity_avatar`, `0066_email_providers` (both
  backends).
- New dependencies: none. (`url` joins `forgebase-customize`; it was already a
  workspace dependency of `forgebase-auth`.)

## Inventory before this change

| Area | What existed |
|---|---|
| Login providers | `google`, `github`, `apple` (ES256 client-secret JWT, `response_mode=form_post`, POST callback), `facebook`, `wechat`, `discord`, generic `oidc` (issuer discovery or explicit endpoints, JWKS-checked RS256/ES256 ID tokens, PKCE, state, nonce), configured in `oauth-providers.json` or over REST with sealed secrets. |
| SMS | `SmsProvider` (`send -> Result<(), String>`): `dev`, `twilio`, generic `webhook`; per-realm gateways over REST with sealed secrets; no E.164 re-check at the gateway, no retries, no message ids. |
| Email | `MailProvider` (`send -> Result<(), SendError>`): `log` and an own `smtp` client. The server used the log provider unless an embedder called `set_mail_provider`: **no configuration selected SMTP**, and there was no send-test path. No `Reply-To`, no message id. The auth crate's own mails (verification, reset) still use `forgebase_auth::Mailer` (ADR-0020 §7), which this ADR does not change. |

## Decisions

### 1. Email: one trait, many transports, configured at runtime

`MailProvider::send` now returns `Result<SendReceipt, SendError>`; the receipt
carries the provider's message id, which the delivery log stores
(`provider_message_id`, shown in the delivery REST view). `OutMail` gains
`reply_to`, filled from the realm's `support_email` when it is a valid address
(SMTP adds a `Reply-To` header, the HTTP transports their own field). The mail
model has no attachments or custom headers, so transports have none either.

| Transport | Request | Id | Notes |
|---|---|---|---|
| `resend` | `POST /emails`, bearer key | `id` | `Idempotency-Key` = delivery id |
| `postmark` | `POST /email`, `X-Postmark-Server-Token` | `MessageID` | `message_stream`; an `ErrorCode != 0` is a refusal even on a 200 |
| `sendgrid` | `POST /v3/mail/send`, bearer key | `X-Message-Id` header | `region`: `global` / `eu` host; `text/plain` before `text/html`; tracking off |
| `mailgun` | `POST /v3/{domain}/messages`, Basic `api:KEY` (form) | `id` | `region`: `us` / `eu` host |
| `ses` | `POST /v2/email/outbound-emails`, SigV4 service `ses` | `MessageId` | `configuration_set`, optional session token |

Error mapping, the same for all: 2xx success; 408/425/429/5xx and connection
failures `Transient` (the queue retries with exponential backoff up to
`email_max_attempts`; a 429 says `retry after Ns`); 401/403 `Permanent`
("refused the credentials"); other 4xx `Permanent` with the provider's own
reason (excerpted, control characters removed). SES throttling that arrives
as a 400 (`ThrottlingException`, `TooManyRequestsException`) is `Transient`.
Retries stay with the queue so a message is never retried in two places; the
duplicate window of a lost answer is closed where the provider supports it
(Resend).

**Selection.** A configuration is JSON with `provider`, the provider's
settings and an optional `from_address`; `smtp` and `log` are selectable the
same way, which also makes SMTP configurable without an embedder for the first
time. Rows live in `_forgebase_email_providers` (migration 0066): one per
realm, and `realm = '*'` for the instance. Resolution at send time: the
realm's row, else the instance's, else the provider installed with
`set_mail_provider` (the log provider by default). Secret fields (`api_key`,
`server_token`, `password`, `secret_access_key`, `session_token`) are sealed
under the master key (`sealed:` + base64url, AAD `mail:{realm}:provider:{field}`),
returned as `********`, kept when that mask or nothing is sent back for the
same provider type, and dropped when the type changes. A stored row that no
longer opens (another master key) or builds is skipped with a warning and the
next level applies. Only platform administrators read or change them
(`GET/PUT/DELETE /api/v1/customize/email/provider?realm=`), because
`base_url`/`endpoint` overrides name hosts the server will contact. The calls
go through the auth service's HTTPS client, so the SSRF guard applies; set
`[auth] allow_private_outbound` for an on-network relay.

**Test send.** `POST /api/v1/customize/email/provider/test?realm=` sends one
message now (not queued, not in the delivery log) through the provider that
realm would use and returns `{provider, message_id, from}`, or `409
mail_test_failed` with the provider's reason.

### 2. SigV4 in `forgebase-auth`

SES and SNS need SigV4; the S3 driver's signer lives in `forgebase-storage`,
which depends on `forgebase-auth`, so neither crate here can use it
(`forgebase-storage` was out of scope). `forgebase_auth::sigv4` carries the
same algorithm (copied, plus `sha256_hex`), with one change: the
`x-amz-content-sha256` header is only sent and signed for `s3`. Tests pin it to
two public AWS Signature Version 4 test-suite vectors (`get-vanilla`,
`post-x-www-form-urlencoded`) and the S3 documentation's GET Object example.
Follow-up: `forgebase-storage` can switch to it and drop its copy.

### 3. SMS: typed errors, ids, E.164, retries

`SmsProvider::send` returns `Result<SmsReceipt, SmsError>`. `SmsError` is
`InvalidRecipient`, `Auth`, `RateLimited { retry_after_secs }`, `Transient` or
`Rejected` (`Display` text is log-safe: no message text, no secrets).
Every HTTP gateway (including Twilio and the webhook, which gain this)
rejects a non-E.164 `to` before any request, and retries 429 and 5xx up to
three attempts, waiting for the gateway's `Retry-After` or a doubling backoff,
both capped at two seconds (a login request is waiting). When throttling
outlasts the retries, the phone flows answer `429 too_many_requests` with
`Retry-After`; every other gateway failure stays the generic `502
provider_error`.

| Provider | Request | Id |
|---|---|---|
| `vonage` | `POST /v1/messages` (Messages API), Basic key/secret, number without `+` | `message_uuid` |
| `messagebird` | `POST /messages`, `AccessKey`; error codes 2/98 auth, 9/21 recipient, 25 throttle | `id` |
| `bird` | `POST /workspaces/{ws}/channels/{ch}/messages`, `AccessKey` | `id` |
| `sns` | Query API `Publish` (form), SigV4 service `sns`; `sms_type`, `sender_id`; error `Code`s mapped | `MessageId` |
| `plivo` | `POST /v1/Account/{auth_id}/Message/`, Basic | `message_uuid[0]` |

Configuration is `sms.json` or the realm SMS REST route (same sealed
secrets: `api_key`, `api_secret`, `secret_access_key`, `session_token` join
`auth_token` and `bearer_token`).

### 4. Login presets on the existing flow

No second flow: presets are `ProviderKind`s whose endpoints, scopes, claim
mapping and email trust are decided in `social::registry` / `social::flow`.

| Kind | Protocol | Endpoints | `email_verified` |
|---|---|---|---|
| `microsoft` | OIDC | `{base_url}/{tenant}/oauth2/v2.0/…`, JWKS `…/discovery/v2.0/keys`, Graph `oidc/userinfo` | **only the optional `xms_edov` claim**; the `email` claim is not proof (see below) |
| `gitlab` | OIDC | `{base_url}` (default `https://gitlab.com`) `/oauth/authorize`, `/token`, `/discovery/keys`, `/userinfo`; issuer = `base_url` | the `email_verified` claim (GitLab: confirmed address) |
| `slack` | OIDC | `slack.com/openid/connect/…`, `openid.connect.token`, `openid.connect.userInfo` | the claim |
| `linkedin` | OIDC | `linkedin.com/oauth/v2/…`, `/oauth/openid/jwks`, `api.linkedin.com/v2/userinfo` | the claim |
| `twitch` | OIDC | `id.twitch.tv/oauth2/…`; the `claims` parameter asks for `email`, `email_verified`, `preferred_username`, `picture` | the claim; name from `preferred_username` |
| `bitbucket` | OAuth 2 | token endpoint with HTTP Basic client auth, no PKCE; `/2.0/user`, `/2.0/user/emails` | the **primary and confirmed** address only |
| `spotify` | OAuth 2 | HTTP Basic client auth; `/v1/me` | **never** (Spotify does not confirm the address) |
| `keycloak` | OIDC discovery | `issuer = {base_url}/realms/{tenant}` | the claim |
| `discord` (existing) | OAuth 2 | + avatar URL from the CDN hash | `verified` |

The `apple`, `google`, `github`, `facebook`, `wechat` and generic `oidc` kinds
are unchanged except as noted below. Extra config fields: `tenant`, `base_url`,
`allowed_tenants`, `authorize_params` (free parameters such as `prompt`, Slack
`team`; the protocol parameters are reserved and refused).

**Microsoft Entra.** `tenant` is `common` (default), `organizations`,
`consumers` or a tenant GUID; a tenant domain name is refused (its issuer is
a GUID we cannot know without a lookup). A GUID pins the issuer. For the
multi-tenant aliases the issuer is a template: `iss` must equal
`{base_url}/{tid}/v2.0` with the token's own `tid` (all tenants sign with the
same keys, so without this check any tenant's token for any `iss` would pass);
`organizations` also refuses the personal-account tenant
(`9188040d-6c67-4c5b-b112-36a304b66dad`), `consumers` accepts only it, and
`allowed_tenants` limits the accepted tenants. The identity key is
`tid:oid`, never the email or the per-app `sub`. The `email` claim is
user-controlled in Entra (the "nOAuth" takeover): it is trusted for account
linking only when the tenant's `xms_edov` optional claim says the domain owner
verified it; otherwise the sign-in creates a placeholder-email account and
never links.

**Apple.** The `user` form field of the first `form_post` callback (JSON with
`name.firstName` / `lastName`) is read for the display name only (control
characters removed, 100 characters, invalid JSON ignored); it is client
supplied, so its `email` is never used. Later sign-ins carry no `user` and
keep the stored name. A private-relay address with `email_verified` (a boolean
or the string `"true"`) is a verified, forwarding mailbox. The client secret
remains a five-minute ES256 JWT per token request.

**Avatar.** Providers' `picture` / avatar URLs are kept when `https`, free of
credentials and at most 2048 bytes, in `_forgebase_oauth_identities.avatar_url`
(migration 0065), refreshed at each sign-in and shown to the identity's owner
(`GET /api/auth/oauth/identities`). Nothing decides on it.

**Linking** is unchanged (ADR-0009 §4): an existing local account is linked
only when the provider vouched for the address, the local account is verified
and `auto_link_verified_email` is on; otherwise `409`. An unverified provider
address never becomes the account's email.

## Consequences and limits

- `MailProvider` and `SmsProvider` are breaking trait changes for embedders
  that implement them (return a receipt); in-tree users were updated.
- The auth crate's own mails still go through `Mailer`, not these transports
  (ADR-0020 §7): a `QueuedMailer` adapter is the follow-up that makes
  verification and reset mails use the realm's provider.
- No attachments or custom headers (the model has none). No inbound webhooks
  (bounces, complaints) from the providers.
- Retries for email stay in the queue; for SMS they are inline and short.
- Microsoft tenant domain names, sovereign-cloud Graph userinfo URLs (set
  `userinfo_url`), Okta/Auth0 presets (use the generic `oidc` kind) and Sign
  in with Apple's server-to-server notifications are not covered.
- The SES/SNS/Postmark/etc. endpoints were exercised against recorded
  request/response shapes in mock servers, not the live services.
- Dashboard: provider pickers for the new kinds, the email provider page and
  the SDK surfaces are follow-ups; `sdk/*/openapi.json` need regenerating for
  the new customize routes.
