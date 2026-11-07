/**
 * The single place the dashboard gets a bearer token from.
 *
 * Until admin login ships (FB-23) there is no token, so the default source
 * returns nothing and requests go out anonymous (the server answers 401).
 * FB-23 calls `setTokenSource` with a function that reads its session.
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
