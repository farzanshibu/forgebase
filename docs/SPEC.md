# MASTER PROMPT: Build ForgeBase — An All-in-One Rust Backend and Multi-Model Database

## 1. Your role

Act as a principal Rust database engineer, distributed systems architect, database kernel engineer, backend security engineer, DevOps engineer and product UI engineer.

Design and implement **ForgeBase**, a production-oriented, self-hosted, high-performance Backend-as-a-Service (BaaS) platform written primarily in Rust.

The product should combine the most useful capabilities of:

- PocketBase — single-binary backend, authentication, collections, admin dashboard and realtime.
- Supabase — PostgreSQL, SQL, authentication, object storage, database policies and server functions.
- Redis — high-speed key-value storage, caching, pub/sub, streams and expiration.
- Convex — reactive queries, automatic realtime synchronization and server-side functions.
- Firebase — authentication, document storage, realtime listeners and developer SDKs.
- SurrealDB — multi-model database, graph relationships, vector search and unified querying.

Do not create a simple CRUD application. Build a complete database and backend platform with a consistent API, admin console, SDKs, security model and deployment tooling.

## 2. Primary product objective

Build a single executable Rust application that starts a complete backend platform.

Example:

```bash
forgebase init my-project
cd my-project

forgebase serve
```

The application should provide:

- Embedded database.
- SQL database.
- Document database.
- High-performance indexes.
- Key-value database and Redis-compatible functionality.
- Object storage.
- Vector database.
- Graph database.
- Realtime subscriptions.
- Authentication and authorization.
- Multi-tenancy.
- Server-side functions.
- API gateway.
- Background jobs and queues.
- Billing and subscription management.
- Admin dashboard.
- Developer APIs and SDKs.
- Backup, restore and migrations.
- Metrics, logs and monitoring.

### Critical architectural requirement

Deliver a **single self-contained executable**, with modular internal Rust architecture.

Support two deployment models:

1. Embedded mode: one portable primary database file, local object-storage directory and rebuildable index files.
2. Server mode: standalone service with configurable storage backends and optional external services for production scaling.

Do not force large video files, object blobs, temporary indexes or runtime artifacts into the primary database file.

The application should remain usable without installing PostgreSQL, Redis, MinIO or Qdrant for its default embedded experience.

## 3. Technology stack

Use Rust as the primary programming language.

Prefer mature, maintained libraries. Before selecting dependencies, verify their current stable versions, security advisories, licenses, APIs and compatibility.

### Backend technology

| Component | Preferred technology |
|---|---|
| Programming language | Rust |
| Async runtime | Tokio |
| HTTP server | Axum |
| Database abstraction | SQLx |
| Primary embedded database | SQLite |
| Database mode | WAL |
| JSON/document operations | SQLite JSON functions |
| Full-text indexing | SQLite FTS5 |
| Vector search | USearch or equivalent ANN engine |
| Graph representation | Indexed relational edge tables |
| Optional embedded KV | redb |
| Cache | Native in-memory cache with optional Redis/Valkey adapter |
| Serialization | Serde |
| Authentication | Argon2id, JWT, opaque sessions |
| WebSocket | Axum WebSocket |
| WASM runtime | Wasmtime |
| Object storage | Local filesystem + S3-compatible adapter |
| CLI | Clap |
| OpenAPI | Utoipa or equivalent |
| Observability | tracing, Prometheus, OpenTelemetry |
| Frontend | React + TypeScript + Tailwind CSS |
| Frontend data tables | TanStack Table |

Use Cargo workspaces and clean module boundaries.

Do not create an unmaintainable monolithic Rust source file merely to satisfy the single-binary requirement.

## 4. Unified database architecture

Create a unified database system supporting different data models through one consistent project namespace, identity system and permission engine.

### 4.1 SQL database

Implement a proper SQL database interface using SQLite as the embedded storage engine.

Support:

- CREATE TABLE.
- ALTER TABLE.
- DROP TABLE.
- INSERT.
- UPDATE.
- DELETE.
- SELECT.
- JOIN.
- GROUP BY.
- ORDER BY.
- LIMIT and OFFSET.
- Transactions.
- Foreign keys.
- Unique constraints.
- Check constraints.
- Views.
- Triggers.
- Prepared statements.
- Parameterized queries.
- Database migrations.
- Schema introspection.

Provide an SQL editor in the admin dashboard.

Support multiple database connections, transaction isolation where supported, configurable connection pooling and safe query execution.

Use SQLite WAL appropriately. Respect its single-writer architecture and do not claim unlimited concurrent write performance.

Design a storage abstraction that can support PostgreSQL as an optional external backend later without forcing the entire codebase to depend on PostgreSQL.

### 4.2 Document database

Build a document database interface.

Features:

- JSON documents.
- Dynamic schemas.
- Optional schema validation.
- Nested objects.
- Nested arrays.
- JSON path queries.
- Partial document updates.
- Document versioning.
- Automatic timestamps.
- Pagination.
- Sorting.
- Filtering.
- Projection.
- Aggregation.
- Document relationships.

Example:

```json
{
  "collection": "products",
  "document": {
    "name": "Laptop",
    "price": 75000,
    "category": "electronics",
    "metadata": {
      "brand": "Example",
      "specifications": {
        "ram": 16,
        "storage": 1024
      }
    }
  }
}
```

Allow developers to create collections through the dashboard or SDK.

### 4.3 Collection management

Create a PocketBase-style collection management system.

Support:

- Base collections.
- Auth collections.
- View collections.
- System collections.
- Custom collections.
- Field definitions.
- Relations.
- Unique validation.
- Required fields.
- Defaults.
- Enums.
- JSON.
- Arrays.
- Numbers.
- Dates.
- Booleans.
- Email.
- URL.
- File references.
- Password fields.
- Geo coordinates.
- Vector fields.

Automatically generate API endpoints from collection schemas.

### 4.4 Key-value and Redis-compatible engine

Build a dedicated high-speed key-value service.

Features:

- GET.
- SET.
- MGET.
- MSET.
- DEL.
- EXISTS.
- INCR.
- DECR.
- EXPIRE.
- TTL.
- KEYS with safe scan-based alternatives.
- Hashes.
- Lists.
- Sets.
- Sorted sets.
- Pub/Sub.
- Streams.
- Atomic operations.
- TTL expiration.
- LRU or configurable eviction.
- Memory limits.
- Key namespaces.
- Tenant isolation.

Provide an in-process cache for embedded deployment.

Optionally expose a Redis-compatible RESP listener, with a clearly documented compatibility matrix and conformance tests.

Support an external Redis or Valkey adapter for distributed caching in server deployments.

Do not pretend that a basic cache implementation is a complete Redis replacement.

Cache data must not silently replace durable SQL records.

### 4.5 High-performance indexing engine

Index performance is a core product requirement.

Implement:

- B-tree indexes.
- Composite indexes.
- Unique indexes.
- Partial indexes.
- Covering indexes where supported.
- Expression indexes.
- JSON-path indexes.
- Full-text indexes.
- Spatial indexes where supported.
- Vector ANN indexes.
- Graph traversal indexes.
- TTL indexes or equivalent expiration mechanisms.

Add:

- Automatic index creation from collection schemas.
- Manual index creation.
- Index inspection.
- Index rebuilding.
- Index statistics.
- Query plan inspection.
- Query profiling.
- Slow-query detection.
- Index recommendations.

Expose index management through APIs and the dashboard.

Use the underlying database's native indexing capabilities instead of implementing custom indexing algorithms without a measurable need.

Persist canonical data first. Treat auxiliary vector and cache indexes as derived indexes that can be rebuilt after a crash.

## 5. Object storage engine

Build a complete object-storage service.

Support:

- Buckets.
- Objects.
- File metadata.
- Upload.
- Download.
- Delete.
- Rename.
- Copy.
- Move.
- Multipart upload.
- Resumable upload.
- Streaming upload and download.
- File versioning.
- Content-type detection.
- File size limits.
- File checksums.
- Content-addressed deduplication.
- Signed URLs.
- Public/private buckets.
- Bucket policies.
- CORS.
- Lifecycle rules.
- Automatic cleanup.
- Storage quotas.
- Thumbnail generation.
- Image processing hooks.

Use local filesystem storage by default.

Provide interchangeable storage drivers:

```rust
trait ObjectStorage {
    async fn put(&self, request: PutObjectRequest)
        -> Result<ObjectMetadata>;

    async fn get(&self, request: GetObjectRequest)
        -> Result<ObjectStream>;

    async fn delete(&self, key: &str)
        -> Result<()>;

    async fn copy(&self, source: &str, destination: &str)
        -> Result<()>;
}
```

Implement:

- Local storage.
- S3-compatible storage.
- Optional remote storage adapters.

Stream large files without loading entire objects into RAM.

Support optional server-side encryption and encrypted metadata.

The core database should store object metadata and references, not require large media files to be embedded in the database.

## 6. Vector database

Build an integrated vector search engine.

Support:

- Vector insertion.
- Vector updates.
- Vector deletion.
- Vector retrieval.
- Batch insertion.
- Vector collections.
- Dimensions.
- Metadata.
- Filtering.
- Similarity search.
- Top-K search.
- Cosine similarity.
- Euclidean distance.
- Dot product.
- Approximate nearest-neighbor search.
- Exact nearest-neighbor search for small datasets.
- Index persistence.
- Index rebuilding.
- Collection isolation.

Support vector types such as:

- Float32.
- Float16 where supported.
- Int8 where supported.
- Binary vectors where supported.

Implement HNSW or another appropriate ANN indexing method through a maintained library such as USearch.

Build a vector abstraction that supports switching indexing engines later.

Important: canonical vectors and metadata must be durable. ANN indexes must be recoverable and rebuildable.

Support hybrid search:

```text
Vector similarity
        +
Keyword/full-text search
        +
Metadata filters
        +
Ranking
        =
Hybrid search
```

Provide APIs for RAG, semantic search, image similarity and recommendation systems.

## 7. Graph database

Build a native graph data model.

Support:

- Nodes.
- Edges.
- Directed relationships.
- Undirected relationships.
- Edge properties.
- Node properties.
- Multiple relationship types.
- Graph traversal.
- Multi-hop queries.
- Shortest paths.
- Neighbor search.
- Graph filtering.
- Recursive relationships.
- Relationship creation and deletion.

Example:

```text
User A
  |
  | FOLLOWS
  |
User B
  |
  | PURCHASED
  |
Product C
```

Provide:

- CREATE NODE.
- CREATE EDGE.
- DELETE EDGE.
- FIND NEIGHBORS.
- TRAVERSE.
- SHORTEST PATH.
- MULTI-HOP QUERY.

Use indexed relational edge tables and recursive SQL where appropriate.

Consider an in-memory graph acceleration layer for expensive graph algorithms.

The admin dashboard should contain a visual graph explorer.

## 8. Multi-model query engine

Create a unified interface that allows developers to work with SQL, JSON documents, graph relationships, vectors and key-value data.

Design a common query API.

Example:

```typescript
const result = await db.query({
  model: "documents",
  collection: "products",
  filter: {
    category: "electronics"
  },
  sort: {
    price: "asc"
  },
  limit: 20
});
```

Vector example:

```typescript
const results = await db.vector.search({
  collection: "knowledge",
  vector: embedding,
  limit: 10,
  filter: {
    tenant_id: "tenant_123"
  }
});
```

Graph example:

```typescript
const graph = await db.graph.traverse({
  start: "user_123",
  edge: "FOLLOWS",
  depth: 3
});
```

SQL example:

```typescript
const users = await db.sql`
  SELECT *
  FROM users
  WHERE active = ${true}
`;
```

Do not build a completely new SQL query parser unless necessary.

Provide a consistent SDK while allowing advanced users to access SQL-specific capabilities directly.

## 9. Realtime engine

Build a high-performance event-driven realtime system.

Support:

- WebSocket connections.
- Server-Sent Events.
- Database change subscriptions.
- Document subscriptions.
- Collection subscriptions.
- Record subscriptions.
- User presence.
- Broadcast messages.
- Pub/Sub.
- Room-based communication.
- Channel-based subscriptions.
- Reconnection.
- Event replay.
- Subscription authorization.
- Connection limits.
- Heartbeats.
- Backpressure.
- Event filtering.

Events:

```json
{
  "type": "record.updated",
  "collection": "products",
  "record_id": "prod_123",
  "tenant_id": "tenant_123",
  "timestamp": "2026-11-07T10:00:00Z",
  "data": {}
}
```

Implement a transactional outbox so changes and their event records are committed consistently.

Ensure users can only subscribe to records they are authorized to access.

Provide reliable event cursors and reconnection semantics.

Use Redis/Valkey or another message broker for cross-instance fan-out when deploying multiple servers.

## 10. Authentication

Build a complete authentication system.

Support:

- Email and password.
- Email verification.
- Password reset.
- Magic links.
- Email OTP.
- Phone OTP through configurable providers.
- Google OAuth.
- GitHub OAuth.
- Apple OAuth.
- Generic OIDC.
- OAuth2.
- WebAuthn/passkeys.
- TOTP MFA.
- Recovery codes.
- Session management.
- Device management.
- Login history.
- API keys.
- Service accounts.
- Personal access tokens.

Security requirements:

- Argon2id password hashing.
- Secure random token generation.
- Short-lived access tokens.
- Rotating refresh tokens.
- Token revocation.
- Session expiration.
- Secure cookie support.
- Rate limiting.
- Account lockout protections.
- Brute-force protection.
- CSRF protection where applicable.
- Audit logging.

Never store plaintext passwords or long-lived secrets.

## 11. Authorization and policy engine

Build a unified authorization engine.

Support:

- RBAC.
- ABAC.
- Row-level security.
- Collection-level permissions.
- Field-level permissions.
- Record-level access.
- Tenant isolation.
- Resource ownership.
- API key scopes.
- Service account permissions.
- Administrative roles.
- Custom policies.

Example rules:

```text
listRule:
  authenticated = true

viewRule:
  owner_id = current_user.id

createRule:
  authenticated = true

updateRule:
  owner_id = current_user.id

deleteRule:
  role = "admin"
```

Provide a policy editor in the dashboard.

Authorization must be enforced at the service and database access layers, not merely hidden in the frontend.

Fail closed when a permission rule cannot be evaluated.

Prevent cross-tenant access even when a caller knows another tenant's record ID.

## 12. Multi-tenancy and projects

Build a project and organization management system.

Support:

- Organizations.
- Projects.
- Project environments.
- Users.
- Teams.
- Invitations.
- Roles.
- Permissions.
- API keys.
- Storage quotas.
- Database quotas.
- Usage limits.
- Project settings.
- Project-level secrets.

Support separate project namespaces.

Every project-owned resource must be isolated.

Provide development, staging and production environments.

## 13. Server-side functions and extension engine

Build a serverless functions system similar to Supabase Edge Functions.

Use WebAssembly and Wasmtime for sandboxed execution.

Support:

- HTTP functions.
- Database triggers.
- Before-create hooks.
- After-create hooks.
- Before-update hooks.
- After-update hooks.
- Before-delete hooks.
- After-delete hooks.
- Scheduled functions.
- Cron jobs.
- Queue consumers.
- Background tasks.
- Webhook handlers.
- Custom validation.
- Event-driven functions.

Function deployment should support:

- WASM modules.
- Function versioning.
- Environment variables.
- Secret injection.
- Execution logs.
- Execution metrics.
- Timeouts.
- Memory limits.
- CPU/fuel limits.
- Concurrency limits.
- Permissions.
- Rollbacks.

Use capability-based security.

Never give user-uploaded code unrestricted filesystem, network or process access.

Functions should be isolated from the host and should receive only explicitly granted capabilities.

## 14. Background jobs and task queues

Implement:

- Job creation.
- Job scheduling.
- Delayed jobs.
- Recurring jobs.
- Retry policies.
- Exponential backoff.
- Dead-letter queues.
- Worker registration.
- Worker heartbeats.
- Concurrency limits.
- Priority queues.
- Job cancellation.
- Job status.
- Job history.

Example:

```typescript
await jobs.enqueue("send_email", {
  recipient: "user@example.com"
});
```

Support internal jobs and external worker processes.

Ensure jobs are not silently lost during process restarts.

## 15. Billing and subscription engine

Implement a billing and metering subsystem.

Support:

- Free plans.
- Paid plans.
- Monthly subscriptions.
- Annual subscriptions.
- Trials.
- Usage-based billing.
- Add-on packages.
- Storage billing.
- Database operation quotas.
- API request quotas.
- Bandwidth quotas.
- Function execution quotas.
- Overage calculation.
- Invoices.
- Discounts.
- Coupons.
- Subscription upgrades.
- Downgrades.
- Cancellations.
- Payment history.
- Customer portal integration.

Create provider abstractions for:

- Stripe.
- Razorpay.
- Manual billing.
- Future payment providers.

Webhook handling must have signature verification, idempotency, replay protection and durable event processing.

Never store card numbers, CVVs or sensitive payment credentials.

Support a self-hosted mode where billing is disabled or configured by the operator.

## 16. API gateway

Automatically expose REST APIs for collections.

Example:

```text
POST   /api/collections/{collection}/records
GET    /api/collections/{collection}/records
GET    /api/collections/{collection}/records/{id}
PATCH  /api/collections/{collection}/records/{id}
DELETE /api/collections/{collection}/records/{id}
```

Additional endpoints:

```text
/api/auth/*
/api/sql/*
/api/documents/*
/api/kv/*
/api/storage/*
/api/vector/*
/api/graph/*
/api/realtime/*
/api/functions/*
/api/jobs/*
/api/billing/*
/admin/*
```

Support:

- REST.
- OpenAPI documentation.
- API versioning.
- Pagination.
- Filtering.
- Sorting.
- Search.
- Rate limiting.
- API key authentication.
- JWT authentication.
- Request validation.
- Consistent error responses.
- CORS.
- Request tracing.

GraphQL may be added as a later optional interface.

## 17. Admin dashboard

Build a full-featured web-based administration console.

Use React, TypeScript and Tailwind CSS.

The interface should be modern, responsive, minimalist and developer-oriented.

Provide the following sections.

### Dashboard

- Database status.
- Database size.
- Memory usage.
- CPU usage.
- Requests per second.
- Active connections.
- Realtime connections.
- Query latency.
- Storage usage.
- Vector index statistics.
- Background job status.
- Error statistics.

### Database Studio

- Collection browser.
- Table editor.
- Spreadsheet-style data viewer.
- Inline cell editing.
- Add/delete records.
- Bulk editing.
- JSON editor.
- Relationship browser.
- Schema designer.
- Index management.
- SQL editor.
- Query history.
- Query profiler.
- Explain query plans.
- Import/export data.

### Redis Studio

- Key browser.
- Value viewer.
- TTL management.
- Memory statistics.
- Key search.
- Pub/Sub inspector.
- Stream browser.
- Cache statistics.

### Object Storage Studio

- Bucket management.
- File browser.
- Upload and download.
- Preview images.
- Preview documents.
- Metadata editing.
- Access policies.
- File versions.
- Storage statistics.

### Vector Studio

- Vector collections.
- Dimensions.
- Inserted vectors.
- Metadata viewer.
- Similarity search playground.
- Filter builder.
- Index configuration.
- Index rebuilding.
- Search performance.

### Graph Studio

- Node browser.
- Edge browser.
- Graph visualization.
- Relationship explorer.
- Traversal query builder.
- Path search.

### Authentication Studio

- Users.
- Sessions.
- Roles.
- Permissions.
- OAuth providers.
- MFA.
- API keys.
- Audit logs.

### Functions Studio

- Function editor.
- WASM upload.
- Deployment.
- Environment variables.
- Secrets.
- Trigger configuration.
- Function logs.
- Execution metrics.
- Scheduled tasks.

### Billing Studio

- Plans.
- Subscriptions.
- Usage.
- Invoices.
- Customers.
- Payment history.
- Webhooks.

### System Studio

- Server logs.
- Error logs.
- Audit logs.
- Database migrations.
- Backups.
- Restore.
- Configuration.
- Health checks.
- System metrics.

## 18. Developer experience and SDKs

Provide developer-friendly SDKs.

Initial SDK priorities:

1. TypeScript / JavaScript.
2. Rust.
3. Flutter / Dart.

Example:

```typescript
import { ForgeBase } from "@forgebase/sdk";

const db = new ForgeBase({
  url: "http://localhost:8090",
  apiKey: "project-api-key"
});

const result = await db.collection("users").getList();

const record = await db
  .collection("users")
  .create({
    name: "Farzan"
  });

const subscription = db
  .collection("users")
  .subscribe("*", (event) => {
    console.log(event);
  });
```

Provide:

- Type-safe queries.
- Authentication helpers.
- File upload helpers.
- Realtime subscriptions.
- Vector APIs.
- Graph APIs.
- Function invocation.
- Error handling.
- Automatic retries where safe.
- OpenAPI-generated client support.
- Pagination helpers.

Generate SDK types from collection schemas where possible.

## 19. CLI and developer tooling

Implement:

```bash
forgebase init
forgebase serve
forgebase dev
forgebase stop
forgebase status
forgebase migrate
forgebase backup
forgebase restore
forgebase export
forgebase import
forgebase collections
forgebase users
forgebase functions deploy
forgebase functions logs
forgebase projects
forgebase doctor
forgebase version
```

Provide:

- TOML configuration.
- Environment variable configuration.
- `.env` support for development.
- Project templates.
- Database migration generation.
- Database seeding.
- Local development mode.
- Production mode.
- Docker support.
- Systemd service support.

## 20. Data integrity and reliability

Treat correctness as more important than benchmark numbers.

Implement:

- ACID transactions wherever the underlying engine supports them.
- Foreign-key integrity.
- Atomic migrations.
- Crash recovery.
- Database consistency checks.
- Durable event outbox.
- Object metadata reconciliation.
- Index recovery.
- Backup and restore.
- Database snapshots.
- Checkpoints.
- Schema versioning.
- Audit history.

Use a transactional workflow for operations involving multiple internal components.

For operations involving external object storage, use staging, commit markers and background reconciliation to handle failures.

Never report an operation as successful before its required durable state is committed.

## 21. Performance engineering

Optimize based on benchmarks, not assumptions.

Targets should be defined on documented hardware and measured with repeatable tests.

Implement:

- Async request handling.
- Connection pooling.
- Prepared statements.
- Query planning.
- Efficient pagination.
- Streaming responses.
- Batching.
- Bulk insertion.
- Bounded concurrency.
- Memory pooling where beneficial.
- Compression.
- Cache invalidation.
- Index statistics.
- Query caching where safe.
- Backpressure.
- Efficient serialization.

Benchmark:

- SQL reads.
- SQL writes.
- Document reads.
- Document writes.
- Key-value operations.
- Object uploads.
- Object downloads.
- Vector queries.
- Graph traversals.
- Realtime fan-out.
- Authentication.
- API throughput.

Measure:

- Throughput.
- p50 latency.
- p95 latency.
- p99 latency.
- CPU consumption.
- Memory consumption.
- Disk I/O.
- Index size.
- Recovery time.

Use Criterion and suitable load-testing tools.

Never claim that ForgeBase is faster than Redis, PostgreSQL, MongoDB, or another product unless a reproducible benchmark proves it under comparable conditions.

## 22. Security architecture

Implement production-grade security.

Requirements:

- TLS support.
- Secure password hashing.
- Encryption for sensitive secrets.
- Token rotation.
- Fine-grained permissions.
- Tenant isolation.
- Request validation.
- SQL injection protection.
- XSS prevention in the admin console.
- CSRF protection where applicable.
- SSRF protection.
- Rate limiting.
- File upload validation.
- MIME-type verification.
- Safe path handling.
- Webhook signature verification.
- Audit logging.
- Secret redaction.
- Secure default configuration.
- Sandboxed functions.
- Dependency auditing.

Run automated security tests.

Do not expose administrative or database-management endpoints publicly without authentication and authorization.

## 23. Observability and operations

Provide:

- Structured logging.
- Distributed tracing.
- Prometheus metrics.
- Health endpoints.
- Readiness checks.
- Liveness checks.
- Error reporting.
- Database metrics.
- Query metrics.
- Realtime metrics.
- Storage metrics.
- Job metrics.
- Function execution metrics.

Support:

```text
GET /health
GET /health/ready
GET /metrics
```

Provide a diagnostic command:

```bash
forgebase doctor
```

It should inspect:

- Database integrity.
- Available disk space.
- Configuration.
- File permissions.
- Storage health.
- Index health.
- Migration status.
- Port availability.

## 24. Deployment and distribution

The primary deliverable must be a single executable.

Example:

```bash
./forgebase serve \
  --host 0.0.0.0 \
  --port 8090 \
  --data-dir ./forgebase-data
```

Default startup should initialize:

- Database.
- API server.
- Authentication.
- Realtime server.
- Internal cache.
- Object storage.
- Function runtime.
- Background worker.
- Admin dashboard.

The executable should serve its embedded frontend assets.

Provide builds for:

- Linux x86_64.
- Linux ARM64.
- macOS ARM64.
- macOS x86_64 where practical.
- Windows x86_64.

Provide Docker images and Docker Compose examples.

Make external dependencies optional rather than mandatory for embedded mode.

For production clustering, clearly identify components that require external infrastructure, such as PostgreSQL, shared object storage and a message broker.

## 25. Suggested repository architecture

Create a modular Rust workspace:

```text
forgebase/
│
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
├── Dockerfile
├── docker-compose.yml
├── forgebase.example.toml
│
├── crates/
│   ├── forgebase-core/
│   ├── forgebase-server/
│   ├── forgebase-api/
│   ├── forgebase-cli/
│   ├── forgebase-db/
│   ├── forgebase-sql/
│   ├── forgebase-documents/
│   ├── forgebase-kv/
│   ├── forgebase-index/
│   ├── forgebase-vector/
│   ├── forgebase-graph/
│   ├── forgebase-storage/
│   ├── forgebase-realtime/
│   ├── forgebase-auth/
│   ├── forgebase-authz/
│   ├── forgebase-functions/
│   ├── forgebase-jobs/
│   ├── forgebase-billing/
│   ├── forgebase-events/
│   ├── forgebase-migrations/
│   └── forgebase-observability/
│
├── sdk/
│   ├── typescript/
│   ├── rust/
│   └── dart/
│
├── dashboard/
│   ├── src/
│   ├── public/
│   ├── package.json
│   └── vite.config.ts
│
├── migrations/
│
├── examples/
│
├── tests/
│   ├── integration/
│   ├── security/
│   ├── performance/
│   └── compatibility/
│
└── docs/
    ├── architecture.md
    ├── api.md
    ├── database.md
    ├── security.md
    ├── deployment.md
    └── sdk.md
```

Adjust the structure as needed, but maintain clear dependency boundaries.

## 26. Implementation roadmap

Do not attempt to implement all modules in one enormous code-generation operation.

Build working, integrated functionality in phases.

### Phase 1 — Core foundation

- Rust workspace.
- Tokio runtime.
- Axum HTTP server.
- SQLite embedded database.
- Database migrations.
- CLI.
- Configuration.
- Health endpoints.
- Basic admin dashboard.
- Single-binary build.

**Milestone:** A working PocketBase-style application that starts and exposes a persistent database.

### Phase 2 — SQL and documents

- SQL query interface.
- Collection management.
- Dynamic schemas.
- JSON documents.
- Relationships.
- Index management.
- Filtering.
- Pagination.
- Table editor.
- Import/export.

**Milestone:** Developers can create tables and collections, insert records and query data from both the SDK and dashboard.

### Phase 3 — Authentication and authorization

- Authentication.
- OAuth.
- Sessions.
- Password reset.
- MFA.
- Roles.
- Policies.
- Record permissions.
- API keys.
- Tenant isolation.

**Milestone:** A secure multi-user application backend.

### Phase 4 — Storage and Redis

- Object storage.
- File uploads.
- Signed URLs.
- KV engine.
- TTL.
- Cache API.
- Pub/Sub.
- Streams.
- Redis-compatible protocol subset.

**Milestone:** A complete embedded storage and cache experience.

### Phase 5 — Realtime

- WebSockets.
- SSE.
- Event system.
- Database change notifications.
- Presence.
- Broadcast.
- Reconnection.
- Authorization-aware subscriptions.

**Milestone:** Reactive applications without an external realtime service.

### Phase 6 — Vector and graph

- Vector storage.
- ANN indexes.
- Similarity search.
- Metadata filtering.
- Hybrid search.
- Graph nodes.
- Graph edges.
- Traversal.
- Graph explorer.

**Milestone:** Integrated multi-model database capabilities.

### Phase 7 — Functions and jobs

- WASM runtime.
- Sandboxed functions.
- Database hooks.
- HTTP functions.
- Cron.
- Background queues.
- Worker system.

**Milestone:** User-defined backend execution.

### Phase 8 — Billing and usage metering

- Organizations.
- Subscription plans.
- Usage tracking.
- Quotas.
- Stripe adapter.
- Razorpay adapter.
- Webhooks.
- Billing dashboard.

**Milestone:** SaaS-ready project and subscription management.

### Phase 9 — Production hardening

- Security audit.
- Load testing.
- Recovery testing.
- Backup/restore.
- Monitoring.
- Multi-process support where applicable.
- Deployment documentation.
- SDK publishing.
- Release automation.

**Milestone:** A documented, tested and deployable platform.

## 27. Mandatory testing requirements

Create automated tests for every major component.

Minimum scenarios:

1. Create a project.
2. Create a SQL table.
3. Create a document collection.
4. Insert and query records.
5. Create a composite index.
6. Validate index usage with query plans.
7. Authenticate a user.
8. Reject unauthorized requests.
9. Verify tenant isolation.
10. Upload and retrieve an object.
11. Generate a signed download URL.
12. Store and retrieve a vector.
13. Execute vector similarity search.
14. Create graph nodes and edges.
15. Execute graph traversal.
16. Subscribe to realtime events.
17. Verify event delivery after a committed change.
18. Execute a sandboxed server function.
19. Schedule and execute a background job.
20. Process a billing webhook idempotently.
21. Restart the server and verify persistence.
22. Back up and restore the database.
23. Rebuild derived indexes.
24. Test concurrent read and write workloads.
25. Test malicious inputs and permission bypass attempts.

Do not mark a feature complete because its endpoint returns HTTP 200. Test actual functionality and persistence.

## 28. Development rules

Follow these rules throughout implementation:

1. Write real, compilable Rust code.
2. Avoid placeholder implementations for core features.
3. Never use mock responses as production functionality.
4. Do not silently skip failing tests.
5. Do not hide errors behind generic success responses.
6. Use safe Rust by default.
7. Use async I/O for network operations.
8. Avoid blocking the Tokio runtime.
9. Use transactions for related database changes.
10. Use prepared statements.
11. Implement explicit error types.
12. Document public APIs.
13. Write unit and integration tests.
14. Keep the Cargo dependency tree controlled.
15. Verify third-party licenses.
16. Avoid unnecessary external services.
17. Preserve backward-compatible APIs where possible.
18. Provide database migration and rollback strategies.
19. Keep security policies centralized.
20. Never claim a module is complete until its acceptance tests pass.

For each phase:

- Explain the architecture briefly.
- Implement actual files.
- Compile the application.
- Run tests.
- Fix failures.
- Update documentation.
- Provide working commands.
- Record remaining limitations.
- Continue to the next phase.

Do not stop after producing a design document.

## 29. Final deliverables

The final product should contain:

- Complete Rust source code.
- Single executable build.
- Embedded database engine.
- SQL and document interfaces.
- Redis-compatible key-value functionality.
- Object storage.
- High-performance indexes.
- Vector search.
- Graph database.
- Realtime engine.
- Authentication and authorization.
- Multi-tenancy.
- Server-side functions.
- Background job system.
- Billing engine.
- REST API.
- OpenAPI documentation.
- Admin dashboard.
- TypeScript SDK.
- Rust SDK.
- Flutter SDK.
- CLI.
- Docker support.
- Backup and restore.
- Database migrations.
- Automated tests.
- Performance benchmarks.
- Security documentation.
- Deployment guide.
- Complete README.

## 30. Definition of success

ForgeBase should feel like PocketBase when starting and configuring a project, like Supabase when managing SQL and backend services, like Redis when using high-speed key-value operations, like Convex when building reactive applications, and like SurrealDB when working with multiple data models.

All capabilities should share:

- One project system.
- One authentication system.
- One authorization system.
- One developer experience.
- One administrative interface.
- One consistent API.
- One deployment experience.

**Begin by inspecting the existing repository. If it is empty, initialize the workspace. Produce an architecture decision record, then implement Phase 1 immediately. Do not stop at planning. Continue implementing and testing the phases in dependency order, maintaining a working build at every milestone.**
