import {
  Archive01Icon, Csv01Icon, Doc01Icon, File01Icon, GlobeIcon, Image01Icon, LockKeyIcon, MusicNote01Icon, Pdf01Icon, SourceCodeIcon,
  Txt01Icon, UserGroupIcon, Video01Icon, Xls01Icon,
} from "@hugeicons/core-free-icons";
import type { IconSvgElement } from "@hugeicons/react";
import type { Bucket, Visibility } from "../../api";

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


/** How each visibility reads in a list: an icon, a badge tone and the words. */
export const VISIBILITY_LOOK: Record<Visibility, { icon: IconSvgElement; tone: "secondary" | "info" | "warning"; text: string }> = {
  private: { icon: LockKeyIcon, tone: "secondary", text: "Only the owner and the access list" },
  authenticated: { icon: UserGroupIcon, tone: "info", text: "Any signed-in caller can read" },
  public: { icon: GlobeIcon, tone: "warning", text: "Anyone with the link can read" },
};

const BY_EXT: [RegExp, IconSvgElement][] = [
  [/\.(png|jpe?g|gif|webp|svg|avif|bmp|ico|heic)$/i, Image01Icon],
  [/\.pdf$/i, Pdf01Icon],
  [/\.(mp4|mov|webm|mkv|avi)$/i, Video01Icon],
  [/\.(mp3|wav|ogg|flac|m4a|aac)$/i, MusicNote01Icon],
  [/\.(zip|gz|tgz|tar|7z|rar|bz2|xz|zst)$/i, Archive01Icon],
  [/\.csv$/i, Csv01Icon],
  [/\.(xlsx?|ods)$/i, Xls01Icon],
  [/\.(docx?|odt|rtf)$/i, Doc01Icon],
  [/\.(txt|md|log)$/i, Txt01Icon],
  [/\.(json|ya?ml|toml|xml|html?|css|js|mjs|ts|tsx|jsx|rs|py|go|sh|sql)$/i, SourceCodeIcon],
];

/** An icon for a file, from its extension, then its content type. */
export function fileIcon(key: string, contentType = ""): IconSvgElement {
  for (const [re, icon] of BY_EXT) if (re.test(key)) return icon;
  const t = contentType.toLowerCase();
  if (t.startsWith("image/")) return Image01Icon;
  if (t.startsWith("video/")) return Video01Icon;
  if (t.startsWith("audio/")) return MusicNote01Icon;
  if (t === "application/pdf") return Pdf01Icon;
  if (t.startsWith("text/")) return Txt01Icon;
  if (t.includes("zip") || t.includes("compressed")) return Archive01Icon;
  return File01Icon;
}
