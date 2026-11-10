/**
 * The breadcrumb trail for a route, and the hook a page uses to override it. Breadcrumbs.tsx draws it.
 * See Breadcrumbs.tsx for examples.
 */
import { createContext, useContext, useEffect } from "react";
import { useLocation } from "react-router-dom";
import { studioAt, studioHref, STUDIOS, type Studio } from "../studios";

export interface Crumb {
  label: string;
  /** Link target; the last crumb has none. */
  to?: string;
}

/** Detail-route kinds: `/<studio>/<kind>/<name>`, listed under the studio's list page. */
const KINDS: Record<string, Record<string, string>> = {
  database: { c: "Collections" },
  storage: { b: "Buckets" },
  functions: { f: "Functions" },
  vector: { c: "Collections" },
  queues: { t: "Topics", s: "Subscriptions" },
  jobs: { j: "Jobs", w: "Workflows" },
  access: { orgs: "Organizations" },
  auth: { users: "Users" },
};

/** Words for route segments that are not a studio tab. */
const SEGMENTS: Record<string, string> = {
  new: "New",
  schema: "Schema",
  indexes: "Indexes",
  transfer: "Import and export",
  versions: "Versions",
  logs: "Logs",
  secrets: "Secrets",
  settings: "Settings",
  objects: "Files",
  acl: "Access",
  share: "Signed URLs",
  search: "Search",
  stats: "Stats",
  members: "Members",
  teams: "Teams",
  invitations: "Invitations",
  roles: "Roles",
  "role-sets": "Role sets",
  overrides: "Overrides",
  projects: "Projects",
  features: "Features and settings",
};

const decode = (s: string) => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};

const words = (seg: string) => SEGMENTS[seg] ?? decode(seg).replace(/[-_]/g, " ").replace(/^./, (c) => c.toUpperCase());

/** The trail for a location. Exported for tests and for pages that want to extend it. */
export function crumbsFor(pathname: string): Crumb[] {
  const here = pathname.replace(/\/+$/, "") || "/";
  if (here === "/_patterns") return [{ label: "Shared patterns" }];
  const s = studioAt(here);
  if (!s) return [{ label: "Page not found" }];
  if (s.path === "") return [{ label: s.label }];
  const out: Crumb[] = [];
  // A nested nav item (System › Features and settings) sits under its parent studio.
  const parent = s.path.includes("/") ? STUDIOS.find((x) => x.path === s.path.split("/")[0]) : undefined;
  if (parent) out.push({ label: parent.label, to: studioHref(parent) });
  out.push({ label: s.label, to: studioHref(s) });
  const rest = here.slice(studioHref(s).length).split("/").filter(Boolean);
  if (rest.length === 0) return finish(out);
  const tab = longestTab(s, here);
  const kinds = KINDS[s.path] ?? {};
  let i = 0;
  if (tab && tab.to !== studioHref(s)) {
    out.push({ label: tab.label, to: tab.to });
    i = tab.to.slice(studioHref(s).length).split("/").filter(Boolean).length;
  }
  let base = tab && tab.to !== studioHref(s) ? tab.to : studioHref(s);
  while (i < rest.length) {
    const seg = rest[i]!;
    const kind = kinds[seg];
    if (kind && rest[i + 1] !== undefined) {
      const name = rest[i + 1]!;
      const listTab = s.tabs?.find((t) => t.label === kind);
      out.push({ label: kind, to: listTab?.to ?? studioHref(s) });
      base = `${base}/${seg}/${name}`;
      out.push({ label: decode(name), to: base });
      i += 2;
      continue;
    }
    base = `${base}/${seg}`;
    out.push({ label: words(seg), to: base });
    i += 1;
  }
  return finish(out);
}

function longestTab(s: Studio, here: string) {
  let best: { to: string; label: string } | undefined;
  for (const t of s.tabs ?? []) {
    if ((here === t.to || here.startsWith(`${t.to}/`)) && (!best || t.to.length > best.to.length)) best = t;
  }
  return best;
}

/** The current page is the last crumb, without a link; drop repeats such as "Functions › Functions". */
export function finish(list: Crumb[]): Crumb[] {
  const out: Crumb[] = [];
  for (const c of list) if (!out.length || out[out.length - 1]!.label !== c.label) out.push(c);
  const last = out[out.length - 1];
  if (last) out[out.length - 1] = { label: last.label };
  return out;
}

export type CrumbOverride = { path: string; crumbs: Crumb[] } | undefined;
export const CrumbContext = createContext<{ override: CrumbOverride; set: (o: CrumbOverride) => void } | null>(null);

/** Replace the derived trail while this component is mounted. Pass a stable array (useMemo) or plain literals. */
export function useCrumbs(crumbs: Crumb[] | undefined) {
  const ctx = useContext(CrumbContext);
  const { pathname } = useLocation();
  const key = crumbs ? JSON.stringify(crumbs) : "";
  const set = ctx?.set;
  useEffect(() => {
    if (!set || !key) return;
    set({ path: pathname, crumbs: JSON.parse(key) as Crumb[] });
    return () => set(undefined);
  }, [set, key, pathname]);
}

