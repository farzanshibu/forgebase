/** Split a console line into arguments: whitespace separated, with "double" or 'single' quotes and backslash escapes inside double quotes. */
export function tokenize(line: string): string[] {
  const out: string[] = [];
  let cur = "", has = false, q: string | null = null;
  for (let i = 0; i < line.length; i++) {
    const c = line[i]!;
    if (q) {
      if (c === q) q = null;
      else if (c === "\\" && q === '"' && i + 1 < line.length) { const n = line[++i]!; cur += n === "n" ? "\n" : n === "t" ? "\t" : n; }
      else cur += c;
    } else if (c === '"' || c === "'") { q = c; has = true; }
    else if (/\s/.test(c)) { if (has || cur) { out.push(cur); cur = ""; has = false; } }
    else cur += c;
  }
  if (q) throw new Error("Unbalanced quote");
  if (has || cur) out.push(cur);
  return out;
}

/** A command reply as redis-cli shows it. */
export function formatReply(v: unknown, depth = 0): string {
  const pad = "  ".repeat(depth);
  if (v === null || v === undefined) return `${pad}(nil)`;
  if (typeof v === "number") return `${pad}(integer) ${v}`;
  if (typeof v === "string") return `${pad}${JSON.stringify(v)}`;
  if (Array.isArray(v)) return v.length === 0 ? `${pad}(empty array)` : v.map((x, i) => `${pad}${i + 1}) ${formatReply(x, depth + 1).trimStart()}`).join("\n");
  if (typeof v === "object" && "base64" in v) return `${pad}base64:${String((v as { base64: string }).base64)}`;
  return `${pad}${JSON.stringify(v)}`;
}

/** `Nms`, `Ns`, `Nm`, `Nh`, `Nd` for a remaining TTL; `no expiry` when absent. */
export function formatTtl(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "no expiry";
  if (ms < 1000) return `${ms} ms`;
  const s = Math.round(ms / 1000);
  if (s < 120) return `${s} s`;
  if (s < 7200) return `${Math.round(s / 60)} min`;
  if (s < 172800) return `${Math.round(s / 3600)} h`;
  return `${Math.round(s / 86400)} d`;
}

/** `name=value` per line (the first `=` splits); blank lines skipped. */
export function parseFields(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (!line) continue;
    const eq = line.indexOf("=");
    if (eq < 1) throw new Error(`Line ${i + 1} must look like name=value`);
    out[line.slice(0, eq)] = line.slice(eq + 1);
  }
  if (Object.keys(out).length === 0) throw new Error("Enter at least one name=value line");
  return out;
}

export const TTL_UNITS: { label: string; ms: number }[] = [
  { label: "seconds", ms: 1000 }, { label: "minutes", ms: 60_000 }, { label: "hours", ms: 3_600_000 }, { label: "days", ms: 86_400_000 },
];

/** A copy of `o` without `key`. */
export function omit<T>(o: Record<string, T>, key: string): Record<string, T> {
  return Object.fromEntries(Object.entries(o).filter(([k]) => k !== key));
}
