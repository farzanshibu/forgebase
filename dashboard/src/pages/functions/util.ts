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
