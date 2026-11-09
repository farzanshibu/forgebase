/**
 * The single place the dashboard gets a bearer token from.
 *
 * Admin sessions use an HttpOnly cookie plus `X-CSRF-Token` (ADR-0014 §1), so
 * the default source returns nothing. The session layer installs a source only
 * while an impersonation is active (its bearer token lives in memory only).
 */
export type TokenSource = () => string | null | undefined | Promise<string | null | undefined>;

let source: TokenSource = () => null;

export function setTokenSource(next: TokenSource): void {
  source = next;
}

export async function authHeaders(): Promise<Record<string, string>> {
  const token = await source();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

let csrf: string | null = null;

/** CSRF token returned by sign-in; the `fb_csrf` cookie is preferred when readable. */
export function setCsrfToken(token: string | null): void {
  csrf = token;
}

function csrfCookie(): string | null {
  if (typeof document === "undefined") return null;
  const m = document.cookie.split(";").map((c) => c.trim()).find((c) => c.startsWith("fb_csrf="));
  return m ? decodeURIComponent(m.slice("fb_csrf=".length)) || null : null;
}

export function csrfHeaders(): Record<string, string> {
  const t = csrfCookie() ?? csrf;
  return t ? { "X-CSRF-Token": t } : {};
}

type Listener = () => void;
let onUnauthorized: Listener | null = null;

/** The session layer registers here to hear about 401s from authenticated requests. */
export function setUnauthorizedHandler(fn: Listener | null): void {
  onUnauthorized = fn;
}

export function notifyUnauthorized(): void {
  onUnauthorized?.();
}
