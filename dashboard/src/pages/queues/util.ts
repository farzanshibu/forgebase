/** Helpers shared by the Queues studio. */

/** Parses a JSON text box; `null` when it does not parse (the caller shows a message). */
export function parseJson(text: string): { ok: true; value: unknown } | { ok: false } {
  try {
    return { ok: true, value: JSON.parse(text) };
  } catch {
    return { ok: false };
  }
}

/** `key=value` lines to an attribute map; throws a readable `Error` on a bad line. */
export function parseAttrs(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const i = line.indexOf("=");
    if (i < 1) throw new Error(`Attribute lines must look like key=value, got "${line}".`);
    out[line.slice(0, i).trim()] = line.slice(i + 1);
  }
  return out;
}

/** 1500 -> "1.5 s", 90000 -> "1.5 min"; `null` -> "none". */
export function duration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "none";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${+(ms / 1000).toFixed(1)} s`;
  if (ms < 3_600_000) return `${+(ms / 60_000).toFixed(1)} min`;
  if (ms < 86_400_000) return `${+(ms / 3_600_000).toFixed(1)} h`;
  return `${+(ms / 86_400_000).toFixed(1)} d`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${+(n / 1024).toFixed(1)} KiB`;
  return `${+(n / 1024 / 1024).toFixed(1)} MiB`;
}

export const pretty = (v: unknown) => JSON.stringify(v, null, 2);
