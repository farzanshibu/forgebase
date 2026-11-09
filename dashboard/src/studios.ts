/** Admin sections from docs/SPEC.md section 17, with the roadmap phase (SPEC section 26) that delivers each. */
export interface Studio {
  path: string;
  label: string;
  summary: string;
  /** Roadmap phase that delivers it; `null` = live now. */
  phase: number | null;
  features: string[];
}

export const STUDIOS: Studio[] = [
  {
    path: "",
    label: "Dashboard",
    summary: "Live metrics overview",
    phase: 9,
    features: ["Requests per second", "Query latency", "Active and realtime connections", "Memory and CPU usage", "Storage and vector index statistics", "Background job status", "Error statistics"],
  },
  {
    path: "database",
    label: "Database Studio",
    summary: "Collections, tables, SQL",
    phase: null,
    features: ["Collection browser and table editor", "Spreadsheet-style viewer with inline editing", "Schema designer and index management", "SQL editor, query history, profiler, explain plans", "Import and export"],
  },
  {
    path: "redis",
    label: "Redis Studio",
    summary: "Keys, TTLs, streams",
    phase: 4,
    features: ["Key browser and search", "Value viewer and TTL management", "Pub/Sub inspector and stream browser", "Memory and cache statistics"],
  },
  {
    path: "storage",
    label: "Object Storage Studio",
    summary: "Buckets and files",
    phase: 4,
    features: ["Bucket management and file browser", "Upload, download and preview", "Metadata, access policies and versions", "Storage statistics"],
  },
  {
    path: "vector",
    label: "Vector Studio",
    summary: "Collections and similarity search",
    phase: null,
    features: ["Vector collections and metadata viewer", "Similarity search playground and filter builder", "Index configuration and rebuilding", "Search performance"],
  },
  {
    path: "graph",
    label: "Graph Studio",
    summary: "Nodes, edges, traversals",
    phase: null,
    features: ["Node and edge browsers", "Graph visualization", "Traversal query builder and path search"],
  },
  {
    path: "auth",
    label: "Authentication Studio",
    summary: "Users, sessions, API keys, audit",
    phase: null,
    features: ["Users, sessions and devices", "Service accounts, API keys and PATs", "Auth and authorization audit logs", "Admin impersonation"],
  },
  {
    path: "access",
    label: "Access Studio",
    summary: "Orgs, roles, permissions, rules",
    phase: null,
    features: ["Realms, organizations, projects and environments", "Members, teams, invitations", "Roles, role sets and the permission matrix", "Overrides, resource ACL and collection rules"],
  },
  {
    path: "identity",
    label: "Identity & Tenant Studio",
    summary: "Realms, client apps, branding",
    phase: null,
    features: ["Realms, tenant settings and identity providers (LDAP, SMS, Turnstile)", "Client apps with secret rotation, consents and OIDC key rotation", "Branding, custom domains with TXT verification", "Email templates with delivery log, legal agreements"],
  },
  {
    path: "functions",
    label: "Functions Studio",
    summary: "WASM functions and schedules",
    phase: null,
    features: ["WASM upload and deployment", "Version history and rollback", "Environment variables and secrets (names only)", "Live execution logs"],
  },
  {
    path: "jobs",
    label: "Jobs",
    summary: "Queues, jobs, schedules",
    phase: null,
    features: ["Queue statistics", "Job list with status filter and job detail", "Retry and cancel", "Schedules"],
  },
  {
    path: "billing",
    label: "Billing Studio",
    summary: "Plans, subscriptions, invoices, credits",
    phase: null,
    features: ["Catalog: products, prices, features, add-ons, usage packs", "Customers, subscriptions, orders and lifecycle actions", "Usage meters, invoices, payments, refunds, dunning", "Revenue metrics, credits and ledgers, providers"],
  },
  {
    path: "system",
    label: "System Studio",
    summary: "Health, version, storage",
    phase: null,
    features: ["Server, error and audit logs", "Database migrations", "Backups and restore", "Configuration"],
  },
];
