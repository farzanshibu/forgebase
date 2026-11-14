/**
 * One plain definition per term the studios use (dashboard-ux.md rule 8). HelpTip reads it.
 *
 * Usage:
 *   <HelpTip term="ttl" />                        // "?" button after a label
 *   <HelpTip term="rls">Row-level security</HelpTip>
 *   GLOSSARY.ttl.short                            // "TTL"
 *
 * Add a term here, never inline in a page, so the same word has the same meaning everywhere.
 */

export interface GlossaryEntry {
  /** The term as it appears on screen. */
  short: string;
  /** One or two plain sentences. */
  text: string;
}

export const GLOSSARY = {
  // Database
  rls: { short: "RLS", text: "Row-level security: rules that decide, record by record, who may read or change it." },
  schema_mode: { short: "Schema mode", text: "Strict rejects fields the collection does not define. Flexible stores them as extra JSON." },
  base: { short: "Base collection", text: "A normal collection of records you create, read, update and delete." },
  auth_collection: { short: "Auth collection", text: "A collection of user accounts: each record can sign in with a password, a link or a provider." },
  view: { short: "View collection", text: "A read-only collection defined by a SQL query over other collections." },
  index: { short: "Index", text: "A sorted copy of one or more fields that makes filters and sorts on them fast." },
  explain: { short: "Explain plan", text: "How the database will run a query: which indexes it uses and which tables it scans in full." },
  direct_login: { short: "Direct login", text: "A PostgreSQL login for one user or service account, for tools such as psql. It sees only what that principal may see." },
  native_policy: { short: "Policy", text: "A PostgreSQL row-level security policy. ForgeBase generates the fb_* ones from your rules; any other is hand-written." },
  // Access
  acl: { short: "ACL", text: "Access control list: who may read or write one resource, such as a bucket or a prefix in it." },
  role: { short: "Role", text: "A named set of permissions you give to members, teams or service accounts." },
  scope: { short: "Scope", text: "A permission a key or token carries, such as collections:read. A request needs the scope for what it does." },
  service_account: { short: "Service account", text: "An identity for a program, not a person. It signs in with API keys." },
  api_key: { short: "API key", text: "A secret your app or server sends in the X-API-Key header. It carries scopes that limit what it can do." },
  pat: { short: "PAT", text: "Personal access token: an API key tied to one person's account." },
  oidc: { short: "OIDC", text: "OpenID Connect: the standard sign-in protocol ForgeBase speaks to client apps and identity providers." },
  tenant: { short: "Tenant", text: "One customer or team inside a realm. A tenant's keys, buckets, topics and jobs are its own; an administrator can see all of them." },
  consent: { short: "Consent", text: "A person's recorded yes to let a client app use their account for the scopes it asks for. They can withdraw it at any time." },
  role_set: { short: "Role set", text: "A named bundle of roles you give to a member or team in one step. A deny in any role of the set still wins." },
  realm: { short: "Realm", text: "A separate space of users, settings and branding. Each tenant usually has its own realm." },
  mfa: { short: "MFA", text: "Multi-factor authentication: a second proof, such as a one-time code, on top of the password." },
  impersonation: { short: "Impersonation", text: "Acting as another user to see what they see. Every action is logged under your name." },
  // Storage
  versioning: { short: "Versioning", text: "Keep every earlier version of a file when it is replaced or deleted, so it can be restored." },
  signed_url: { short: "Signed URL", text: "A link that works without a key until it expires. Share it to let someone download or upload one file." },
  reconcile: { short: "Reconcile", text: "Compare what the database says is stored with what is on disk, and report or fix the difference." },
  multipart: { short: "Multipart upload", text: "Upload a big file in parts, so a dropped connection only resends the part it was on." },
  // Functions
  edge_function: { short: "Edge function", text: "Your code, run by the server next to your data when a request or an event calls it." },
  wasm: { short: "WebAssembly", text: "A compact program format the server runs safely in a sandbox. Functions written in TypeScript compile to it." },
  fuel: { short: "Fuel", text: "A limit on how much work one function call may do before the server stops it." },
  capability: { short: "Capability", text: "Something a function is allowed to reach, such as the database, storage or outgoing HTTP." },
  principal: { short: "Principal", text: "Who a request acts as: a user, an API key, a service account or a function." },
  secret: { short: "Secret", text: "A value such as a password or token that a function can read at run time. The dashboard shows names only." },
  // Queues and jobs
  topic: { short: "Topic", text: "A named stream that publishers send messages to." },
  subscription: { short: "Subscription", text: "A reader of a topic. Each subscription gets its own copy of every message." },
  ack_deadline: { short: "Ack deadline", text: "How long a reader has to confirm (acknowledge) a message before it is delivered again." },
  dead_letter: { short: "Dead-letter topic", text: "Where a message goes after it fails too many times, so it can be looked at instead of lost." },
  ordering_key: { short: "Ordering key", text: "Messages with the same key are delivered in the order they were published." },
  dedup_id: { short: "Dedup id", text: "A unique id per message. A second message with the same id is dropped." },
  backlog: { short: "Backlog", text: "Messages published but not yet acknowledged by this subscription." },
  job: { short: "Job", text: "A unit of background work, run by a worker, with retries, progress and a result." },
  workflow: { short: "Workflow", text: "Jobs joined into steps, where each step starts when the ones it depends on finish." },
  cron: { short: "Cron", text: "A schedule written as minute, hour, day of month, month and weekday, such as 0 3 * * * for 03:00 daily." },
  reserved_kind: { short: "Reserved kind", text: "A job or queue kind whose name starts with a prefix the server keeps for its own work. Only an administrator can enqueue it; anyone else gets 403." },
  idempotency_key: { short: "Idempotency key", text: "A unique key per request, so sending it twice does the work only once." },
  redrive: { short: "Redrive", text: "Deliver dead letters to the subscription again, with a fresh attempt counter. Each dead letter is redriven once." },
  signing_secret: { short: "Signing secret", text: "The secret ForgeBase signs each webhook with. Your receiver checks the X-Forgebase-Signature header against it to know the call is genuine." },
  grace_window: { short: "Grace window", text: "After a rotation the old secret keeps signing for a while, next to the new one, so you can deploy the new secret without dropping a delivery." },
  circuit_breaker: { short: "Auto-pause", text: "Stops delivering to an endpoint after this many failed attempts in a row, so a dead endpoint is not hammered. Messages wait and nothing is dead-lettered until you resume." },
  push_attempt: { short: "Attempt", text: "One call to the webhook or function: its status, how long it took and what the endpoint answered. Test sends are logged too, marked as tests." },
  event_catalog: { short: "Event catalog", text: "The kinds of event you can subscribe to: each topic with its schema and an example, and each record source with created, updated and deleted events." },
  // Key-value
  ttl: { short: "TTL", text: "Time to live: how long a key is kept before the server deletes it on its own." },
  namespace: { short: "Namespace", text: "A separate set of keys, so two apps or features never overwrite each other's keys." },
  pubsub: { short: "Pub/Sub", text: "Publish and subscribe: messages sent to a channel reach everyone listening to it, right away." },
  resp: { short: "Redis protocol", text: "The wire protocol Redis clients speak. Point any Redis client at ForgeBase's port." },
  // Vector and graph
  embedding: { short: "Embedding", text: "A list of numbers that stands for the meaning of a text or image. Similar things have nearby embeddings." },
  dimension: { short: "Dimension", text: "How many numbers each embedding has. It must match the model that made the embeddings." },
  metric: { short: "Metric", text: "How distance between embeddings is measured: cosine, dot product or Euclidean (L2)." },
  knn: { short: "k", text: "How many nearest results a similarity search returns." },
  exact_search: { short: "Exact search", text: "Compare the query with every stored embedding. Slow on big collections but never misses." },
  hybrid_search: { short: "Hybrid search", text: "Combine similarity search with a keyword search and merge the two rankings." },
  hnsw: { short: "HNSW index", text: "A fast approximate index for similarity search. Higher settings find better matches but use more memory." },
  node: { short: "Node", text: "A thing in the graph, such as a person or a product, with a label and properties." },
  edge: { short: "Edge", text: "A typed link from one node to another, such as follows or bought." },
  traversal: { short: "Traversal", text: "Walking the graph from a start node along edges, up to a depth." },
  // Billing
  dunning: { short: "Dunning", text: "Retrying a failed payment and reminding the customer, on a schedule, before the subscription is cancelled." },
  proration: { short: "Proration", text: "Charging or crediting the part of a period that was used when a plan changes mid-period." },
  meter: { short: "Usage meter", text: "Counts something a customer uses, such as API calls, so it can be billed or limited." },
  entitlement: { short: "Entitlement", text: "What a customer's plan lets them use, such as a feature or a monthly limit." },
  webhook_delivery: { short: "Webhook delivery", text: "One request a payment provider sent to ForgeBase, kept with its headers and body whether or not it verified, so you can see why it was applied, ignored or refused." },
  reverify: { short: "Re-verify", text: "Check a refused delivery again with the webhook secrets as they are now, and process it once if it passes. Use it after fixing a wrong secret." },
  replay: { short: "Replay", text: "Process a verified delivery again through the normal path. An event that was already handled changes nothing." },
  // System and metrics
  wal: { short: "WAL", text: "Write-ahead log: the database writes changes here first, so they survive a crash." },
  rss: { short: "RSS", text: "Resident set size: the memory the server process is using right now." },
  p50: { short: "p50", text: "The median: half of the requests were faster than this." },
  p95: { short: "p95", text: "95 in 100 requests were faster than this. It shows the slow tail." },
  scrape: { short: "Scrape", text: "One read of the server's metrics. The charts update on each scrape." },
  backup: { short: "Backup", text: "A copy of the data directory you can restore later, on this server or another." },
  request_id: { short: "Request id", text: "A unique id the server gives each request. Search the server log for it to find what happened." },
  // Database topology (FB-176)
  replica: { short: "Read replica", text: "A copy of your database on another server that follows the primary and answers reads. It spreads the load and can take over if the primary fails." },
  primary: { short: "Primary", text: "The server that takes every write. Replicas copy it." },
  lag: { short: "Replica lag", text: "How far a replica is behind the primary, as time and as bytes of changes it has not applied yet. Small is healthy." },
  shard: { short: "Shard", text: "A separate database server that holds some of your tenants, so no single server has to hold them all. Each shard can have its own replicas." },
  promote: { short: "Promote", text: "Make a replica the new primary. Do it when the primary has failed or you are moving to a new server. The old primary must be rebuilt as a replica before it rejoins." },
  rebalance: { short: "Rebalance", text: "Move tenants between shards so each shard holds a fair share. ForgeBase shows the plan first and moves nothing until you confirm." },
  freeze: { short: "Freeze", text: "While a tenant moves, its writes pause for a moment at the end (usually a second or two) so no change is lost. Reads and other tenants carry on." },
  read_after: { short: "Read after", text: "A marker your app sends so a read sees its own earlier writes, even when a replica answers it." },
  topology_set: { short: "Set", text: "A primary server with its replicas. Control is the first set; every shard is another." },
} satisfies Record<string, GlossaryEntry>;

export type GlossaryTerm = keyof typeof GLOSSARY;

export function define(term: GlossaryTerm): GlossaryEntry {
  return GLOSSARY[term];
}
