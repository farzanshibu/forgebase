/** Admin sign-in, whoami, sign-out and impersonation (ADR-0014 §1, §4). Routes live outside the OpenAPI document. */
import { request, send } from "./client";

export interface AdminInfo {
  id: string;
  email: string;
}

/** What an administrator manages: the whole server, or the one realm they hold `identity:admin` in (FB-137). */
export type AdminScope = "platform" | { realm: string };

export interface MeResponse {
  subject: { type: string; id?: string; collection?: string };
  admin?: { id: string; email: string };
  /** Present for administrators only. A server older than FB-137 omits it: treat that as `platform`. */
  admin_scope?: AdminScope;
  impersonator?: string | null;
  mfa?: boolean;
  mfa_enabled?: boolean;
}

export interface LoginSession {
  session_id: string;
  csrf_token?: string;
  subject: { type: string; id?: string };
  session_expires_at?: string;
}
export interface MfaChallenge {
  mfa_required: true;
  mfa_token: string;
  expires_in: number;
}
export type LoginResult = LoginSession | MfaChallenge;
export const isMfaChallenge = (r: LoginResult): r is MfaChallenge => (r as MfaChallenge).mfa_required === true;

export interface ImpersonationGrant {
  access_token: string;
  token_type?: string;
  expires_in: number;
  expires_at: string;
  session_id: string;
  impersonator: string;
  subject: { type: string; id?: string; collection?: string };
  reason: string;
}

const quiet = { quiet401: true } as const;

export const login = (email: string, password: string) =>
  request<LoginResult>("/api/auth/admins/login", {
    method: "POST",
    body: { email, password, cookie: true, device_name: "Admin dashboard" },
    ...quiet,
  });
/**
 * Sign-in as a user of a realm (`/api/auth/login`): how a realm administrator signs in, since they are users holding
 * `identity:admin` in their realm and not instance administrators. `realm` is sent in X-ForgeBase-Realm; empty means the default realm.
 */
export const userLogin = (email: string, password: string, realm?: string) =>
  request<LoginResult>("/api/auth/login", {
    method: "POST",
    body: { email, password, cookie: true, device_name: "Admin dashboard" },
    headers: realm ? { "X-ForgeBase-Realm": realm } : undefined,
    ...quiet,
  });
export const mfaVerify = (mfa_token: string, code: string) =>
  request<LoginSession>("/api/auth/mfa/verify", { method: "POST", body: { mfa_token, code }, ...quiet });
export const me = (signal?: AbortSignal) => request<MeResponse>("/api/auth/me", { signal, ...quiet });
export const logout = () => request<void>("/api/auth/logout", { method: "POST", ...quiet });
export const impersonate = (principal: string, reason: string, ttl_minutes?: number) =>
  send<ImpersonationGrant>("POST", "/api/auth/impersonate", { principal, reason, ...(ttl_minutes ? { ttl_minutes } : {}) });
/** Ends the impersonation: the bearer token of the impersonated session signs itself out. */
export const endImpersonation = () => logout();
