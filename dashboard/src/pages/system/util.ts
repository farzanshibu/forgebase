/** A blank input means "not set"; anything else must be a whole number of at least `min`. */
export function parseCount(s: string, min: number): { ok: true; value?: number } | { ok: false } {
  const t = s.trim();
  if (t === "") return { ok: true };
  if (!/^\d+$/.test(t)) return { ok: false };
  const n = Number(t);
  return n >= min ? { ok: true, value: n } : { ok: false };
}
