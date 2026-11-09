/** Parse "1, 2, 3" or "[1,2,3]" into numbers; throws a readable Error. */
export function parseVector(text: string): number[] {
  const t = text.trim();
  if (!t) throw new Error("Enter a vector, for example 0.1, 0.2, 0.3");
  let raw: unknown;
  if (t.startsWith("[")) {
    try {
      raw = JSON.parse(t);
    } catch (e) {
      throw new Error(`Invalid vector JSON: ${(e as Error).message}`, { cause: e });
    }
  } else {
    raw = t.split(/[\s,]+/).filter(Boolean).map(Number);
  }
  if (!Array.isArray(raw) || raw.some((n) => typeof n !== "number" || !Number.isFinite(n))) throw new Error("A vector is a list of finite numbers");
  return raw as number[];
}

export function parseJsonObject(text: string, what: string): Record<string, unknown> | undefined {
  if (!text.trim()) return undefined;
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch (e) {
    throw new Error(`${what} is not valid JSON: ${(e as Error).message}`, { cause: e });
  }
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`${what} must be a JSON object`);
  return v as Record<string, unknown>;
}

export const fmt = (n: number, digits = 4) => (Number.isInteger(n) ? String(n) : n.toFixed(digits));
export const bytes = (n: number) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1048576).toFixed(1)} MiB`);
