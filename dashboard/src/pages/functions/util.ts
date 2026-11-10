import type { HookEventName, HookSpec } from "../../api";
import { HOOK_EVENTS } from "../../api";

/** Read a file's bytes (FileReader works in browsers and jsdom alike). */
export function readFileBytes(file: Blob): Promise<Uint8Array> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onerror = () => reject(r.error ?? new Error("Could not read the file"));
    r.onload = () => resolve(new Uint8Array(r.result as ArrayBuffer));
    r.readAsArrayBuffer(file);
  });
}

/** `KEY=value` per line; blank lines ignored. */
export function parseEnv(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (!line) continue;
    const eq = line.indexOf("=");
    if (eq < 1) throw new Error(`Environment line ${i + 1} must look like KEY=value`);
    out[line.slice(0, eq).trim()] = line.slice(eq + 1);
  }
  return out;
}
export const formatEnv = (env: Record<string, string>) => Object.entries(env).map(([k, v]) => `${k}=${v}`).join("\n");

/** `collection: before_create, after_create` per line. */
export function parseHooks(text: string): HookSpec[] {
  const out: HookSpec[] = [];
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.trim();
    if (!line) continue;
    const [collection, events] = line.split(":");
    const names = (events ?? "").split(",").map((e) => e.trim()).filter(Boolean);
    if (!collection?.trim() || names.length === 0) throw new Error(`Hook line ${i + 1} must look like collection: after_create, after_update`);
    const bad = names.find((n) => !HOOK_EVENTS.includes(n as HookEventName));
    if (bad) throw new Error(`Hook line ${i + 1}: unknown event ${bad}`);
    out.push({ collection: collection.trim(), events: names as HookEventName[] });
  }
  return out;
}
export const formatHooks = (hooks: HookSpec[]) => hooks.map((h) => `${h.collection}: ${h.events.join(", ")}`).join("\n");

export const csv = (text: string) => text.split(",").map((s) => s.trim()).filter(Boolean);

export function when(iso: string | null | undefined): string {
  if (!iso) return "never";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toISOString().replace("T", " ").replace(/\.\d+Z$/, "Z");
}

export interface CompileIssue { line: number; column: number; message: string }

/**
 * Line/column issues of a `400 function_compile_failed` answer. The envelope may carry `error.details` as
 * `{line, column}`, as a list under `errors`/`diagnostics`, or only the position inside the message ("3:14" or "line 3, column 14").
 */
export function compileIssues(err: unknown): CompileIssue[] {
  const e = err as { code?: unknown; message?: unknown; body?: unknown };
  if (!e || e.code !== "function_compile_failed") return [];
  const message = typeof e.message === "string" ? e.message : "Compile error";
  const details = (e.body as { error?: { details?: Record<string, unknown> } } | undefined)?.error?.details ?? {};
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) && v >= 1 ? Math.floor(v) : undefined);
  const one = (o: Record<string, unknown>, fallback: string): CompileIssue | undefined => {
    const line = num(o.line);
    if (line === undefined) return undefined;
    return { line, column: num(o.column ?? o.col) ?? 1, message: typeof o.message === "string" ? o.message : fallback };
  };
  for (const key of ["errors", "diagnostics"]) {
    const list = details[key];
    if (Array.isArray(list)) {
      const out = list.map((x) => (typeof x === "object" && x ? one(x as Record<string, unknown>, message) : undefined)).filter((x): x is CompileIssue => !!x);
      if (out.length) return out;
    }
  }
  const direct = one(details, message);
  if (direct) return [{ ...direct, message }];
  const m = /(?:line\s+)?(\d+)\s*(?::|,\s*(?:col(?:umn)?\s+))\s*(\d+)/i.exec(message);
  return m ? [{ line: Number(m[1]), column: Number(m[2]), message }] : [];
}
