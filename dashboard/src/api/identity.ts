/**
 * Typed client for the identity provider and realm identity REST (`/api/auth/*`, ADR-0015 / ADR-0019).
 * Hand-written types over the documented routes (the OpenAPI document types most bodies as free-form objects).
 * Secrets are write-only: the server answers `"********"` (or null) for a stored secret; sending `""` clears it, and
 * sending `"********"`, null or nothing keeps it.
 */
import { request, send } from "./client";

const A = "/api/auth";
const enc = encodeURIComponent;

export const SECRET_MASK = "********";

export interface LoginMethods { password: boolean; magic_link: boolean; email_otp: boolean; oauth: boolean; passkey: boolean; phone: boolean; ldap: boolean }
export type LdapScope = "base" | "one" | "sub";
export interface LdapSettings {
  url: string; start_tls: boolean; insecure_plaintext: boolean; ca_pem: string | null; bind_dn: string | null; bind_password: string | null;
  base_dn: string; user_filter: string; scope: LdapScope; email_attribute: string; name_attribute: string; id_attribute: string | null;
  trust_email: boolean; auto_link_verified_email: boolean; allow_signup: boolean; timeout_secs: number;
}
export type TurnstileAction = "signup" | "login" | "password_reset" | "otp";
export const TURNSTILE_ACTIONS: TurnstileAction[] = ["signup", "login", "password_reset", "otp"];
export interface TurnstileSettings { enabled: boolean; site_key: string; secret: string | null; actions: TurnstileAction[]; hostnames: string[]; verify_url: string | null }
export interface RealmConfig {
  signup: boolean; methods: LoginMethods; require_verified_email: boolean | null; phone: { signup: boolean };
  ldap: LdapSettings | null; turnstile: TurnstileSettings | null;
}
export interface RealmIdentity { realm: string; collection: string; config: RealmConfig; created_at: string; updated_at: string }
export interface RealmInput { collection?: string; config: RealmConfig }
export interface OAuthProviderInfo { id: string; kind: string; display_name: string }

export const DEFAULT_LDAP: LdapSettings = {
  url: "", start_tls: false, insecure_plaintext: false, ca_pem: null, bind_dn: null, bind_password: null, base_dn: "", user_filter: "(uid={username})",
  scope: "sub", email_attribute: "mail", name_attribute: "cn", id_attribute: null, trust_email: true, auto_link_verified_email: false, allow_signup: true, timeout_secs: 10,
};
export const DEFAULT_TURNSTILE: TurnstileSettings = { enabled: false, site_key: "", secret: null, actions: ["signup", "login"], hostnames: [], verify_url: null };

export type ClientType = "confidential" | "public";
export const GRANT_TYPES = ["authorization_code", "refresh_token", "client_credentials", "urn:ietf:params:oauth:grant-type:device_code"] as const;
export const OIDC_SCOPES = ["openid", "profile", "email", "phone", "offline_access"] as const;
export interface ClientApp {
  client_id: string; realm: string; name: string; description: string | null; type: ClientType; redirect_uris: string[]; post_logout_redirect_uris: string[];
  grant_types: string[]; scopes: string[]; skip_consent: boolean; access_token_ttl: number | null; refresh_token_ttl: number | null; disabled: boolean;
  secret_created_at: string | null; previous_secret_expires_at: string | null; created_at: string; updated_at: string;
}
/** The secret appears in this answer only, once. */
export interface CreatedClient { client: ClientApp; client_secret?: string }
export interface ClientInput {
  realm?: string; name: string; description?: string; type?: ClientType; redirect_uris?: string[]; post_logout_redirect_uris?: string[];
  grant_types?: string[]; scopes?: string[]; skip_consent?: boolean; access_token_ttl?: number; refresh_token_ttl?: number;
}
export type ClientPatch = Partial<Omit<ClientInput, "realm" | "type">> & { disabled?: boolean };
export interface ConsentGrant { id: string; client_id: string; client_name: string; realm: string; scopes: string[]; created_at: string; updated_at: string }

export const IDENTITY_OPERATIONS = [
  "get /api/auth/realms", "get /api/auth/realms/{realm}", "put /api/auth/realms/{realm}", "get /api/auth/realm",
  "get /api/auth/clients", "post /api/auth/clients", "get /api/auth/clients/{id}", "patch /api/auth/clients/{id}", "delete /api/auth/clients/{id}",
  "post /api/auth/clients/{id}/secret", "get /api/auth/consents", "delete /api/auth/consents/{id}", "post /api/auth/keys/oidc/rotate",
  "post /api/auth/realms", "get /api/auth/realms/{realm}/keys", "get /api/auth/realms/{realm}/consents", "delete /api/auth/realms/{realm}/consents/{id}",
  "get /api/auth/realms/{realm}/oauth-providers", "get /api/auth/realms/{realm}/oauth-providers/{id}", "put /api/auth/realms/{realm}/oauth-providers/{id}",
  "delete /api/auth/realms/{realm}/oauth-providers/{id}", "get /api/auth/realms/{realm}/sms", "put /api/auth/realms/{realm}/sms", "delete /api/auth/realms/{realm}/sms",
] as const;

/** Administrator view of a remembered consent. */
export interface AdminConsent extends ConsentGrant { collection: string; user_id: string }
export interface OidcKeyInfo { kid: string; alg: string; status: "active" | "retired"; created_at: string; verify_until: string | null; verifying: boolean }
export interface OidcKeys { realm: string; current: string | null; items: OidcKeyInfo[] }
export type ProviderKind = "google" | "github" | "apple" | "facebook" | "wechat" | "discord" | "oidc";
export const PROVIDER_KINDS: ProviderKind[] = ["google", "github", "apple", "facebook", "wechat", "discord", "oidc"];
/** A social login provider of a realm. Secrets are write-only (`"********"` when stored). `source: "file"` entries are read-only. */
export interface SocialProvider {
  id: string; realm: string; kind: ProviderKind; display_name?: string | null; source: "admin" | "file"; read_only: boolean;
  client_id?: string; client_secret?: string | null; scopes?: string[] | null; issuer?: string | null; authorize_url?: string | null; token_url?: string | null;
  userinfo_url?: string | null; jwks_url?: string | null; emails_url?: string | null; redirect_uri?: string | null;
  apple?: { team_id: string; key_id: string; private_key_pem?: string | null } | null;
  allow_signup?: boolean; auto_link_verified_email?: boolean; created_at?: string; updated_at?: string;
}
export type SocialProviderInput = Omit<SocialProvider, "id" | "realm" | "source" | "read_only" | "created_at" | "updated_at"> & { client_id: string };
export type SmsProviderType = "dev" | "twilio" | "webhook";
export interface SmsConfig {
  provider: SmsProviderType; account_sid?: string; auth_token?: string | null; from?: string | null; messaging_service_sid?: string | null; base_url?: string | null;
  url?: string; bearer_token?: string | null;
}
export interface SmsGateway { realm: string; configured: boolean; config: SmsConfig | null; created_at?: string; updated_at?: string }
export interface RealmCreateInput { id: string; name?: string; collection?: string; config?: RealmConfig }

export const listRealmIdentities = async (signal?: AbortSignal) => (await request<{ items: RealmIdentity[] }>(`${A}/realms`, { signal })).items;
export const putRealmIdentity = (realm: string, body: RealmInput) => send<RealmIdentity>("PUT", `${A}/realms/${enc(realm)}`, body);
/** Public sign-in options of the request's realm, including the registered OAuth/social providers (read-only). */
export const realmInfo = (realm: string, signal?: AbortSignal) =>
  request<{ realm: string; oauth_providers: OAuthProviderInfo[] }>(`${A}/realm`, { signal, headers: { "X-ForgeBase-Realm": realm } });

export const listClients = async (realm?: string, signal?: AbortSignal) =>
  (await request<{ items: ClientApp[] }>(`${A}/clients${realm ? `?realm=${enc(realm)}` : ""}`, { signal })).items;
export const createClient = (body: ClientInput) => send<CreatedClient>("POST", `${A}/clients`, body);
export const updateClient = (id: string, patch: ClientPatch) => send<ClientApp>("PATCH", `${A}/clients/${enc(id)}`, patch);
export const deleteClient = (id: string) => send<void>("DELETE", `${A}/clients/${enc(id)}`);
/** Rotates the secret; the previous one keeps working for `graceSeconds` (0 to 30 days). The new secret is returned once. */
export const rotateClientSecret = (id: string, graceSeconds?: number) =>
  send<CreatedClient>("POST", `${A}/clients/${enc(id)}/secret`, graceSeconds === undefined ? {} : { grace_seconds: graceSeconds });

export const listConsents = async (signal?: AbortSignal) => (await request<{ items: ConsentGrant[] }>(`${A}/consents`, { signal })).items;
export const revokeConsent = (id: string) => send<void>("DELETE", `${A}/consents/${enc(id)}`);
export const rotateOidcKeys = (realm: string) => send<{ realm: string; kid: string }>("POST", `${A}/keys/oidc/rotate`, { realm });

/** One call: creates the realm and its identity settings. */
export const createRealmWithIdentity = (body: RealmCreateInput) => send<RealmIdentity>("POST", `${A}/realms`, body);
export const listRealmConsents = async (realm: string, clientId?: string, signal?: AbortSignal) =>
  (await request<{ items: AdminConsent[] }>(`${A}/realms/${enc(realm)}/consents${clientId ? `?client_id=${enc(clientId)}` : ""}`, { signal })).items;
export const revokeRealmConsent = (realm: string, id: string) => send<void>("DELETE", `${A}/realms/${enc(realm)}/consents/${enc(id)}`);
export const listOidcKeys = (realm: string, signal?: AbortSignal) => request<OidcKeys>(`${A}/realms/${enc(realm)}/keys`, { signal });
export const listSocialProviders = async (realm: string, signal?: AbortSignal) =>
  (await request<{ items: SocialProvider[] }>(`${A}/realms/${enc(realm)}/oauth-providers`, { signal })).items;
export const getSocialProvider = (realm: string, id: string, signal?: AbortSignal) => request<SocialProvider>(`${A}/realms/${enc(realm)}/oauth-providers/${enc(id)}`, { signal });
export const putSocialProvider = (realm: string, id: string, body: SocialProviderInput) => send<SocialProvider>("PUT", `${A}/realms/${enc(realm)}/oauth-providers/${enc(id)}`, body);
export const deleteSocialProvider = (realm: string, id: string) => send<void>("DELETE", `${A}/realms/${enc(realm)}/oauth-providers/${enc(id)}`);
export const getSmsGateway = (realm: string, signal?: AbortSignal) => request<SmsGateway>(`${A}/realms/${enc(realm)}/sms`, { signal });
export const putSmsGateway = (realm: string, body: SmsConfig) => send<SmsGateway>("PUT", `${A}/realms/${enc(realm)}/sms`, body);
export const deleteSmsGateway = (realm: string) => send<void>("DELETE", `${A}/realms/${enc(realm)}/sms`);
