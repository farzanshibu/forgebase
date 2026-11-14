/** A blank input means "not set"; anything else must be a whole number of at least `min`. */
export function parseCount(s: string, min: number): { ok: true; value?: number } | { ok: false } {
  const t = s.trim();
  if (t === "") return { ok: true };
  if (!/^\d+$/.test(t)) return { ok: false };
  const n = Number(t);
  return n >= min ? { ok: true, value: n } : { ok: false };
}

/** Days of validity left below which the certificate gets a warning: the threshold `forgebase doctor` uses. */
export const TLS_WARN_DAYS = 14;

/** How long a certificate has left, in words, with how worrying that is. Negative seconds mean it has expired. */
export function expiryWords(secs: number): { level: "ok" | "warn" | "bad"; text: string } {
  if (secs < 0) {
    const d = Math.ceil(-secs / 86_400);
    return { level: "bad", text: `expired ${d} day${d === 1 ? "" : "s"} ago` };
  }
  const days = Math.floor(secs / 86_400);
  const text = days >= 1 ? `${days} day${days === 1 ? "" : "s"}` : `${Math.max(1, Math.floor(secs / 3600))} hour${secs < 7200 ? "" : "s"}`;
  return { level: days < TLS_WARN_DAYS ? "warn" : "ok", text: `${text} left` };
}
