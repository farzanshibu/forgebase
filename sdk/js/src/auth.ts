import { InvalidResponseError } from "./errors";
import type { Credentials, Transport } from "./http";

/** Wire shape of a session (`/api/auth/login`, `/refresh`, ...). */
interface WireSession {
  token_type: string;
  access_token?: string;
  expires_in: number;
  refresh_token?: string;
  session_id: string;
  csrf_token?: string;
  session_expires_at: string;
  subject: unknown;
  user?: Record<string, unknown>;
}

export interface Session {
  /** Bearer token; absent in cookie mode. */
  accessToken?: string;
  /** Rotating refresh token; absent in cookie mode. */
  refreshToken?: string;
  /** Access token expiry, epoch milliseconds. */
  expiresAt: number;
  sessionId: string;
  /** Absolute session expiry (RFC 3339). */
  sessionExpiresAt: string;
  /** CSRF token for cookie mode (sent as `X-CSRF-Token`). */
  csrfToken?: string;
  /** Who signed in (`{kind:"user",collection,id}` and friends). */
  subject: unknown;
  /** The user record, for user sign-ins. */
  user?: Record<string, unknown>;
  /** True when the session lives in an HttpOnly cookie. */
  cookie: boolean;
}

/** A second factor is required: call `auth.verifyMfa(mfaToken, code)`. */
export interface MfaChallenge {
  mfaRequired: true;
  mfaToken: string;
  /** Seconds the challenge is valid. */
  expiresIn: number;
}

export type SignInResult = Session | MfaChallenge;

export const isMfaChallenge = (r: SignInResult): r is MfaChallenge => "mfaRequired" in r;

export interface SignInOptions {
  /** User collection (server default `users`). */
  collection?: string;
  /** Use an HttpOnly session cookie (browsers) instead of returning tokens. */
  cookie?: boolean;
  /** Name shown in the device list. */
  deviceName?: string;
}

/** Where the session is kept between page loads / process restarts. */
export interface SessionStorage {
  get(): string | null | Promise<string | null>;
  set(value: string): void | Promise<void>;
  remove(): void | Promise<void>;
}

export function memoryStorage(): SessionStorage {
  let v: string | null = null;
  return {
    get: () => v,
    set: (x) => void (v = x),
    remove: () => void (v = null),
  };
}

/** Wraps `localStorage` / `sessionStorage` (or any `Storage`). */
export function webStorage(storage: Pick<Storage, "getItem" | "setItem" | "removeItem">, key = "forgebase.session"): SessionStorage {
  return {
    get: () => storage.getItem(key),
    set: (v) => storage.setItem(key, v),
    remove: () => storage.removeItem(key),
  };
}

export type AuthEvent = "signed_in" | "token_refreshed" | "signed_out";
export type AuthListener = (event: AuthEvent, session: Session | null) => void;

export interface AuthOptions {
  /** Static API key / service-account key / PAT: sent as `X-API-Key`. */
  apiKey?: string;
  /** Static bearer access token (no refresh). */
  accessToken?: string;
  storage?: SessionStorage;
  /** Refresh the access token this long (ms) before it expires (default 30 000). */
  refreshSkewMs?: number;
  /** Refresh automatically (default true). */
  autoRefresh?: boolean;
  now?: () => number;
}

const SAFE = new Set(["GET", "HEAD", "OPTIONS"]);

function toSession(w: WireSession, now: number, cookie: boolean): Session {
  if (typeof w?.session_id !== "string") throw new InvalidResponseError("sign-in response carries no session");
  return {
    accessToken: w.access_token,
    refreshToken: w.refresh_token,
    expiresAt: now + w.expires_in * 1000,
    sessionId: w.session_id,
    sessionExpiresAt: w.session_expires_at,
    csrfToken: w.csrf_token,
    subject: w.subject,
    user: w.user,
    cookie: cookie || w.access_token === undefined,
  };
}

/** Holds the session and implements the transport's credentials (headers, refresh, retry after 401). */
export class AuthManager implements Credentials {
  private session: Session | null = null;
  private loaded = false;
  private refreshing: Promise<Session> | null = null;
  private readonly listeners = new Set<AuthListener>();
  private readonly storage: SessionStorage;
  private readonly now: () => number;
  apiKey: string | undefined;
  private staticToken: string | undefined;

  constructor(
    private readonly transport: Transport,
    private readonly opts: AuthOptions = {},
  ) {
    this.storage = opts.storage ?? memoryStorage();
    this.now = opts.now ?? Date.now;
    this.apiKey = opts.apiKey;
    this.staticToken = opts.accessToken;
  }

  get fetchCredentials(): RequestCredentials | undefined {
    return this.session?.cookie ? "include" : undefined;
  }

  onChange(fn: AuthListener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit(e: AuthEvent): void {
    for (const l of this.listeners) l(e, this.session);
  }

  private async load(): Promise<void> {
    if (this.loaded) return;
    this.loaded = true;
    try {
      const raw = await this.storage.get();
      if (raw && !this.session) this.session = JSON.parse(raw) as Session;
    } catch {
      /* unreadable storage: start signed out */
    }
  }

  /** The current session (after restoring it from storage). */
  async getSession(): Promise<Session | null> {
    await this.load();
    return this.session;
  }

  /** Current access token, refreshed first when it is about to expire. */
  async getAccessToken(): Promise<string | undefined> {
    if (this.staticToken) return this.staticToken;
    await this.load();
    const s = this.session;
    if (!s?.accessToken) return undefined;
    if (this.opts.autoRefresh !== false && s.refreshToken && s.expiresAt - (this.opts.refreshSkewMs ?? 30_000) <= this.now()) {
      try {
        return (await this.refresh()).accessToken;
      } catch {
        return s.accessToken;
      }
    }
    return s.accessToken;
  }

  async setSession(s: Session | null, event: AuthEvent = "signed_in"): Promise<void> {
    this.loaded = true;
    this.session = s;
    try {
      if (s) await this.storage.set(JSON.stringify(s));
      else await this.storage.remove();
    } catch {
      /* storage is best effort */
    }
    this.emit(s ? event : "signed_out");
  }

  /** Use an API key (`fb_...`) for every request. */
  setApiKey(key: string | undefined): void {
    this.apiKey = key;
  }

  /** Use a static bearer token for every request. */
  setAccessToken(token: string | undefined): void {
    this.staticToken = token;
  }

  /** Rotates the refresh token. Concurrent callers share one request. */
  refresh(): Promise<Session> {
    this.refreshing ??= this.doRefresh().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  private async doRefresh(): Promise<Session> {
    await this.load();
    const rt = this.session?.refreshToken;
    if (!rt) throw new InvalidResponseError("no refresh token: sign in first", 401);
    try {
      const w = await this.transport.request<WireSession>("/api/auth/refresh", {
        method: "POST",
        body: { refresh_token: rt },
        anonymous: true,
      });
      const next = toSession(w, this.now(), false);
      next.user ??= this.session?.user;
      await this.setSession(next, "token_refreshed");
      return next;
    } catch (err) {
      // A rejected refresh token (revoked, reused) ends the session; transient failures keep it.
      if ((err as { status?: number }).status === 401) await this.setSession(null);
      throw err;
    }
  }

  async headers(method: string): Promise<Record<string, string>> {
    const h: Record<string, string> = {};
    if (this.apiKey) {
      h["X-API-Key"] = this.apiKey;
      return h;
    }
    const token = await this.getAccessToken();
    if (token) h.Authorization = `Bearer ${token}`;
    else if (this.session?.cookie && this.session.csrfToken && !SAFE.has(method)) h["X-CSRF-Token"] = this.session.csrfToken;
    return h;
  }

  async onUnauthorized(sent: Record<string, string>): Promise<boolean> {
    if (this.apiKey || this.opts.autoRefresh === false || !sent.Authorization) return false;
    await this.load();
    const s = this.session;
    if (!s?.refreshToken) return false;
    // Someone else already rotated the token since this request was built: just retry.
    if (s.accessToken && sent.Authorization !== `Bearer ${s.accessToken}`) return true;
    try {
      await this.refresh();
      return true;
    } catch {
      return false;
    }
  }

  /** Converts a sign-in style response and stores the session. */
  async accept(body: unknown, cookie: boolean): Promise<SignInResult> {
    const b = body as Partial<{ mfa_required: boolean; mfa_token: string; expires_in: number }> | undefined;
    if (b?.mfa_required) return { mfaRequired: true, mfaToken: String(b.mfa_token), expiresIn: Number(b.expires_in) };
    const s = toSession(body as WireSession, this.now(), cookie);
    await this.setSession(s);
    return s;
  }
}

/** `client.auth`: sign-up, sign-in flows, refresh and account helpers. */
export class AuthApi {
  constructor(
    private readonly t: Transport,
    private readonly mgr: AuthManager,
  ) {
    this.oauth = new OAuthApi(t, mgr);
  }

  /** Subscribe to sign-in, refresh and sign-out events. Returns an unsubscribe function. */
  onChange(fn: AuthListener): () => void {
    return this.mgr.onChange(fn);
  }

  getSession(): Promise<Session | null> {
    return this.mgr.getSession();
  }

  getAccessToken(): Promise<string | undefined> {
    return this.mgr.getAccessToken();
  }

  /** Adopt tokens obtained elsewhere (SSR hand-off, tests). */
  setSession(s: Session | null): Promise<void> {
    return this.mgr.setSession(s);
  }

  setApiKey(key: string | undefined): void {
    this.mgr.setApiKey(key);
  }

  /** Creates a user. Does not sign in. */
  async signUp(input: { email: string; password: string; collection?: string; data?: Record<string, unknown> }): Promise<Record<string, unknown>> {
    const r = await this.t.request<{ user: Record<string, unknown> }>("/api/auth/signup", { method: "POST", body: input, anonymous: true });
    return r.user;
  }

  /** Email + password. Returns a session, or an `MfaChallenge` when a second factor is needed. */
  async signIn(input: { email: string; password: string } & SignInOptions): Promise<SignInResult> {
    return this.mgr.accept(await this.post("/api/auth/login", wireOpts(input, { email: input.email, password: input.password })), !!input.cookie);
  }

  /** Administrator sign-in (`/api/auth/admins/login`). */
  async signInAdmin(input: { email: string; password: string } & Omit<SignInOptions, "collection">): Promise<SignInResult> {
    return this.mgr.accept(await this.post("/api/auth/admins/login", wireOpts(input, { email: input.email, password: input.password })), !!input.cookie);
  }

  /** Second step after `signIn` returned an `MfaChallenge`: a TOTP or recovery code. */
  async verifyMfa(mfaToken: string, code: string): Promise<Session> {
    const r = await this.mgr.accept(await this.post("/api/auth/mfa/verify", { mfa_token: mfaToken, code }), false);
    return r as Session;
  }

  /** Emails a magic link. The link points to `<public_url>/auth/magic-link?token=...`: pass that token to `confirmMagicLink`. */
  async requestMagicLink(email: string, collection?: string): Promise<void> {
    await this.post("/api/auth/magic-link/request", { email, collection });
  }

  async confirmMagicLink(token: string, opts: Omit<SignInOptions, "collection"> = {}): Promise<SignInResult> {
    return this.mgr.accept(await this.post("/api/auth/magic-link/confirm", wireOpts(opts, { token })), !!opts.cookie);
  }

  /** Emails a one-time code. */
  async requestOtp(email: string, collection?: string): Promise<void> {
    await this.post("/api/auth/otp/request", { email, collection });
  }

  async verifyOtp(input: { email: string; code: string } & SignInOptions): Promise<SignInResult> {
    return this.mgr.accept(await this.post("/api/auth/otp/verify", wireOpts(input, { email: input.email, code: input.code })), !!input.cookie);
  }

  /** Rotates the refresh token and returns the new session (also done automatically). */
  refresh(): Promise<Session> {
    return this.mgr.refresh();
  }

  /** Revokes the session on the server and forgets it locally. */
  async signOut(): Promise<void> {
    try {
      if (await this.mgr.getSession()) await this.t.request("/api/auth/logout", { method: "POST" });
    } finally {
      await this.mgr.setSession(null);
    }
  }

  /** The caller as the server sees it (`subject`, `credential`, `mfa`, scopes...). */
  me<T = Record<string, unknown>>(): Promise<T> {
    return this.t.request<T>("/api/auth/me");
  }

  async requestPasswordReset(email: string, collection?: string): Promise<void> {
    await this.post("/api/auth/password-reset/request", { email, collection });
  }

  async confirmPasswordReset(token: string, password: string): Promise<void> {
    await this.post("/api/auth/password-reset/confirm", { token, password });
  }

  async requestEmailVerification(email: string, collection?: string): Promise<void> {
    await this.post("/api/auth/verify-email/request", { email, collection });
  }

  async confirmEmailVerification(token: string): Promise<void> {
    await this.post("/api/auth/verify-email/confirm", { token });
  }

  async changePassword(current: string, next: string): Promise<void> {
    await this.t.request("/api/auth/password/change", { method: "POST", body: { current_password: current, new_password: next } });
  }

  /** Personal access tokens: the secret is only returned here. */
  createToken(input: { name: string; scopes: string[]; expiresInDays?: number }): Promise<Record<string, unknown>> {
    return this.t.request("/api/auth/tokens", { method: "POST", body: { name: input.name, scopes: input.scopes, expires_in_days: input.expiresInDays } });
  }

  listSessions(): Promise<unknown> {
    return this.t.request("/api/auth/sessions");
  }

  revokeSession(id: string): Promise<void> {
    return this.t.request(`/api/auth/sessions/${encodeURIComponent(id)}`, { method: "DELETE" });
  }

  readonly oauth: OAuthApi;

  private post(path: string, body: Record<string, unknown>): Promise<unknown> {
    return this.t.request(path, { method: "POST", body, anonymous: true });
  }
}

/** Social / OIDC sign-in (ADR-0009). */
export class OAuthApi {
  constructor(
    private readonly t: Transport,
    private readonly mgr: AuthManager,
  ) {}

  /** Providers configured on the server. */
  async providers(): Promise<unknown[]> {
    return (await this.t.request<{ providers: unknown[] }>("/api/auth/oauth/providers", { anonymous: true })).providers;
  }

  /** URL to send the browser to (`window.location.assign(url)`): the server redirects to the provider. */
  redirectUrl(provider: string, opts: SignInOptions = {}): string {
    return this.t.url(`/api/auth/oauth/${encodeURIComponent(provider)}/start`, {
      collection: opts.collection,
      cookie: opts.cookie ? "true" : undefined,
      device_name: opts.deviceName,
    });
  }

  /** Same as `redirectUrl` but returns the provider URL and the `state` instead of redirecting (native and server apps). */
  async start(provider: string, opts: SignInOptions = {}): Promise<{ authorizationUrl: string; state: string }> {
    const r = await this.t.request<{ authorization_url: string; state: string }>(`/api/auth/oauth/${encodeURIComponent(provider)}/start`, {
      method: "POST",
      body: wireOpts(opts, {}),
      anonymous: true,
    });
    return { authorizationUrl: r.authorization_url, state: r.state };
  }

  /** Finishes the flow with the `code` and `state` the provider sent to your redirect URI. */
  async complete(provider: string, params: { code: string; state: string }, opts: Pick<SignInOptions, "cookie"> = {}): Promise<SignInResult> {
    const r = await this.t.request(`/api/auth/oauth/${encodeURIComponent(provider)}/callback`, { query: params, anonymous: true });
    return this.mgr.accept(r, !!opts.cookie);
  }

  /** Parses `?code=...&state=...` (or `?error=...`) from the redirect URL and completes the flow. */
  completeFromUrl(provider: string, url: string | URL, opts: Pick<SignInOptions, "cookie"> = {}): Promise<SignInResult> {
    const u = typeof url === "string" ? new URL(url, "http://localhost") : url;
    const error = u.searchParams.get("error");
    const code = u.searchParams.get("code");
    const state = u.searchParams.get("state");
    if (error || !code || !state) return Promise.reject(new InvalidResponseError(error ? `provider error: ${error}` : "redirect URL has no code/state", 400));
    return this.complete(provider, { code, state }, opts);
  }

  /** Starts linking a provider to the signed-in user. */
  async link(provider: string): Promise<{ authorizationUrl: string; state: string }> {
    const r = await this.t.request<{ authorization_url: string; state: string }>(`/api/auth/oauth/${encodeURIComponent(provider)}/link`, { method: "POST" });
    return { authorizationUrl: r.authorization_url, state: r.state };
  }

  identities(): Promise<unknown> {
    return this.t.request("/api/auth/oauth/identities");
  }

  unlink(id: string): Promise<void> {
    return this.t.request(`/api/auth/oauth/identities/${encodeURIComponent(id)}`, { method: "DELETE" });
  }
}

function wireOpts(o: SignInOptions, base: Record<string, unknown>): Record<string, unknown> {
  return { ...base, collection: o.collection, cookie: o.cookie, device_name: o.deviceName };
}
