import type { AuthManager, SignInOptions, SignInResult } from "./auth";
import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Identity administration (`/api/auth/*` beyond sign-in): API keys, personal access tokens, service accounts,
 * OAuth / OIDC clients, realms, user lifecycle, impersonation, audit, MFA, phone, passkeys: `client.identity`.
 * The wire types the OpenAPI fixture leaves as `object` are written by hand and follow the server's JSON (snake_case).
 */

const seg = encodeURIComponent;

type Json = Record<string, unknown>;

// ---------------------------------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------------------------------

/** A created token or API key: `key` is the secret, shown once. */
export type CreatedApiKey = Schemas["AuthKeyCreated"];
export type TotpSetup = Schemas["AuthTotpSetup"];

/** An API key or personal access token, never its secret. */
export interface ApiKeyInfo {
  id: string;
  /** `api_key` or `pat`. */
  kind: string;
  name: string;
  owner_type: string;
  collection?: string | null;
  owner_id: string;
  scopes: string[];
  created_at: string;
  expires_at?: string | null;
  last_used_at?: string | null;
  revoked: boolean;
  /** Tenant (environment id) the key is bound to. */
  tenant?: string | null;
}

export interface ApiKeyOptions {
  name: string;
  /** `<area>:read|write[:<name>]`. */
  scopes: string[];
  expiresInDays?: number;
  /** Bind the key to one tenant: requests then act in it only. */
  tenant?: string;
}

export interface ServiceAccount {
  id: string;
  name: string;
  description: string;
  role: string;
  created_by: string;
  created_at: string;
  disabled: boolean;
  realm: string;
}

export interface AdminAccount {
  id: string;
  email: string;
  created_at: string;
  last_login_at?: string | null;
}

/** One session of a user (own, or of another user through `identity.users.sessions`). */
export interface SessionInfo {
  id: string;
  auth_method: string;
  mfa: boolean;
  cookie: boolean;
  ip?: string | null;
  user_agent?: string | null;
  device_name?: string | null;
  created_at: string;
  last_seen_at: string;
  expires_at: string;
  current: boolean;
  impersonator?: string | null;
}

export interface AuthAuditEntry {
  id: number;
  at: string;
  event: string;
  outcome: string;
  actor: string;
  subject_type?: string | null;
  collection?: string | null;
  subject_id?: string | null;
  ip?: string | null;
  user_agent?: string | null;
  request_id?: string | null;
  detail: unknown;
}

export interface AuthAuditQuery {
  /** Event name, e.g. `login.failed`. */
  event?: string;
  subjectId?: string;
  /** Only rows with an id below this one (paging). */
  before?: number;
  /** 1 to 500 (default set by the server). */
  limit?: number;
}

export interface ImpersonationGrant {
  token_type: string;
  /** Access token acting as the target; use it with `new ForgeBase(url, { apiKey })`-style bearer clients. */
  access_token: string;
  expires_in: number;
  expires_at: string;
  session_id: string;
  impersonator: string;
  subject: unknown;
  reason: string;
}

export interface ImpersonationInfo {
  session_id: string;
  admin_id: string;
  target: string;
  reason: string;
  created_at: string;
  expires_at: string;
  ended_at?: string | null;
  active: boolean;
}

/** Lifecycle state of an account. */
export interface AccountStatus {
  /** `active`, `suspended`, `deactivated` or `anonymised`. */
  status: string;
  reason?: string | null;
  /** Suspension end (RFC 3339), if limited. */
  until?: string | null;
  changed_by?: string | null;
  changed_at?: string | null;
}

export type OAuthClientType = "confidential" | "public";

/** An OAuth / OIDC client app, never its secret. */
export interface OAuthClient {
  client_id: string;
  realm: string;
  name: string;
  description?: string | null;
  type: OAuthClientType;
  redirect_uris: string[];
  post_logout_redirect_uris: string[];
  grant_types: string[];
  scopes: string[];
  skip_consent: boolean;
  access_token_ttl?: number | null;
  refresh_token_ttl?: number | null;
  disabled: boolean;
  secret_created_at?: string | null;
  previous_secret_expires_at?: string | null;
  created_at: string;
  updated_at: string;
}

/** A created client, or a client with a rotated secret: `client_secret` is shown this once. */
export interface CreatedOAuthClient {
  client: OAuthClient;
  client_secret?: string;
}

export interface CreateClientInput {
  name: string;
  /** Realm (default: the request's realm). */
  realm?: string;
  description?: string;
  /** `confidential` (default) or `public`. */
  type?: OAuthClientType;
  /** Exact-match redirect URIs. */
  redirectUris?: string[];
  postLogoutRedirectUris?: string[];
  /** Default `authorization_code` + `refresh_token`. */
  grantTypes?: string[];
  /** Default `openid profile email offline_access`. */
  scopes?: string[];
  /** First-party app: no consent screen. */
  skipConsent?: boolean;
  /** Seconds, 60 to 86 400. */
  accessTokenTtl?: number;
  /** Seconds, 300 to 31 536 000. */
  refreshTokenTtl?: number;
}

export type UpdateClientInput = Partial<Omit<CreateClientInput, "realm" | "type">> & { disabled?: boolean };

/** A remembered consent of a user to a client. */
export interface OidcConsent {
  id: string;
  client_id: string;
  client_name: string;
  realm: string;
  scopes: string[];
  created_at: string;
  updated_at: string;
}

export interface ConsentPage {
  items: OidcConsent[];
  limit: number;
  offset: number;
}

export interface RealmLoginMethods {
  password: boolean;
  magic_link: boolean;
  email_otp: boolean;
  oauth: boolean;
  passkey: boolean;
  phone: boolean;
  ldap: boolean;
}

/** Identity settings of a realm. Secrets are write-only: reads show them masked. */
export interface RealmIdentityConfig {
  signup: boolean;
  methods: RealmLoginMethods;
  require_verified_email?: boolean | null;
  phone: { signup: boolean };
  ldap?: Json | null;
  turnstile?: Json | null;
}

export interface RealmIdentity {
  realm: string;
  collection: string;
  config: RealmIdentityConfig;
  created_at: string;
  updated_at: string;
}

/** What a sign-in page needs to know about the request's realm (no credentials needed). */
export interface RealmInfo {
  realm: string;
  collection: string;
  signup: boolean;
  methods: RealmLoginMethods;
  phone: { signup: boolean };
  turnstile?: { site_key: string; actions: unknown } | null;
  oauth_providers: unknown[];
}

export interface CreateRealmInput {
  /** Realm id (lowercase slug). */
  id: string;
  /** Display name (default: the id). */
  name?: string;
  /** Auth collection of the realm's users (default `<id>_users`). */
  collection?: string;
  config?: Partial<RealmIdentityConfig>;
}

export interface OidcKeys {
  realm: string;
  /** Kid of the key that signs now. */
  current?: string | null;
  items: { kid: string; alg: string; status: string; created_at: string; verify_until?: string | null; verifying: boolean }[];
}

/** A social login provider of a realm (secrets never leave the server). */
export interface RealmProvider {
  id: string;
  realm: string;
  kind: string;
  display_name?: string;
  /** `file` for providers defined in `oauth-providers.json` (read-only). */
  source?: string;
  read_only?: boolean;
  [key: string]: unknown;
}

/** SMS gateway of a realm, secrets masked; `configured: false` means the instance gateway is used. */
export interface SmsGateway {
  realm?: string;
  configured?: boolean;
  provider?: string;
  [key: string]: unknown;
}

export interface PhoneStatus {
  phone?: string | null;
  verified_at?: string | null;
}

export interface WebauthnCredential {
  id: string;
  name?: string | null;
  user_verified: boolean;
  use_as_mfa: boolean;
  backup_eligible: boolean;
  sign_count: number;
  created_at: string;
  last_used_at?: string | null;
}

/** `PublicKeyCredentialCreationOptions` / `RequestOptions` as the server sends them: pass `publicKey` to `navigator.credentials`. */
export interface WebauthnOptions {
  ceremony_id: string;
  publicKey: Json;
  [key: string]: unknown;
}

const listOf = async <T>(p: Promise<{ items: T[] }>): Promise<T[]> => (await p).items;

const keyBody = (o: ApiKeyOptions) => ({ name: o.name, scopes: o.scopes, expires_in_days: o.expiresInDays, tenant: o.tenant });

const signInBody = (o: Pick<SignInOptions, "collection" | "cookie" | "deviceName">, base: Json): Json => ({
  ...base,
  collection: o.collection,
  cookie: o.cookie,
  device_name: o.deviceName,
});

// ---------------------------------------------------------------------------------------------------------------------
// Machine credentials
// ---------------------------------------------------------------------------------------------------------------------

/** API keys of every owner (administrators). Create them under a service account: `identity.serviceAccounts.keys`. */
export class ApiKeysApi {
  constructor(private readonly t: Transport) {}

  /** Every API key and personal access token of the realm. */
  list(): Promise<ApiKeyInfo[]> {
    return listOf(this.t.request("/api/auth/api-keys"));
  }

  /** Revokes a key at once. */
  revoke(id: string): Promise<void> {
    return this.t.request(`/api/auth/api-keys/${seg(id)}`, { method: "DELETE" });
  }
}

/** Personal access tokens of the caller. Create one with `client.auth.createToken`. */
export class TokensApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<ApiKeyInfo[]> {
    return listOf(this.t.request("/api/auth/tokens"));
  }

  revoke(id: string): Promise<void> {
    return this.t.request(`/api/auth/tokens/${seg(id)}`, { method: "DELETE" });
  }
}

/** API keys of one service account. */
export class ServiceAccountKeysApi {
  constructor(private readonly t: Transport) {}

  list(serviceAccountId: string): Promise<ApiKeyInfo[]> {
    return listOf(this.t.request(`/api/auth/service-accounts/${seg(serviceAccountId)}/keys`));
  }

  /** The secret (`key`) is only returned here. */
  create(serviceAccountId: string, input: ApiKeyOptions): Promise<CreatedApiKey> {
    return this.t.request(`/api/auth/service-accounts/${seg(serviceAccountId)}/keys`, { method: "POST", body: keyBody(input) });
  }
}

export class ServiceAccountsApi {
  readonly keys: ServiceAccountKeysApi;
  constructor(private readonly t: Transport) {
    this.keys = new ServiceAccountKeysApi(t);
  }

  list(): Promise<ServiceAccount[]> {
    return listOf(this.t.request("/api/auth/service-accounts"));
  }

  create(input: { name: string; description?: string; /** Default `service`. */ role?: string }): Promise<ServiceAccount> {
    return this.t.request("/api/auth/service-accounts", { method: "POST", body: input });
  }

  get(id: string): Promise<ServiceAccount> {
    return this.t.request(`/api/auth/service-accounts/${seg(id)}`);
  }

  /** Deletes the account and revokes its keys. */
  delete(id: string): Promise<void> {
    return this.t.request(`/api/auth/service-accounts/${seg(id)}`, { method: "DELETE" });
  }
}

export class AdminsApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<AdminAccount[]> {
    return listOf(this.t.request("/api/auth/admins"));
  }

  create(input: { email: string; password: string }): Promise<AdminAccount> {
    return this.t.request("/api/auth/admins", { method: "POST", body: input });
  }
}

// ---------------------------------------------------------------------------------------------------------------------
// OAuth / OIDC
// ---------------------------------------------------------------------------------------------------------------------

/** Client apps of the built-in OpenID Connect provider (administrators). */
export class OAuthClientsApi {
  constructor(private readonly t: Transport) {}

  /** Clients of the caller's realm; platform administrators can pass another `realm`. */
  list(opts: { realm?: string } = {}): Promise<OAuthClient[]> {
    return listOf(this.t.request("/api/auth/clients", { query: { realm: opts.realm } }));
  }

  /** The `client_secret` of a confidential client is shown once. */
  create(input: CreateClientInput): Promise<CreatedOAuthClient> {
    return this.t.request("/api/auth/clients", {
      method: "POST",
      body: {
        realm: input.realm,
        name: input.name,
        description: input.description,
        type: input.type,
        redirect_uris: input.redirectUris,
        post_logout_redirect_uris: input.postLogoutRedirectUris,
        grant_types: input.grantTypes,
        scopes: input.scopes,
        skip_consent: input.skipConsent,
        access_token_ttl: input.accessTokenTtl,
        refresh_token_ttl: input.refreshTokenTtl,
      },
    });
  }

  get(clientId: string): Promise<OAuthClient> {
    return this.t.request(`/api/auth/clients/${seg(clientId)}`);
  }

  /** Changes the listed fields; the rest stay. `disabled: true` stops new authorizations and tokens. */
  update(clientId: string, patch: UpdateClientInput): Promise<OAuthClient> {
    return this.t.request(`/api/auth/clients/${seg(clientId)}`, {
      method: "PATCH",
      body: {
        name: patch.name,
        description: patch.description,
        redirect_uris: patch.redirectUris,
        post_logout_redirect_uris: patch.postLogoutRedirectUris,
        grant_types: patch.grantTypes,
        scopes: patch.scopes,
        skip_consent: patch.skipConsent,
        access_token_ttl: patch.accessTokenTtl,
        refresh_token_ttl: patch.refreshTokenTtl,
        disabled: patch.disabled,
      },
    });
  }

  delete(clientId: string): Promise<void> {
    return this.t.request(`/api/auth/clients/${seg(clientId)}`, { method: "DELETE" });
  }

  /** Issues a new secret (shown once). The old one keeps working for `graceSeconds` when given. */
  rotateSecret(clientId: string, opts: { graceSeconds?: number } = {}): Promise<CreatedOAuthClient> {
    return this.t.request(`/api/auth/clients/${seg(clientId)}/secret`, { method: "POST", body: { grace_seconds: opts.graceSeconds } });
  }
}

/** The signed-in user's remembered consents to client apps. */
export class ConsentsApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<OidcConsent[]> {
    return listOf(this.t.request("/api/auth/consents"));
  }

  revoke(id: string): Promise<void> {
    return this.t.request(`/api/auth/consents/${seg(id)}`, { method: "DELETE" });
  }
}

/** Public discovery documents of a realm's OpenID provider. The protocol endpoints (authorize, token, ...) are for OIDC libraries. */
export class OidcApi {
  constructor(private readonly t: Transport) {}

  /** `<base>/api/auth/oidc/<realm>`: the issuer to give an OIDC library. */
  issuer(realm: string): string {
    return this.t.url(`/api/auth/oidc/${seg(realm)}`);
  }

  /** The `.well-known/openid-configuration` URL. */
  discoveryUrl(realm: string): string {
    return `${this.issuer(realm)}/.well-known/openid-configuration`;
  }

  discovery(realm: string): Promise<Json> {
    return this.t.request(`/api/auth/oidc/${seg(realm)}/.well-known/openid-configuration`, { anonymous: true });
  }

  /** Public signing keys (JWKS). */
  jwks(realm: string): Promise<{ keys: Json[] }> {
    return this.t.request(`/api/auth/oidc/${seg(realm)}/jwks`, { anonymous: true });
  }
}

/** Social providers of one realm (administrators). Provider bodies are the server's JSON; secrets are write-only. */
export class RealmProvidersApi {
  constructor(private readonly t: Transport) {}

  list(realm: string): Promise<RealmProvider[]> {
    return listOf(this.t.request(`/api/auth/realms/${seg(realm)}/oauth-providers`));
  }

  get(realm: string, id: string): Promise<RealmProvider> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/oauth-providers/${seg(id)}`);
  }

  /** Creates or replaces a provider (`id` goes in the URL, not the body). Providers from `oauth-providers.json` are read-only. */
  put(realm: string, id: string, config: Json): Promise<RealmProvider> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/oauth-providers/${seg(id)}`, { method: "PUT", body: config });
  }

  delete(realm: string, id: string): Promise<void> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/oauth-providers/${seg(id)}`, { method: "DELETE" });
  }
}

/** SMS gateway of one realm (administrators). */
export class RealmSmsApi {
  constructor(private readonly t: Transport) {}

  get(realm: string): Promise<SmsGateway> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/sms`);
  }

  /** `{provider: "dev"}`, `{provider: "twilio", account_sid, auth_token, from | messaging_service_sid}` or `{provider: "webhook", url, bearer_token?}`. */
  put(realm: string, config: Json): Promise<SmsGateway> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/sms`, { method: "PUT", body: config });
  }

  /** Back to the instance gateway. */
  delete(realm: string): Promise<void> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/sms`, { method: "DELETE" });
  }
}

/** Realms and their identity settings (administrators). */
export class RealmsApi {
  readonly providers: RealmProvidersApi;
  readonly sms: RealmSmsApi;
  constructor(private readonly t: Transport) {
    this.providers = new RealmProvidersApi(t);
    this.sms = new RealmSmsApi(t);
  }

  /** What a sign-in page needs for the request's realm (`X-Forgebase-Realm`): methods, signup, providers. No credentials needed. */
  info(): Promise<RealmInfo> {
    return this.t.request("/api/auth/realm", { anonymous: true });
  }

  list(): Promise<RealmIdentity[]> {
    return listOf(this.t.request("/api/auth/realms"));
  }

  get(realm: string): Promise<RealmIdentity> {
    return this.t.request(`/api/auth/realms/${seg(realm)}`);
  }

  /** Creates the realm together with its identity settings. */
  create(input: CreateRealmInput): Promise<RealmIdentity> {
    return this.t.request("/api/auth/realms", { method: "POST", body: { id: input.id, name: input.name, collection: input.collection, config: input.config } });
  }

  /** Replaces the identity settings (and, once, the user collection). */
  update(realm: string, input: { collection?: string; config: Partial<RealmIdentityConfig> }): Promise<RealmIdentity> {
    return this.t.request(`/api/auth/realms/${seg(realm)}`, { method: "PUT", body: { collection: input.collection, config: input.config } });
  }

  /** Signing keys of the realm's OpenID provider. */
  keys(realm: string): Promise<OidcKeys> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/keys`);
  }

  /** Consents users of the realm gave to client apps. */
  consents(realm: string, opts: { clientId?: string; limit?: number; offset?: number } = {}): Promise<ConsentPage> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/consents`, { query: { client_id: opts.clientId, limit: opts.limit, offset: opts.offset } });
  }

  revokeConsent(realm: string, id: string): Promise<void> {
    return this.t.request(`/api/auth/realms/${seg(realm)}/consents/${seg(id)}`, { method: "DELETE" });
  }
}

// ---------------------------------------------------------------------------------------------------------------------
// Users, impersonation, audit
// ---------------------------------------------------------------------------------------------------------------------

/** Administrator view of end users and their lifecycle: `identity.users`. */
export class UsersAdminApi {
  constructor(private readonly t: Transport) {}

  private p(collection: string, id: string, tail = ""): string {
    return `/api/auth/users/${seg(collection)}/${seg(id)}${tail}`;
  }

  /** The user record with security details. */
  get(collection: string, id: string): Promise<Json> {
    return this.t.request(this.p(collection, id));
  }

  status(collection: string, id: string): Promise<AccountStatus> {
    return this.t.request(this.p(collection, id, "/status"));
  }

  /** Blocks sign-in; `until` (a `Date` or Unix seconds) ends the suspension by itself. */
  suspend(collection: string, id: string, opts: { reason?: string; until?: Date | number } = {}): Promise<AccountStatus> {
    const until = opts.until instanceof Date ? Math.floor(opts.until.getTime() / 1000) : opts.until;
    return this.t.request(this.p(collection, id, "/suspend"), { method: "POST", body: { reason: opts.reason, until } });
  }

  unsuspend(collection: string, id: string): Promise<AccountStatus> {
    return this.t.request(this.p(collection, id, "/unsuspend"), { method: "POST", body: {} });
  }

  deactivate(collection: string, id: string, opts: { reason?: string } = {}): Promise<AccountStatus> {
    return this.t.request(this.p(collection, id, "/deactivate"), { method: "POST", body: { reason: opts.reason } });
  }

  reactivate(collection: string, id: string): Promise<AccountStatus> {
    return this.t.request(this.p(collection, id, "/reactivate"), { method: "POST", body: {} });
  }

  /** Strips personal data but keeps the record (GDPR erasure that preserves references). */
  anonymise(collection: string, id: string): Promise<Json> {
    return this.t.request(this.p(collection, id, "/anonymise"), { method: "POST", body: {} });
  }

  /** Deletes the user and their credentials. */
  delete(collection: string, id: string): Promise<void> {
    return this.t.request(this.p(collection, id, "/delete"), { method: "POST", body: {} });
  }

  /** Everything stored about the user (data portability). */
  export(collection: string, id: string): Promise<Json> {
    return this.t.request(this.p(collection, id, "/export"));
  }

  sessions(collection: string, id: string): Promise<SessionInfo[]> {
    return listOf(this.t.request(this.p(collection, id, "/sessions")));
  }

  /** Signs the user out everywhere. Resolves with the number of sessions revoked. */
  async revokeSessions(collection: string, id: string): Promise<number> {
    return (await this.t.request<{ revoked: number }>(this.p(collection, id, "/sessions/revoke"), { method: "POST", body: {} })).revoked;
  }

  loginHistory(collection: string, id: string, opts: { limit?: number } = {}): Promise<Json[]> {
    return listOf(this.t.request(this.p(collection, id, "/login-history"), { query: { limit: opts.limit } }));
  }
}

/** "Sign in as" for support (administrators): every grant is audited and short lived. */
export class ImpersonationApi {
  constructor(private readonly t: Transport) {}

  /** `principal` is `user:<collection>/<id>`. `ttlMinutes` defaults to 15. Administrators and service accounts cannot be impersonated. */
  start(input: { principal: string; reason: string; ttlMinutes?: number }): Promise<ImpersonationGrant> {
    return this.t.request("/api/auth/impersonate", { method: "POST", body: { principal: input.principal, reason: input.reason, ttl_minutes: input.ttlMinutes } });
  }

  list(): Promise<ImpersonationInfo[]> {
    return listOf(this.t.request("/api/auth/impersonations"));
  }

  /** Ends the grant (and its session) before it expires; `id` is the grant's `session_id`. */
  end(id: string): Promise<void> {
    return this.t.request(`/api/auth/impersonations/${seg(id)}/end`, { method: "POST", body: {} });
  }
}

// ---------------------------------------------------------------------------------------------------------------------
// The signed-in user's own security settings
// ---------------------------------------------------------------------------------------------------------------------

/** Two-factor authentication with an authenticator app (TOTP). */
export class MfaApi {
  constructor(private readonly t: Transport) {}

  /** Starts enrolment: show `otpauth_url` as a QR code, then `confirmTotp` with the first code. */
  setupTotp(): Promise<TotpSetup> {
    return this.t.request("/api/auth/mfa/totp/setup", { method: "POST", body: {} });
  }

  /** Turns TOTP on. The recovery codes are shown once. */
  confirmTotp(code: string): Promise<Schemas["AuthTotpEnabled"]> {
    return this.t.request("/api/auth/mfa/totp/confirm", { method: "POST", body: { code } });
  }

  /** Turns TOTP off (needs a current code). */
  disableTotp(code: string): Promise<Schemas["AuthTotpDisabled"]> {
    return this.t.request("/api/auth/mfa/totp/disable", { method: "POST", body: { code } });
  }

  /** Replaces the recovery codes (needs a current code); resolves with the new ones. */
  async recoveryCodes(code: string): Promise<string[]> {
    return (await this.t.request<Schemas["AuthRecoveryCodes"]>("/api/auth/mfa/recovery-codes", { method: "POST", body: { code } })).recovery_codes;
  }
}

/** Phone numbers: SMS sign-in and linking a number to the account. */
export class PhoneApi {
  constructor(
    private readonly t: Transport,
    private readonly mgr: AuthManager,
  ) {}

  /** Texts a sign-in code (always accepted, whether or not the number can sign in). */
  async requestCode(phone: string, opts: { collection?: string } = {}): Promise<void> {
    await this.t.request("/api/auth/phone/request", { method: "POST", body: { phone, collection: opts.collection }, anonymous: true });
  }

  /** Signs in with the code; stores the session like `auth.signIn`. */
  async signIn(input: { phone: string; code: string } & SignInOptions): Promise<SignInResult> {
    const r = await this.t.request("/api/auth/phone/verify", { method: "POST", body: signInBody(input, { phone: input.phone, code: input.code }), anonymous: true });
    return this.mgr.accept(r, !!input.cookie);
  }

  /** The number linked to the signed-in user. */
  get(): Promise<PhoneStatus> {
    return this.t.request("/api/auth/phone");
  }

  /** Starts linking a number: a code is texted to it. */
  async add(phone: string): Promise<void> {
    await this.t.request("/api/auth/phone/add", { method: "POST", body: { phone } });
  }

  /** Finishes linking with the texted code. */
  confirm(phone: string, code: string): Promise<PhoneStatus> {
    return this.t.request("/api/auth/phone/confirm", { method: "POST", body: { phone, code } });
  }

  remove(): Promise<void> {
    return this.t.request("/api/auth/phone", { method: "DELETE" });
  }
}

/** Passkeys (WebAuthn). The `publicKey` options go to `navigator.credentials.create/get`; send the result back as `credential`. */
export class WebauthnApi {
  constructor(
    private readonly t: Transport,
    private readonly mgr: AuthManager,
  ) {}

  /** Registration options for the signed-in user. `useAsMfa` makes the key a second factor. */
  registerOptions(opts: { useAsMfa?: boolean } = {}): Promise<WebauthnOptions> {
    return this.t.request("/api/auth/webauthn/register/options", { method: "POST", body: { use_as_mfa: opts.useAsMfa } });
  }

  async registerVerify(input: { ceremonyId: string; credential: Json; name?: string }): Promise<WebauthnCredential> {
    const r = await this.t.request<{ credential: WebauthnCredential }>("/api/auth/webauthn/register/verify", {
      method: "POST",
      body: { ceremony_id: input.ceremonyId, credential: input.credential, name: input.name },
    });
    return r.credential;
  }

  /** Sign-in options; omit `email` for a discoverable (usernameless) passkey. */
  signInOptions(opts: { collection?: string; email?: string } = {}): Promise<WebauthnOptions> {
    return this.t.request("/api/auth/webauthn/login/options", { method: "POST", body: opts, anonymous: true });
  }

  /** Completes the passkey sign-in and stores the session. */
  async signIn(input: { ceremonyId: string; credential: Json } & Pick<SignInOptions, "cookie" | "deviceName">): Promise<SignInResult> {
    const r = await this.t.request("/api/auth/webauthn/login/verify", {
      method: "POST",
      body: { ceremony_id: input.ceremonyId, credential: input.credential, cookie: input.cookie, device_name: input.deviceName },
      anonymous: true,
    });
    return this.mgr.accept(r, !!input.cookie);
  }

  /** Second-factor options after `auth.signIn` returned an MFA challenge. */
  mfaOptions(mfaToken: string): Promise<WebauthnOptions> {
    return this.t.request("/api/auth/webauthn/mfa/options", { method: "POST", body: { mfa_token: mfaToken }, anonymous: true });
  }

  /** Completes the second factor with a passkey and stores the session. */
  async verifyMfa(input: { mfaToken: string; ceremonyId: string; credential: Json }): Promise<SignInResult> {
    const r = await this.t.request("/api/auth/webauthn/mfa/verify", {
      method: "POST",
      body: { mfa_token: input.mfaToken, ceremony_id: input.ceremonyId, credential: input.credential },
      anonymous: true,
    });
    return this.mgr.accept(r, false);
  }

  async credentials(): Promise<WebauthnCredential[]> {
    return (await this.t.request<{ credentials: WebauthnCredential[] }>("/api/auth/webauthn/credentials")).credentials;
  }

  deleteCredential(id: string): Promise<void> {
    return this.t.request(`/api/auth/webauthn/credentials/${seg(id)}`, { method: "DELETE" });
  }
}

/** LDAP directory sign-in (when the realm enables it). */
export class LdapApi {
  constructor(
    private readonly t: Transport,
    private readonly mgr: AuthManager,
  ) {}

  async signIn(input: { username: string; password: string } & Pick<SignInOptions, "cookie" | "deviceName">): Promise<SignInResult> {
    const r = await this.t.request("/api/auth/ldap/login", {
      method: "POST",
      body: { username: input.username, password: input.password, cookie: input.cookie, device_name: input.deviceName },
      anonymous: true,
    });
    return this.mgr.accept(r, !!input.cookie);
  }
}

/** Self-service account lifecycle of the signed-in user (GDPR). */
export class AccountApi {
  constructor(private readonly t: Transport) {}

  /** Deactivates the account; an administrator (or the user, signing in again) can reactivate it. Password accounts re-enter their password. */
  deactivate(opts: { currentPassword?: string } = {}): Promise<{ status: string }> {
    return this.t.request("/api/auth/account/deactivate", { method: "POST", body: { current_password: opts.currentPassword } });
  }

  /** Deletes the account, or with `anonymise` keeps an anonymised record. */
  delete(opts: { currentPassword?: string; anonymise?: boolean } = {}): Promise<Json> {
    return this.t.request("/api/auth/account/delete", { method: "POST", body: { current_password: opts.currentPassword, anonymise: opts.anonymise } });
  }

  /** Everything stored about the signed-in user. */
  export(): Promise<Json> {
    return this.t.request("/api/auth/account/export");
  }
}

// ---------------------------------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------------------------------

/**
 * Identity administration and account security: `client.identity`. Sign-in, sessions and `createToken` live
 * on `client.auth`; this covers what an admin console or a settings page needs on top.
 */
export class IdentityApi {
  /** API keys of every owner (administrators). */
  readonly apiKeys: ApiKeysApi;
  /** Personal access tokens of the caller. */
  readonly tokens: TokensApi;
  readonly serviceAccounts: ServiceAccountsApi;
  readonly admins: AdminsApi;
  /** OAuth / OIDC client apps (administrators). */
  readonly clients: OAuthClientsApi;
  /** The signed-in user's consents to client apps. */
  readonly consents: ConsentsApi;
  readonly oidc: OidcApi;
  readonly realms: RealmsApi;
  /** End users: lifecycle, sessions, export (administrators). */
  readonly users: UsersAdminApi;
  readonly impersonation: ImpersonationApi;
  readonly mfa: MfaApi;
  readonly phone: PhoneApi;
  readonly webauthn: WebauthnApi;
  readonly ldap: LdapApi;
  /** Self-service deactivate / delete / export. */
  readonly account: AccountApi;

  constructor(
    private readonly t: Transport,
    mgr: AuthManager,
  ) {
    this.apiKeys = new ApiKeysApi(t);
    this.tokens = new TokensApi(t);
    this.serviceAccounts = new ServiceAccountsApi(t);
    this.admins = new AdminsApi(t);
    this.clients = new OAuthClientsApi(t);
    this.consents = new ConsentsApi(t);
    this.oidc = new OidcApi(t);
    this.realms = new RealmsApi(t);
    this.users = new UsersAdminApi(t);
    this.impersonation = new ImpersonationApi(t);
    this.mfa = new MfaApi(t);
    this.phone = new PhoneApi(t, mgr);
    this.webauthn = new WebauthnApi(t, mgr);
    this.ldap = new LdapApi(t, mgr);
    this.account = new AccountApi(t);
  }

  /** Authentication audit log (administrators), newest first. */
  audit(q: AuthAuditQuery = {}): Promise<AuthAuditEntry[]> {
    return listOf(this.t.request("/api/auth/audit", { query: { event: q.event, subject_id: q.subjectId, before: q.before, limit: q.limit } }));
  }

  /** The caller's recent sign-ins. */
  loginHistory(): Promise<Json[]> {
    return listOf(this.t.request("/api/auth/login-history"));
  }

  /** Rotates the access-token signing key; resolves with the new key id. */
  async rotateSigningKey(): Promise<string> {
    return (await this.t.request<Schemas["AuthKeyRotated"]>("/api/auth/keys/rotate", { method: "POST", body: {} })).active_kid;
  }

  /** Rotates the OpenID Connect signing key of a realm (default: the request's). */
  rotateOidcKey(opts: { realm?: string } = {}): Promise<{ realm: string; kid: string }> {
    return this.t.request("/api/auth/keys/oidc/rotate", { method: "POST", body: { realm: opts.realm } });
  }
}
