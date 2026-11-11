import type { Bucket } from "../../api";

/** The part of a key after its folder prefix. */
export function baseName(key: string, prefix = ""): string {
  const rest = key.startsWith(prefix) ? key.slice(prefix.length) : key;
  return rest.endsWith("/") ? rest.slice(0, -1) : rest;
}

/** Breadcrumb parts of a prefix: `a/b/` -> [{label:"a", prefix:"a/"}, {label:"b", prefix:"a/b/"}]. */
export function crumbs(prefix: string): { label: string; prefix: string }[] {
  const parts = prefix.split("/").filter(Boolean);
  return parts.map((label, i) => ({ label, prefix: `${parts.slice(0, i + 1).join("/")}/` }));
}

export const EXPIRY_PRESETS: { label: string; seconds: number }[] = [
  { label: "5 minutes", seconds: 300 },
  { label: "1 hour", seconds: 3600 },
  { label: "1 day", seconds: 86400 },
  { label: "7 days", seconds: 604800 },
];

/** One media type per line or comma separated; blank = no restriction. */
export function parseTypes(text: string): string[] | null {
  const out = text.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
  return out.length ? out : null;
}

/** Whole bytes from "10" with an optional unit (KiB, MiB, GiB); blank = no limit. Throws on garbage. */
export function parseBytes(text: string): number | null {
  const t = text.trim();
  if (!t) return null;
  const m = /^(\d+(?:\.\d+)?)\s*(b|kib|mib|gib|kb|mb|gb)?$/i.exec(t);
  if (!m) throw new Error("Enter a size like 5242880, 10 MiB or 2 GiB");
  const unit = (m[2] ?? "b").toLowerCase();
  const mult = unit === "b" ? 1 : unit.startsWith("k") ? 1024 : unit.startsWith("m") ? 1024 ** 2 : 1024 ** 3;
  return Math.round(Number(m[1]) * mult);
}

/** Trigger a browser download of `blob` under `name`. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

/** Where a bucket lives: one tenant, or the whole instance (FB-140: a tenant's buckets are stored as `<tenant>/<name>`). */
export const bucketScope = (b: Bucket) => (b.tenant ? { label: b.tenant, text: `Tenant ${b.tenant}` } : { label: "Instance", text: "Instance-wide: every tenant's rules apply to it" });

