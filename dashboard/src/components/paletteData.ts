/**
 * What the command palette lists besides studios: resources fetched when it opens (cached 30 s) and the keys that
 * open it. Kept apart from CommandPalette.tsx so that file exports components only.
 */
import { listBuckets, listCollections, listFunctions, listQueueTopics, listVectorCollections } from "../api";

export interface PaletteResource {
  /** "Collection", "Bucket", … */
  kind: string;
  name: string;
  to: string;
}

const SOURCES: { studio: string; kind: string; load: (signal: AbortSignal) => Promise<{ name: string }[]>; to: (name: string) => string }[] = [
  { studio: "database", kind: "Collection", load: (s) => listCollections(s), to: (n) => `/database/c/${encodeURIComponent(n)}` },
  { studio: "storage", kind: "Bucket", load: (s) => listBuckets(s), to: (n) => `/storage/b/${encodeURIComponent(n)}` },
  { studio: "functions", kind: "Function", load: (s) => listFunctions(s), to: (n) => `/functions/f/${encodeURIComponent(n)}` },
  { studio: "queues", kind: "Topic", load: (s) => listQueueTopics(s), to: (n) => `/queues/t/${encodeURIComponent(n)}` },
  { studio: "vector", kind: "Vector collection", load: (s) => listVectorCollections(s), to: (n) => `/vector/c/${encodeURIComponent(n)}` },
];

export const RESOURCE_TTL_MS = 30_000;
let cache: { at: number; key: string; items: PaletteResource[] } | undefined;

/** The cached list, if any (shown at once when the palette reopens). */
export function cachedResources(): PaletteResource[] | undefined {
  return cache?.items;
}

/** Test hook: forget cached resources. */
export function resetPaletteCache() {
  cache = undefined;
}

/** Resources of the visible studios; a source that fails (no permission, feature off) is skipped. */
export async function loadResources(studios: string[], signal: AbortSignal): Promise<PaletteResource[]> {
  const key = studios.join(",");
  if (cache && cache.key === key && Date.now() - cache.at < RESOURCE_TTL_MS) return cache.items;
  const wanted = SOURCES.filter((s) => studios.includes(s.studio));
  const results = await Promise.allSettled(wanted.map((s) => s.load(signal).then((list) => list.map((r) => ({ kind: s.kind, name: r.name, to: s.to(r.name) })))));
  const items = results.flatMap((r) => (r.status === "fulfilled" ? r.value : []));
  if (!signal.aborted) cache = { at: Date.now(), key, items };
  return items;
}

export const isMac = () => typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** The keys that open the palette, for labels: ["⌘", "K"] or ["Ctrl", "K"]. */
export const paletteKeys = () => (isMac() ? ["⌘", "K"] : ["Ctrl", "K"]);
