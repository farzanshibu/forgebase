/**
 * Scopes of API keys and personal tokens, so a developer never has to know the grammar (audit A-08).
 *
 * The server's grammar (crates/forgebase-auth/src/scopes.rs): `*` | `<area>:*` | `<area>:read|write` |
 * `<area>:read|write:<name>`. The area is the path segment after /api/v1/ (`collections`, `storage`, …) and the name is
 * the resource in it (a collection, a bucket). `write` does not imply `read`.
 *
 * Usage:
 *   buildScopes([{ area: "collections", read: true, write: false, only: ["notes"] }])   // ["collections:read:notes"]
 *   describeScope("collections:read:notes")                                            // "Read records in notes"
 *   firstReadExample(scopes)                                                           // { path: "/api/v1/collections/notes/records", … }
 */

export type Access = "read" | "write";

export interface ScopeArea {
  /** The scope's area: the path segment after /api/v1/. */
  id: string;
  /** Plain name shown in the dialog. */
  label: string;
  /** What one resource of this area is called, in the singular: "collection". */
  resource: string;
  /** What read and write let a key do, in words. */
  read: string;
  write: string;
}

export const AREAS: readonly ScopeArea[] = [
  { id: "collections", label: "Records", resource: "collection", read: "Read records", write: "Create, change and delete records" },
  { id: "storage", label: "Files", resource: "bucket", read: "Download and list files", write: "Upload and delete files" },
  { id: "functions", label: "Functions", resource: "function", read: "View functions and their logs", write: "Deploy and run functions" },
  { id: "kv", label: "Key-value", resource: "namespace", read: "Read keys", write: "Set and delete keys" },
  { id: "vector", label: "Vector", resource: "collection", read: "Search and read embeddings", write: "Add and delete embeddings" },
  { id: "graph", label: "Graph", resource: "graph", read: "Read nodes and edges", write: "Add and change nodes and edges" },
  { id: "queues", label: "Queues", resource: "topic", read: "Read topics and messages", write: "Publish messages and manage topics" },
  { id: "jobs", label: "Jobs", resource: "job kind", read: "See jobs and schedules", write: "Enqueue and manage jobs" },
];

/** One area's choice in the dialog. `only` empty means every resource of the area, unless `narrow` is set: then it means "not picked yet". */
export interface AreaChoice {
  area: string;
  read: boolean;
  write: boolean;
  only: string[];
  /** "All" was unticked: the person is picking resources. With none picked the area grants nothing, never everything. */
  narrow: boolean;
}

export const emptyChoice = (area: string): AreaChoice => ({ area, read: false, write: false, only: [], narrow: false });

/** The scope strings for the choices, in a stable order. */
export function buildScopes(choices: readonly AreaChoice[]): string[] {
  const out: string[] = [];
  for (const c of choices) {
    if (c.narrow && c.only.length === 0) continue;
    for (const access of ["read", "write"] as const) {
      if (!c[access]) continue;
      if (c.only.length === 0) out.push(`${c.area}:${access}`);
      else for (const name of c.only) out.push(`${c.area}:${access}:${name}`);
    }
  }
  return out;
}

/** The choices a list of scope strings stands for; strings the dialog cannot show (`*`, `auth:read`) are ignored. */
export function parseScopes(scopes: readonly string[]): AreaChoice[] {
  const by = new Map<string, AreaChoice>();
  for (const s of scopes) {
    const [area, access, name] = s.split(":");
    if (!area || (access !== "read" && access !== "write")) continue;
    const c = by.get(area) ?? emptyChoice(area);
    c[access] = true;
    if (name) {
      c.narrow = true;
      if (!c.only.includes(name)) c.only.push(name);
    }
    by.set(area, c);
  }
  return [...by.values()];
}

/** A scope in plain words: `collections:read:notes` reads "Read records in notes". */
export function describeScope(scope: string): string {
  if (scope === "*") return "Full access to everything";
  const [area, access, name] = scope.split(":");
  const a = AREAS.find((x) => x.id === area);
  if (!a) return scope;
  if (access === "*") return `Everything in ${a.label.toLowerCase()}`;
  if (access !== "read" && access !== "write") return scope;
  const what = access === "read" ? a.read : a.write;
  return name ? `${what} in ${name}` : `${what}, all ${a.resource}s`;
}

/** A request the key can make, for the curl example next to the secret. `null` when the key reads nothing. */
export function readExample(scopes: readonly string[], url: string): { what: string; curl: string } | null {
  const enc = encodeURIComponent;
  for (const s of scopes) {
    const [area, access, name] = s.split(":");
    if (area === "collections" && (access === "read" || access === "*")) {
      const c = name ?? "<collection>";
      return { what: `List records of ${c}`, curl: `curl -H "X-API-Key: <YOUR_API_KEY>" \\\n  "${url}/api/v1/collections/${name ? enc(name) : "<collection>"}/records?per_page=5"` };
    }
  }
  for (const s of scopes) {
    const [area, access] = s.split(":");
    if (area === "storage" && (access === "read" || access === "*")) return { what: "List buckets", curl: `curl -H "X-API-Key: <YOUR_API_KEY>" \\\n  "${url}/api/v1/storage/buckets"` };
    if (area === "functions" && (access === "read" || access === "*")) return { what: "List functions", curl: `curl -H "X-API-Key: <YOUR_API_KEY>" \\\n  "${url}/api/v1/functions"` };
  }
  return null;
}

/** Expiry choices, in days; `0` means the key never expires. */
export const EXPIRIES: { value: number; label: string }[] = [
  { value: 30, label: "30 days" },
  { value: 90, label: "90 days" },
  { value: 365, label: "1 year" },
  { value: 0, label: "Never expires" },
];
