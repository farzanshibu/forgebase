/**
 * One record per studio. The nav rail, the breadcrumbs, PageHeader and the command palette all read it, so a
 * studio has one name everywhere (dashboard-ux.md rule 8, audit A-14).
 *
 * Usage:
 *   const s = studio("queues");                 // { label: "Queues", title, summary, group: "build", docs, tabs }
 *   <PageHeader studio="queues" primary={<Button tone="primary">New topic</Button>} />
 *   visibleStudios(features)                    // what the nav and the palette show (disabled features hidden)
 *   docsHref("docs/database.md", "0.3.0")       // GitHub docs at v0.3.0 (decision Q2)
 */
import type { ServerFeature } from "./api";
import type { GlossaryTerm } from "./glossary";

export type StudioGroup = "build" | "users" | "operate";

export interface StudioTab {
  /** Route, with a leading slash. */
  to: string;
  label: string;
}

export interface Studio {
  /** Route without a leading slash; "" is Overview. */
  path: string;
  /** Short name: nav rail, breadcrumbs, palette. */
  label: string;
  /** PageHeader heading. */
  title: string;
  /** One plain sentence on what it does: PageHeader description, palette subtitle. */
  summary: string;
  /** Nav group; `null` for Overview, which sits above the groups. */
  group: StudioGroup | null;
  /** Repo-relative docs page, linked by PageHeader through `docsHref`. */
  docs: string;
  /** The fb92 feature id that switches it on and off (GET /api/system/features). */
  feature?: string;
  /** The glossary term PageHeader explains next to the title. */
  term?: GlossaryTerm;
  /** Sections, for the palette's "Go to" list. Pages keep their own RouteTabs. */
  tabs?: StudioTab[];
}

export const GROUPS: { id: StudioGroup; label: string }[] = [
  { id: "build", label: "Build" },
  { id: "users", label: "Users and access" },
  { id: "operate", label: "Operate" },
];

export const STUDIOS: Studio[] = [
  { path: "", label: "Overview", title: "Overview", summary: "Health, usage and the first steps for a new project.", group: null, docs: "docs/sdk.md" },
  // Build
  {
    path: "database", label: "Database", title: "Database", group: "build", docs: "docs/database.md",
    summary: "Collections of records with a schema, plus SQL, indexes, slow queries and, on PostgreSQL, policies, direct logins and extensions.",
    tabs: [{ to: "/database", label: "Collections" }, { to: "/database/sql", label: "SQL editor" }, { to: "/database/indexes", label: "Indexes" }, { to: "/database/slow", label: "Slow queries" }, { to: "/database/policies", label: "Policies" }, { to: "/database/logins", label: "Direct logins" }, { to: "/database/extensions", label: "Extensions" }],
  },
  { path: "storage", label: "Storage", title: "Storage", group: "build", docs: "docs/adr/0018-object-storage.md", feature: "storage", summary: "Buckets that hold the files your app uploads, such as avatars or exports." },
  { path: "functions", label: "Functions", title: "Functions", term: "edge_function", group: "build", docs: "docs/adr/0017-functions.md", feature: "functions", summary: "Edge functions: your code, run next to your data on each request." },
  {
    path: "redis", label: "Key-value", title: "Key-value", group: "build", docs: "docs/adr/0005-kv.md", feature: "kv",
    summary: "Redis compatible key-value store for caches, counters and sessions.",
    tabs: [{ to: "/redis", label: "Keys" }, { to: "/redis/console", label: "Console" }, { to: "/redis/pubsub", label: "Pub/Sub" }, { to: "/redis/stats", label: "Memory and stats" }, { to: "/redis/rules", label: "Rules" }],
  },
  { path: "vector", label: "Vector", title: "Vector", term: "embedding", group: "build", docs: "docs/adr/0012-vector.md", feature: "vector", summary: "Store embeddings and find the most similar ones." },
  {
    path: "graph", label: "Graph", title: "Graph", term: "node", group: "build", docs: "docs/adr/0006-graph.md", feature: "graph",
    summary: "Nodes and the edges between them, with traversals and path search.",
    tabs: [{ to: "/graph", label: "Graphs" }, { to: "/graph/nodes", label: "Nodes" }, { to: "/graph/edges", label: "Edges" }, { to: "/graph/explorer", label: "Explorer" }, { to: "/graph/query", label: "Query runner" }],
  },
  {
    path: "queues", label: "Queues", title: "Queues", term: "topic", group: "build", docs: "docs/adr/0022-queues-pubsub.md", feature: "queues",
    summary: "Durable topics and subscriptions that carry messages between services.",
    tabs: [{ to: "/queues", label: "Topics" }, { to: "/queues/subscriptions", label: "Subscriptions" }, { to: "/queues/inspector", label: "Pub/Sub inspector" }],
  },
  {
    path: "jobs", label: "Jobs", title: "Jobs", term: "job", group: "build", docs: "docs/adr/0007-jobs.md", feature: "jobs",
    summary: "Background jobs with retries and progress, workflows and schedules.",
    tabs: [{ to: "/jobs", label: "Queues and jobs" }, { to: "/jobs/workflows", label: "Workflows" }, { to: "/jobs/schedules", label: "Schedules" }],
  },
  // Users and access
  {
    path: "auth", label: "Authentication", title: "Authentication", term: "api_key", group: "users", docs: "docs/adr/0004-auth.md",
    summary: "Users, sessions, API keys and the audit log.",
    tabs: [{ to: "/auth", label: "Users" }, { to: "/auth/service-accounts", label: "Service accounts" }, { to: "/auth/keys", label: "API keys" }, { to: "/auth/audit", label: "Audit log" }, { to: "/auth/admins", label: "Admins" }],
  },
  {
    path: "access", label: "Access", title: "Access", term: "role", group: "users", docs: "docs/adr/0008-policy.md", feature: "access",
    summary: "Organizations, roles and the rules that decide who can do what.",
    tabs: [{ to: "/access", label: "Organizations" }, { to: "/access/realm", label: "Realm" }, { to: "/access/rules", label: "Collection rules" }, { to: "/access/tester", label: "Rule tester" }],
  },
  {
    path: "identity", label: "Identity", title: "Identity and tenants", term: "tenant", group: "users", docs: "docs/adr/0019-identity-provider.md", feature: "identity",
    summary: "Realms, client apps, sign-in providers, branding and legal agreements.",
    tabs: [
      { to: "/identity", label: "Realms" }, { to: "/identity/providers", label: "Identity providers" }, { to: "/identity/clients", label: "Client apps" },
      { to: "/identity/consents", label: "Consents and keys" }, { to: "/identity/branding", label: "Branding" }, { to: "/identity/domains", label: "Custom domains" },
      { to: "/identity/email", label: "Email templates" }, { to: "/identity/legal", label: "Legal agreements" },
    ],
  },
  {
    path: "billing", label: "Billing", title: "Billing", term: "entitlement", group: "users", docs: "docs/adr/0021-billing.md", feature: "billing",
    summary: "Plans, subscriptions, invoices, payments and credits.",
    tabs: [{ to: "/billing/catalog", label: "Catalog" }, { to: "/billing/customers", label: "Customers" }, { to: "/billing", label: "Money" }, { to: "/billing/settings", label: "Settings" }],
  },
  // Operate
  {
    path: "system", label: "System", title: "System", group: "operate", docs: "docs/deployment.md",
    summary: "Server health, version and backups.",
    tabs: [{ to: "/system", label: "Health" }, { to: "/system/topology", label: "Database topology" }, { to: "/system/backups", label: "Backups" }],
  },
  { path: "system/features", label: "Features and settings", title: "Features and settings", group: "operate", docs: "docs/adr/0026-first-run-setup-and-feature-configuration.md", summary: "Switch server features on or off and change their settings." },
];

/** Actions the palette offers under "Create". Each goes to the studio's create route; `?new=1` asks the page to open its CreateSheet (see `useCreateParam`). */
export const CREATE_ACTIONS: { label: string; to: string; studio: string; keywords?: string[] }[] = [
  { label: "New collection", to: "/database/new", studio: "database", keywords: ["table", "schema"] },
  { label: "New bucket", to: "/storage?new=1", studio: "storage", keywords: ["upload", "files"] },
  { label: "New function", to: "/functions/new", studio: "functions", keywords: ["edge", "deploy", "code"] },
  { label: "Create API key", to: "/auth/keys?new=1", studio: "auth", keywords: ["token", "secret"] },
  { label: "New topic", to: "/queues?new=1", studio: "queues", keywords: ["queue", "publish"] },
];

/** The studio record for a path ("" for Overview). Throws on an unknown path so a typo fails in tests. */
export function studio(path: string): Studio {
  const s = STUDIOS.find((x) => x.path === path);
  if (!s) throw new Error(`Unknown studio "${path}"`);
  return s;
}

/** The route of a studio's main page. */
export const studioHref = (s: Studio) => (s.path === "" ? "/" : `/${s.path}`);

/** The studio a location belongs to: the longest matching path, Overview for "/". */
export function studioAt(pathname: string): Studio | undefined {
  const here = pathname.replace(/\/+$/, "") || "/";
  if (here === "/") return STUDIOS[0];
  let best: Studio | undefined;
  for (const s of STUDIOS) {
    if (s.path === "") continue;
    const p = `/${s.path}`;
    if ((here === p || here.startsWith(`${p}/`)) && (!best || s.path.length > best.path.length)) best = s;
  }
  return best;
}

/**
 * Whether the server switched this studio off (FB-92): its own `feature` is off, or every feature the server maps to
 * its path is off. Several features can share a path: `kv` (always on) and `resp` (the Redis wire protocol, often
 * off) both point at /redis, and the studio must stay while the key-value store runs. A missing list counts as on.
 */
export function isStudioDisabled(s: Studio, features: ServerFeature[] | undefined): boolean {
  const list = features ?? [];
  const on = (f: ServerFeature) => f.enabled || f.always_on;
  if (s.feature) {
    const own = list.find((f) => f.id === s.feature);
    if (own) return !on(own);
  }
  const atPath = list.filter((f) => (f.studio_path ?? "").replace(/^\/+|\/+$/g, "") === s.path);
  return atPath.length > 0 && atPath.every((f) => !on(f));
}

/** Studios that change the whole server: a realm administrator (FB-137) does not see them (their pages say "for platform administrators"). */
export const PLATFORM_ONLY: readonly string[] = ["billing", "system", "system/features"];

/**
 * The studios the nav and the palette show: every studio whose feature is on. A realm administrator (`platform:
 * false`) does not get the platform-only ones (Billing, System, Features and settings).
 */
export function visibleStudios(features: ServerFeature[] | undefined, opts: { platform?: boolean } = {}): Studio[] {
  const platform = opts.platform ?? true;
  return STUDIOS.filter((s) => !isStudioDisabled(s, features) && (platform || !PLATFORM_ONLY.includes(s.path)));
}

export const DOCS_REPO = "https://github.com/farzanshibu/forgebase";

/**
 * GitHub docs at the running version's tag, or `main` for a dev build or an unknown version (decision Q2).
 * `doc` is repo-relative, with an optional #anchor.
 */
export function docsHref(doc: string, version?: string): string {
  const release = version && /^\d+\.\d+\.\d+$/.test(version) ? `v${version}` : "main";
  return `${DOCS_REPO}/blob/${release}/${doc.replace(/^\/+/, "")}`;
}
