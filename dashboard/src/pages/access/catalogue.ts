/** Catalogue of `resource:action` permission keys (ADR-0008 section 5) behind the permission matrix. */
export interface ResourceRow {
  resource: string;
  actions: string[];
}

export const STATIC_RESOURCES: ResourceRow[] = [
  { resource: "org", actions: ["read", "update", "delete"] },
  { resource: "users", actions: ["read", "invite", "update", "delete"] },
  { resource: "teams", actions: ["read", "create", "delete", "members:manage"] },
  { resource: "projects", actions: ["read", "create", "update", "delete", "share"] },
  { resource: "roles", actions: ["read", "manage"] },
  { resource: "acl", actions: ["manage"] },
  { resource: "billing", actions: ["manage"] },
  { resource: "api_keys", actions: ["revoke"] },
  { resource: "analytics", actions: ["export"] },
  { resource: "ai", actions: ["generate"] },
  { resource: "schema", actions: ["read"] },
  { resource: "engine", actions: ["kv_read", "kv_write", "graph_read", "graph_write"] },
];

export const DATA_ACTIONS = ["list", "view", "create", "update", "delete"];

/** Static resources plus one `data:<collection>` row per collection. */
export function catalogue(collections: string[]): ResourceRow[] {
  return [...STATIC_RESOURCES, ...collections.map((c) => ({ resource: `data:${c}`, actions: DATA_ACTIONS }))];
}

export const keyOf = (resource: string, action: string) => `${resource}:${action}`;

/** All keys of a catalogue. */
export function allKeys(rows: ResourceRow[]): Set<string> {
  return new Set(rows.flatMap((r) => r.actions.map((a) => keyOf(r.resource, a))));
}

export type Cell = "inherit" | "allow" | "deny";
export const NEXT: Record<Cell, Cell> = { inherit: "allow", allow: "deny", deny: "inherit" };

/** Split role lists into the matrix's exact-key cells and the patterns it cannot show (`*`, unknown keys). */
export function toCells(allow: string[], deny: string[], keys: Set<string>) {
  const cells: Record<string, Cell> = {};
  const customAllow: string[] = [];
  const customDeny: string[] = [];
  for (const p of allow) {
    if (keys.has(p)) cells[p] = "allow";
    else customAllow.push(p);
  }
  for (const p of deny) {
    if (keys.has(p)) cells[p] = "deny";
    else customDeny.push(p);
  }
  return { cells, customAllow, customDeny };
}

/** Inverse of `toCells`: cell states plus custom patterns back into allow/deny lists. */
export function fromCells(cells: Record<string, Cell>, customAllow: string[], customDeny: string[]) {
  const allow = [...customAllow];
  const deny = [...customDeny];
  for (const [k, v] of Object.entries(cells).sort(([a], [b]) => a.localeCompare(b))) {
    if (v === "allow") allow.push(k);
    else if (v === "deny") deny.push(k);
  }
  return { allow, deny };
}
