//! The feature registry (FB-91, ADR-0026): every module of a ForgeBase
//! server, whether it can be switched off, the HTTP routes it owns and its
//! settings, plus the comment-preserving writer behind
//! `PATCH /api/system/config`.
//!
//! The registry is static data. [`setting_value`] reads a setting's value
//! from a resolved [`Config`]; [`ConfigSources`] says which layer set it.
//! Route patterns use `*` for one path segment and a trailing `**` for any
//! number of segments (zero included).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

use crate::config::{Config, ConfigError, ConfigOrigin, ConfigSources, DEFAULT_DATA_DIR, Source};

/// The type of a setting, as the registry reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingType {
    /// `true` or `false`.
    Bool,
    /// A non-negative integer.
    Int,
    /// Free text.
    String,
    /// One of [`SettingDef::options`].
    Enum,
    /// A duration such as `"30s"` (none of today's settings).
    Duration,
    /// A list of strings (none of today's settings).
    List,
    /// An `http://` or `https://` URL, or empty.
    Url,
}

impl SettingType {
    /// Lowercase name, as in the registry JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            SettingType::Bool => "bool",
            SettingType::Int => "int",
            SettingType::String => "string",
            SettingType::Enum => "enum",
            SettingType::Duration => "duration",
            SettingType::List => "list",
            SettingType::Url => "url",
        }
    }
}

/// One setting of the registry.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted key, as in `forgebase.toml` (`storage.max_object_bytes`).
    pub key: &'static str,
    /// Short label.
    pub label: &'static str,
    /// One or two sentences; names the environment variable and flag.
    pub help: &'static str,
    /// Value type.
    pub ty: SettingType,
    /// Unit of an integer (`bytes`, `seconds`, `ms`, `days`).
    pub unit: Option<&'static str>,
    /// Allowed values of an [`SettingType::Enum`].
    pub options: &'static [&'static str],
    /// Environment variable that sets it, if any.
    pub env: Option<&'static str>,
    /// Command-line flag that sets it, if any.
    pub flag: Option<&'static str>,
    /// A secret: never listed with a value, never written by the API.
    pub secret: bool,
    /// Whether `PATCH /api/system/config` may write it (before looking at
    /// its source: a setting from the environment or a flag is never
    /// editable).
    pub editable: bool,
    /// Whether a change needs a restart. Every setting does today.
    pub restart_required: bool,
}

impl SettingDef {
    const fn new(key: &'static str, label: &'static str, ty: SettingType, help: &'static str) -> Self {
        Self {
            key,
            label,
            help,
            ty,
            unit: None,
            options: &[],
            env: None,
            flag: None,
            secret: false,
            editable: true,
            restart_required: true,
        }
    }

    const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = Some(unit);
        self
    }

    const fn options(mut self, options: &'static [&'static str]) -> Self {
        self.options = options;
        self
    }

    const fn env(mut self, env: &'static str) -> Self {
        self.env = Some(env);
        self
    }

    const fn flag(mut self, flag: &'static str) -> Self {
        self.flag = Some(flag);
        self
    }

    const fn read_only(mut self) -> Self {
        self.editable = false;
        self
    }

    const fn secret(key: &'static str, label: &'static str, env: Option<&'static str>, help: &'static str) -> Self {
        Self {
            key,
            label,
            help,
            ty: SettingType::String,
            unit: None,
            options: &[],
            env,
            flag: None,
            secret: true,
            editable: false,
            restart_required: true,
        }
    }
}

/// One module of the registry.
#[derive(Debug, Clone, Copy)]
pub struct FeatureDef {
    /// Stable id (`storage`).
    pub id: &'static str,
    /// Display name, also used in the `feature_disabled` message.
    pub name: &'static str,
    /// One line on what it does.
    pub summary: &'static str,
    /// Whether `[<section>] enabled = false` turns it off. When `false`, the
    /// module is always on (ADR-0026 says why for each).
    pub can_disable: bool,
    /// The setting that switches it (first in [`FeatureDef::settings`]).
    pub enable_key: Option<&'static str>,
    /// Dashboard route of its studio.
    pub studio_path: Option<&'static str>,
    /// Documentation, relative to the repository root.
    pub docs: &'static str,
    /// HTTP routes it owns; answered with `404 feature_disabled` when off.
    pub routes: &'static [&'static str],
    /// Settings, the enable switch first.
    pub settings: &'static [SettingDef],
}

impl FeatureDef {
    /// Whether the module is on in `config` (always for modules that cannot
    /// be switched off).
    pub fn enabled(&self, config: &Config) -> bool {
        match self.enable_key {
            Some(key) => setting_value(config, key).and_then(|v| v.as_bool()).unwrap_or(true),
            None => true,
        }
    }

    /// The message of the `feature_disabled` error.
    pub fn disabled_message(&self) -> String {
        format!("{} is disabled on this server", self.name)
    }
}

use SettingType::{Bool, Enum, Int, String as Text, Url};

const SERVER: &[SettingDef] = &[
    SettingDef::new(
        "data_dir",
        "Data directory",
        Text,
        "Where the database, storage/, indexes/ and backups/ live. Set it with --data-dir or FORGEBASE_DATA_DIR; it is not editable here because a new value points the server at other data.",
    )
    .env("FORGEBASE_DATA_DIR")
    .flag("--data-dir")
    .read_only(),
    SettingDef::new(
        "server.host",
        "Bind address",
        Text,
        "Address the HTTP server listens on; loopback by default (put a TLS proxy in front). Env FORGEBASE_HOST, flag --host.",
    )
    .env("FORGEBASE_HOST")
    .flag("--host"),
    SettingDef::new(
        "server.port",
        "Port",
        Int,
        "TCP port of the HTTP server; 0 picks a free port. Env FORGEBASE_PORT, flag --port.",
    )
    .env("FORGEBASE_PORT")
    .flag("--port"),
    SettingDef::new(
        "server.request_timeout_secs",
        "Request timeout",
        Int,
        "Longest a request may run, 1 to 3600 seconds. Env FORGEBASE_REQUEST_TIMEOUT_SECS.",
    )
    .unit("seconds")
    .env("FORGEBASE_REQUEST_TIMEOUT_SECS"),
    SettingDef::new(
        "server.shutdown_timeout_secs",
        "Shutdown grace",
        Int,
        "How long shutdown waits for in-flight requests, at most 3600 seconds. Env FORGEBASE_SHUTDOWN_TIMEOUT_SECS.",
    )
    .unit("seconds")
    .env("FORGEBASE_SHUTDOWN_TIMEOUT_SECS"),
    SettingDef::new(
        "server.ops_access",
        "Operational endpoints",
        Enum,
        "Who may read /metrics and /api/system/info: loopback (administrators, plus callers on this host), admin or public. Env FORGEBASE_OPS_ACCESS.",
    )
    .options(&["loopback", "admin", "public"])
    .env("FORGEBASE_OPS_ACCESS"),
    SettingDef::new(
        "log.level",
        "Log filter",
        Text,
        "Tracing filter such as info or forgebase=debug; must not be empty. Env FORGEBASE_LOG_LEVEL, flag --log-level.",
    )
    .env("FORGEBASE_LOG_LEVEL")
    .flag("--log-level"),
    SettingDef::new(
        "log.format",
        "Log format",
        Enum,
        "pretty for people, json for one object per line. Env FORGEBASE_LOG_FORMAT, flag --log-format.",
    )
    .options(&["pretty", "json"])
    .env("FORGEBASE_LOG_FORMAT")
    .flag("--log-format"),
    SettingDef::new(
        "database.synchronous",
        "Durability",
        Enum,
        "full fsyncs the WAL on every commit; normal is faster but a power cut can lose the last commits. Env FORGEBASE_DB_SYNCHRONOUS.",
    )
    .options(&["full", "normal"])
    .env("FORGEBASE_DB_SYNCHRONOUS"),
    SettingDef::new(
        "database.busy_timeout_ms",
        "Lock wait",
        Int,
        "How long a connection waits for another process's lock (CLI, backups), at most 600000 ms. Env FORGEBASE_DB_BUSY_TIMEOUT_MS.",
    )
    .unit("ms")
    .env("FORGEBASE_DB_BUSY_TIMEOUT_MS"),
    SettingDef::new(
        "database.read_pool_size",
        "Read connections",
        Int,
        "Parallel read-only database connections, 1 to 64. Env FORGEBASE_DB_READ_POOL_SIZE.",
    )
    .env("FORGEBASE_DB_READ_POOL_SIZE"),
];

const AUTH: &[SettingDef] = &[
    SettingDef::new(
        "auth.trust_forwarded_for",
        "Trust X-Forwarded-For",
        Bool,
        "Take the client address from the last X-Forwarded-For entry; only behind a reverse proxy you run. Env FORGEBASE_AUTH_TRUST_FORWARDED_FOR.",
    )
    .env("FORGEBASE_AUTH_TRUST_FORWARDED_FOR"),
    SettingDef::new(
        "auth.allow_private_outbound",
        "Private outbound addresses",
        Bool,
        "Let OAuth/OIDC, SMS and Turnstile requests reach loopback and private addresses (an identity provider on your own network). Env FORGEBASE_AUTH_ALLOW_PRIVATE_OUTBOUND.",
    )
    .env("FORGEBASE_AUTH_ALLOW_PRIVATE_OUTBOUND"),
    SettingDef::secret(
        "auth.master_key",
        "Master key",
        None,
        "Seals stored secrets. It is the file <data dir>/auth/master.key, created on first start; no environment variable sets it. Back it up with the data directory.",
    ),
];

const STORAGE: &[SettingDef] = &[
    SettingDef::new(
        "storage.enabled",
        "Object storage",
        Bool,
        "Serve /api/v1/storage and run the blob reconciler. Env FORGEBASE_STORAGE_ENABLED.",
    )
    .env("FORGEBASE_STORAGE_ENABLED"),
    SettingDef::new(
        "storage.driver",
        "Driver",
        Enum,
        "local keeps blobs under <data dir>/storage; s3 uses an S3-compatible service (needs s3_endpoint and s3_bucket).",
    )
    .options(&["local", "s3"]),
    SettingDef::new(
        "storage.max_object_bytes",
        "Largest object",
        Int,
        "Largest object in bytes; a bucket may set a lower limit.",
    )
    .unit("bytes"),
    SettingDef::new(
        "storage.upload_ttl_secs",
        "Upload session lifetime",
        Int,
        "Seconds a multipart or resumable upload session lives.",
    )
    .unit("seconds"),
    SettingDef::new(
        "storage.reconcile_grace_secs",
        "Reconciler grace",
        Int,
        "Seconds before the reconciler removes unreferenced blobs and stale staged files.",
    )
    .unit("seconds"),
    SettingDef::new(
        "storage.s3_endpoint",
        "S3 endpoint",
        Url,
        "S3 only: endpoint URL such as https://s3.eu-central-1.amazonaws.com.",
    ),
    SettingDef::new("storage.s3_region", "S3 region", Text, "S3 only: signing region."),
    SettingDef::new("storage.s3_bucket", "S3 bucket", Text, "S3 only: bucket that holds the blobs."),
    SettingDef::new("storage.s3_prefix", "S3 key prefix", Text, "S3 only: key prefix inside the bucket."),
    SettingDef::new(
        "storage.s3_part_size",
        "S3 part size",
        Int,
        "S3 only: multipart part size in bytes (S3 needs at least 5 MiB).",
    )
    .unit("bytes"),
    SettingDef::secret(
        "storage.s3_access_key",
        "S3 access key",
        Some("FORGEBASE_STORAGE_S3_ACCESS_KEY"),
        "S3 only: set with the environment variable FORGEBASE_STORAGE_S3_ACCESS_KEY.",
    ),
    SettingDef::secret(
        "storage.s3_secret_key",
        "S3 secret key",
        Some("FORGEBASE_STORAGE_S3_SECRET_KEY"),
        "S3 only: set with the environment variable FORGEBASE_STORAGE_S3_SECRET_KEY.",
    ),
    SettingDef::secret(
        "storage.s3_session_token",
        "S3 session token",
        Some("FORGEBASE_STORAGE_S3_SESSION_TOKEN"),
        "S3 only, optional: set with the environment variable FORGEBASE_STORAGE_S3_SESSION_TOKEN.",
    ),
];

const FUNCTIONS: &[SettingDef] = &[SettingDef::new(
    "functions.enabled",
    "Server functions",
    Bool,
    "Serve /api/v1/functions, run data hooks and the after-hook dispatcher. Off, record writes skip function hooks. Env FORGEBASE_FUNCTIONS_ENABLED.",
)
.env("FORGEBASE_FUNCTIONS_ENABLED")];

const QUEUES: &[SettingDef] = &[
    SettingDef::new(
        "queue.enabled",
        "Queues and topics",
        Bool,
        "Serve /api/v1/queues and run its maintenance and push task. Env FORGEBASE_QUEUE_ENABLED.",
    )
    .env("FORGEBASE_QUEUE_ENABLED"),
    SettingDef::new(
        "queue.max_message_bytes",
        "Largest message",
        Int,
        "Largest message (data, attributes, ordering key) and the default limit of a topic, 64 to 10485760 bytes.",
    )
    .unit("bytes"),
    SettingDef::new(
        "queue.default_retention_secs",
        "Default retention",
        Int,
        "Retention of topics that do not set one, at least 60 seconds.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.max_retention_secs",
        "Longest retention",
        Int,
        "Longest retention a topic may set; at least the default retention.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.dedup_window_secs",
        "Dedup window",
        Int,
        "A repeated dedup_id inside this window is accepted once, 1 to 86400 seconds.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.max_pull_wait_ms",
        "Longest pull wait",
        Int,
        "Longest long-poll wait of a pull, at most 300000 ms; keep it below the request timeout.",
    )
    .unit("ms"),
    SettingDef::new(
        "queue.poll_interval_ms",
        "Maintenance interval",
        Int,
        "Interval of maintenance (expired leases, retention) and push rounds, 10 to 60000 ms.",
    )
    .unit("ms"),
    SettingDef::new(
        "queue.push_timeout_secs",
        "Push timeout",
        Int,
        "Timeout of one webhook or function push, 1 to 300 seconds.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.push_concurrency",
        "Push concurrency",
        Int,
        "Push subscriptions served at once, 1 to 256.",
    ),
    SettingDef::new(
        "queue.allow_http_push",
        "Allow http:// webhooks",
        Bool,
        "Accept http:// webhook endpoints; local development only.",
    ),
    SettingDef::new(
        "queue.dead_letter_retention_days",
        "Dead-letter retention",
        Int,
        "Days dead-letter records are kept.",
    )
    .unit("days"),
];

const REALTIME: &[SettingDef] = &[
    SettingDef::new(
        "realtime.enabled",
        "Realtime",
        Bool,
        "Serve WebSocket, SSE and channels under /api/v1/realtime. The record-change log keeps being written when off (functions and queues read it). Env FORGEBASE_REALTIME_ENABLED.",
    )
    .env("FORGEBASE_REALTIME_ENABLED"),
    SettingDef::new(
        "realtime.max_connections_per_principal",
        "Connections per principal",
        Int,
        "Most simultaneous realtime connections one user or API key may hold, 1 to 1000000. Env FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL.",
    )
    .env("FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL"),
];

const VECTOR: &[SettingDef] = &[SettingDef::new(
    "vector.enabled",
    "Vector search",
    Bool,
    "Serve /api/v1/vector and load, warm and save vector indexes. Env FORGEBASE_VECTOR_ENABLED.",
)
.env("FORGEBASE_VECTOR_ENABLED")];

const GRAPH: &[SettingDef] = &[SettingDef::new(
    "graph.enabled",
    "Graph",
    Bool,
    "Serve the node and edge API under /api/v1/graph. Env FORGEBASE_GRAPH_ENABLED.",
)
.env("FORGEBASE_GRAPH_ENABLED")];

const IDENTITY: &[SettingDef] = &[SettingDef::new(
    "identity.enabled",
    "Identity provider",
    Bool,
    "Serve the OIDC provider (/api/auth/oidc, clients, consents, signing keys) and realm management (/api/v1/realms). Existing realms keep signing in when off. Env FORGEBASE_IDENTITY_ENABLED.",
)
.env("FORGEBASE_IDENTITY_ENABLED")];

const ACCESS: &[SettingDef] = &[SettingDef::new(
    "access.enabled",
    "Access control API",
    Bool,
    "Serve policies, roles, organisations, teams, projects and suspensions under /api/v1. The policy engine keeps enforcing the stored rules when off. Env FORGEBASE_ACCESS_ENABLED.",
)
.env("FORGEBASE_ACCESS_ENABLED")];

const RESP: &[SettingDef] = &[
    SettingDef::new(
        "resp.enabled",
        "Redis protocol listener",
        Bool,
        "Bind a RESP2/RESP3 listener for the key-value store; off by default, no TLS. Env FORGEBASE_RESP_ENABLED, flag --resp.",
    )
    .env("FORGEBASE_RESP_ENABLED")
    .flag("--resp"),
    SettingDef::new(
        "resp.host",
        "Bind address",
        Text,
        "Address of the RESP listener; keep it on loopback or behind a tunnel. Env FORGEBASE_RESP_HOST, flag --resp-host.",
    )
    .env("FORGEBASE_RESP_HOST")
    .flag("--resp-host"),
    SettingDef::new(
        "resp.port",
        "Port",
        Int,
        "TCP port of the RESP listener; 0 picks a free port. Env FORGEBASE_RESP_PORT, flag --resp-port.",
    )
    .env("FORGEBASE_RESP_PORT")
    .flag("--resp-port"),
    SettingDef::new(
        "resp.max_connections",
        "Most connections",
        Int,
        "Most simultaneous connections, 1 to 1000000; further ones are refused. Env FORGEBASE_RESP_MAX_CONNECTIONS.",
    )
    .env("FORGEBASE_RESP_MAX_CONNECTIONS"),
    SettingDef::new(
        "resp.max_bulk_bytes",
        "Largest argument",
        Int,
        "Largest single argument, 1 to 536870912 bytes. Env FORGEBASE_RESP_MAX_BULK_BYTES.",
    )
    .unit("bytes")
    .env("FORGEBASE_RESP_MAX_BULK_BYTES"),
    SettingDef::new(
        "resp.idle_timeout_secs",
        "Idle timeout",
        Int,
        "Close a connection idle this long, at most 86400 seconds (0 = never; subscribed connections are exempt). Env FORGEBASE_RESP_IDLE_TIMEOUT_SECS.",
    )
    .unit("seconds")
    .env("FORGEBASE_RESP_IDLE_TIMEOUT_SECS"),
];

const BILLING: &[SettingDef] = &[SettingDef::new(
    "billing.enabled",
    "Billing",
    Bool,
    "Serve plans, subscriptions, invoices and provider webhooks under /api/v1/billing (test mode only). Env FORGEBASE_BILLING_ENABLED, flag --billing.",
)
.env("FORGEBASE_BILLING_ENABLED")
.flag("--billing")];

/// Every module, in display order.
pub const FEATURES: &[FeatureDef] = &[
    FeatureDef {
        id: "server",
        name: "Server",
        summary: "HTTP listener, logging and database connection settings shared by every module.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/system"),
        docs: "docs/deployment.md",
        routes: &[],
        settings: SERVER,
    },
    FeatureDef {
        id: "database",
        name: "Database",
        summary: "Collections, records, schemas, indexes and SQL on the embedded SQLite database.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/database"),
        docs: "docs/adr/0003-data-core.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "auth",
        name: "Authentication",
        summary: "Administrators, users, sessions, API keys and sign-in methods.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/auth"),
        docs: "docs/adr/0004-auth.md",
        routes: &[],
        settings: AUTH,
    },
    FeatureDef {
        id: "kv",
        name: "Key-value store",
        summary: "Redis-style keys, hashes, lists and counters under /api/v1/kv; functions and the Redis listener use it.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/redis"),
        docs: "docs/adr/0005-kv.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "storage",
        name: "Object storage",
        summary: "Buckets and objects on local disk or S3, with uploads, signed links and a reconciler.",
        can_disable: true,
        enable_key: Some("storage.enabled"),
        studio_path: Some("/storage"),
        docs: "docs/adr/0018-object-storage.md",
        routes: &["/api/v1/storage/**"],
        settings: STORAGE,
    },
    FeatureDef {
        id: "functions",
        name: "Server functions",
        summary: "WebAssembly functions run on HTTP calls, record hooks, schedules and queue pushes.",
        can_disable: true,
        enable_key: Some("functions.enabled"),
        studio_path: Some("/functions"),
        docs: "docs/adr/0017-functions.md",
        routes: &["/api/v1/functions/**"],
        settings: FUNCTIONS,
    },
    FeatureDef {
        id: "jobs",
        name: "Jobs",
        summary: "Durable background jobs, schedules and workflows; every module's maintenance runs on it.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/jobs"),
        docs: "docs/adr/0007-jobs.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "queues",
        name: "Queues and topics",
        summary: "Durable topics with pull and push subscriptions, dead letters and replay.",
        can_disable: true,
        enable_key: Some("queue.enabled"),
        studio_path: Some("/queues"),
        docs: "docs/adr/0022-queues-pubsub.md",
        routes: &["/api/v1/queues/**"],
        settings: QUEUES,
    },
    FeatureDef {
        id: "realtime",
        name: "Realtime",
        summary: "Record changes, channels and presence over WebSocket and server-sent events.",
        can_disable: true,
        enable_key: Some("realtime.enabled"),
        studio_path: None,
        docs: "docs/adr/0011-realtime.md",
        routes: &["/api/v1/realtime/**"],
        settings: REALTIME,
    },
    FeatureDef {
        id: "vector",
        name: "Vector search",
        summary: "Embedding collections with approximate nearest-neighbour search.",
        can_disable: true,
        enable_key: Some("vector.enabled"),
        studio_path: Some("/vector"),
        docs: "docs/adr/0012-vector.md",
        routes: &["/api/v1/vector/**"],
        settings: VECTOR,
    },
    FeatureDef {
        id: "graph",
        name: "Graph",
        summary: "Nodes, edges, traversals and shortest paths over records.",
        can_disable: true,
        enable_key: Some("graph.enabled"),
        studio_path: Some("/graph"),
        docs: "docs/adr/0006-graph.md",
        routes: &["/api/v1/graph/**"],
        settings: GRAPH,
    },
    FeatureDef {
        id: "identity",
        name: "Identity provider",
        summary: "OpenID Connect provider for your own apps, and realms (separate user pools).",
        can_disable: true,
        enable_key: Some("identity.enabled"),
        studio_path: Some("/identity"),
        docs: "docs/adr/0019-identity-provider.md",
        routes: &[
            "/api/auth/oidc/**",
            "/api/auth/clients/**",
            "/api/auth/consents/**",
            "/api/auth/keys/oidc/**",
            "/api/auth/realms/*/keys/**",
            "/api/auth/realms/*/consents/**",
            "/api/v1/realms/**",
            "/api/v1/realm-bindings/**",
        ],
        settings: IDENTITY,
    },
    FeatureDef {
        id: "access",
        name: "Access control",
        summary: "Policies, roles, ACLs, organisations, teams, projects, invitations and suspensions.",
        can_disable: true,
        enable_key: Some("access.enabled"),
        studio_path: Some("/access"),
        docs: "docs/adr/0008-policy.md",
        routes: &[
            "/api/v1/acl/**",
            "/api/v1/authz/**",
            "/api/v1/invitations/**",
            "/api/v1/orgs/**",
            "/api/v1/overrides/**",
            "/api/v1/policies/**",
            "/api/v1/projects/**",
            "/api/v1/role-sets/**",
            "/api/v1/roles/**",
            "/api/v1/suspensions/**",
            "/api/v1/teams/**",
        ],
        settings: ACCESS,
    },
    FeatureDef {
        id: "resp",
        name: "Redis protocol",
        summary: "A RESP2/RESP3 listener so redis-cli and Redis clients reach the key-value store.",
        can_disable: true,
        enable_key: Some("resp.enabled"),
        studio_path: Some("/redis"),
        docs: "docs/adr/0013-resp.md",
        routes: &[],
        settings: RESP,
    },
    FeatureDef {
        id: "billing",
        name: "Billing",
        summary: "Plans, prices, subscriptions, invoices and Stripe, Razorpay and store webhooks (test mode).",
        can_disable: true,
        enable_key: Some("billing.enabled"),
        studio_path: Some("/billing"),
        docs: "docs/adr/0021-billing.md",
        routes: &["/api/v1/billing/**"],
        settings: BILLING,
    },
    FeatureDef {
        id: "entitlements",
        name: "Plans and quotas",
        summary: "Features, plans, add-ons, usage limits and plan-based rate limits.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/adr/0010-entitlements.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "credits",
        name: "Credits",
        summary: "Prepaid credit balances that pay for metered usage.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/adr/0016-credits.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "customize",
        name: "Customization",
        summary: "Custom domains, branding, email templates and agreements.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/adr/0020-product-customization.md",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "backups",
        name: "Backups",
        summary: "Online snapshots of the data directory under /api/v1/system/backups.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/system"),
        docs: "docs/adr/0024-backup-restore.md",
        routes: &[],
        settings: &[],
    },
];

/// The registry entry with this id.
pub fn feature(id: &str) -> Option<&'static FeatureDef> {
    FEATURES.iter().find(|f| f.id == id)
}

/// The setting with this dotted key.
pub fn setting(key: &str) -> Option<&'static SettingDef> {
    FEATURES.iter().flat_map(|f| f.settings.iter()).find(|s| s.key == key)
}

/// The modules that are off in `config`.
pub fn disabled(config: &Config) -> Vec<&'static FeatureDef> {
    FEATURES.iter().filter(|f| f.can_disable && !f.enabled(config)).collect()
}

/// Whether a route `pattern` (`*` = one segment, trailing `**` = any number
/// of segments) matches `path`. Empty segments (`//`) are ignored. Works on
/// concrete paths and on OpenAPI templates (`/api/auth/realms/{realm}/keys`).
pub fn route_matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/').filter(|s| !s.is_empty());
    let mut have = path.split('/').filter(|s| !s.is_empty());
    loop {
        match (want.next(), have.next()) {
            (Some("**"), _) => return true,
            (Some(w), Some(h)) if w == "*" || w == h => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// The disabled module (among `disabled`) that owns `path`, if any.
pub fn owner_of<'a>(disabled: &[&'a FeatureDef], path: &str) -> Option<&'a FeatureDef> {
    disabled
        .iter()
        .copied()
        .find(|f| f.routes.iter().any(|p| route_matches(p, path)))
}

/// The value of setting `key` in `config` as JSON (`None` for a secret or an
/// unknown key).
pub fn setting_value(config: &Config, key: &str) -> Option<Json> {
    if setting(key).is_some_and(|s| s.secret) {
        return None;
    }
    let doc = serde_json::to_value(config).ok()?;
    lookup(&doc, key)
}

fn lookup(doc: &Json, key: &str) -> Option<Json> {
    let mut cur = doc;
    for part in key.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur.clone())
}

/// Every setting's built-in default (secrets omitted). `data_dir` is the
/// relative default name, not an absolute path.
pub fn default_values() -> BTreeMap<&'static str, Json> {
    let defaults = Config::defaults(Path::new(""));
    let doc = serde_json::to_value(&defaults).unwrap_or(Json::Null);
    let mut out = BTreeMap::new();
    for s in FEATURES.iter().flat_map(|f| f.settings.iter()).filter(|s| !s.secret) {
        let v = if s.key == "data_dir" {
            Json::from(DEFAULT_DATA_DIR)
        } else {
            lookup(&doc, s.key).unwrap_or(Json::Null)
        };
        out.insert(s.key, v);
    }
    out
}

/// The source of `def` in `sources`: secrets report `env` when their
/// variable is set.
pub fn source_of(def: &SettingDef, sources: &ConfigSources) -> Source {
    if def.secret {
        return match def.env {
            Some(var) => sources.get(var),
            None => Source::Default,
        };
    }
    sources.get(def.key)
}

/// Whether the API may write `def` given its current source.
pub fn editable(def: &SettingDef, sources: &ConfigSources) -> bool {
    def.editable && !def.secret && !matches!(source_of(def, sources), Source::Env | Source::Flag)
}

// ------------------------------------------------------------------ writes

/// Why a configuration change was refused.
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// Unknown keys, wrong types, read-only settings or out-of-range values:
    /// `key -> reason`.
    #[error("invalid settings")]
    Invalid(BTreeMap<String, String>),
    /// Settings set by an environment variable or a flag, which the file
    /// cannot override: `key -> reason`.
    #[error("settings set by the environment or a flag")]
    Overridden(BTreeMap<String, String>),
    /// The current config file cannot be read or parsed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Writing the file failed.
    #[error("cannot write {path}: {source}")]
    Write {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A written change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchOutcome {
    /// The file that was written.
    pub written: PathBuf,
    /// Keys written, sorted.
    pub keys: Vec<String>,
    /// Whether any written key needs a restart to take effect.
    pub restart_required: bool,
}

/// Validates every key of `set`, then writes them all to the config file
/// (the loaded one, else `forgebase.toml` next to `data_dir`) in one atomic
/// replace that keeps comments and layout. Nothing is written when any key
/// is refused.
pub fn apply_patch(
    origin: &ConfigOrigin,
    data_dir: &Path,
    set: &serde_json::Map<String, Json>,
) -> Result<PatchOutcome, PatchError> {
    let path = origin.target_file(data_dir);
    let current = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(ConfigError::Read { path, source }.into()),
    };
    let base_text = current.clone().unwrap_or_default();
    // The file as it is must parse: never rewrite a file we cannot read.
    let (_, sources) = origin.candidate(&path, current.as_deref())?;

    let mut invalid = BTreeMap::new();
    let mut overridden = BTreeMap::new();
    let mut changes = Vec::new();
    if set.is_empty() {
        invalid.insert("set".to_owned(), "no settings given".to_owned());
    }
    for (key, value) in set {
        let Some(def) = setting(key) else {
            invalid.insert(key.clone(), "unknown setting".to_owned());
            continue;
        };
        if def.secret {
            invalid.insert(key.clone(), secret_reason(def));
            continue;
        }
        if !def.editable {
            invalid.insert(key.clone(), "read-only here; see the setting's help".to_owned());
            continue;
        }
        let item = match to_toml(def, value) {
            Ok(v) => v,
            Err(reason) => {
                invalid.insert(key.clone(), reason);
                continue;
            }
        };
        // Types the JSON check cannot see (a port above 65535): parse the
        // file with only this change.
        match set_keys(&base_text, std::slice::from_ref(&(def.key, item.clone()))) {
            Ok(text) => {
                if let Err(e) = origin.candidate(&path, Some(&text)) {
                    invalid.insert(key.clone(), parse_reason(&e));
                    continue;
                }
            }
            Err(reason) => {
                invalid.insert(key.clone(), reason);
                continue;
            }
        }
        match source_of(def, &sources) {
            Source::Env => {
                overridden.insert(key.clone(), overridden_reason(def, "an environment variable", def.env));
            }
            Source::Flag => {
                overridden.insert(key.clone(), overridden_reason(def, "a command-line flag", def.flag));
            }
            Source::Default | Source::File => changes.push((def.key, item)),
        }
    }
    if !invalid.is_empty() {
        return Err(PatchError::Invalid(invalid));
    }
    if !overridden.is_empty() {
        return Err(PatchError::Overridden(overridden));
    }
    let text = set_keys(&base_text, &changes).map_err(|reason| {
        PatchError::Invalid(changes.iter().map(|(k, _)| ((*k).to_owned(), reason.clone())).collect())
    })?;
    let (candidate, _) = origin.candidate(&path, Some(&text))?;
    let problems: BTreeMap<String, String> = candidate
        .problems()
        .into_iter()
        .filter_map(|e| match e {
            ConfigError::Invalid { field, message } => Some((field.to_owned(), message)),
            _ => None,
        })
        .collect();
    if !problems.is_empty() {
        return Err(PatchError::Invalid(problems));
    }
    write_atomic(&path, &text).map_err(|source| PatchError::Write {
        path: path.clone(),
        source,
    })?;
    let mut keys: Vec<String> = changes.iter().map(|(k, _)| (*k).to_owned()).collect();
    keys.sort();
    let restart_required = changes.iter().any(|(k, _)| setting(k).is_some_and(|s| s.restart_required));
    Ok(PatchOutcome {
        written: path,
        keys,
        restart_required,
    })
}

fn secret_reason(def: &SettingDef) -> String {
    match def.env {
        Some(var) => format!("a secret: set it with the environment variable {var}"),
        None => "a secret: it cannot be set through the API".to_owned(),
    }
}

fn overridden_reason(def: &SettingDef, what: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("set by {what} ({n}), which the config file cannot override"),
        None => format!("{} is set by {what}, which the config file cannot override", def.key),
    }
}

fn parse_reason(e: &ConfigError) -> String {
    match e {
        ConfigError::Parse { message, .. } => {
            // toml's message starts with the location; the last line is the reason.
            message
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(message)
                .trim()
                .to_owned()
        }
        other => other.to_string(),
    }
}

/// Checks `value` against `def` and converts it to a TOML value.
fn to_toml(def: &SettingDef, value: &Json) -> Result<toml_edit::Value, String> {
    match def.ty {
        SettingType::Bool => value.as_bool().map(toml_edit::Value::from).ok_or_else(|| "expected true or false".to_owned()),
        SettingType::Int => value
            .as_i64()
            .filter(|n| *n >= 0)
            .map(toml_edit::Value::from)
            .ok_or_else(|| "expected a non-negative integer".to_owned()),
        SettingType::String => value
            .as_str()
            .map(toml_edit::Value::from)
            .ok_or_else(|| "expected a string".to_owned()),
        SettingType::Enum => match value.as_str() {
            Some(s) if def.options.contains(&s) => Ok(toml_edit::Value::from(s)),
            _ => Err(format!("expected one of {}", def.options.join(", "))),
        },
        SettingType::Url => match value.as_str() {
            Some(s) if s.is_empty() || s.starts_with("https://") || s.starts_with("http://") => {
                Ok(toml_edit::Value::from(s))
            }
            _ => Err("expected an http:// or https:// URL".to_owned()),
        },
        SettingType::Duration => match value.as_str() {
            Some(s) if is_duration(s) => Ok(toml_edit::Value::from(s)),
            _ => Err("expected a duration such as \"30s\", \"500ms\", \"5m\" or \"1h\"".to_owned()),
        },
        SettingType::List => {
            let items = value.as_array().ok_or_else(|| "expected a list of strings".to_owned())?;
            let mut arr = toml_edit::Array::new();
            for i in items {
                arr.push(i.as_str().ok_or_else(|| "expected a list of strings".to_owned())?);
            }
            Ok(toml_edit::Value::Array(arr))
        }
    }
}

fn is_duration(s: &str) -> bool {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && matches!(&s[digits..], "ms" | "s" | "m" | "h")
}

/// Sets dotted `changes` in the TOML document `text`, keeping comments,
/// order and the decoration (such as a trailing comment) of replaced values.
/// A missing section is appended as a new `[section]` table.
pub fn set_keys(text: &str, changes: &[(&str, toml_edit::Value)]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    for (key, value) in changes {
        let (section, name) = match key.split_once('.') {
            Some((s, n)) => (Some(s), n),
            None => (None, *key),
        };
        let table: &mut dyn toml_edit::TableLike = match section {
            None => doc.as_table_mut(),
            Some(s) => {
                if !doc.contains_key(s) {
                    doc.insert(s, toml_edit::Item::Table(toml_edit::Table::new()));
                }
                doc.get_mut(s)
                    .and_then(toml_edit::Item::as_table_like_mut)
                    .ok_or_else(|| format!("[{s}] is not a table in the config file"))?
            }
        };
        match table.get_mut(name) {
            Some(item) => match item.as_value_mut() {
                Some(old) => {
                    let decor = old.decor().clone();
                    *old = value.clone();
                    *old.decor_mut() = decor;
                }
                None => *item = toml_edit::Item::Value(value.clone()),
            },
            None => {
                table.insert(name, toml_edit::Item::Value(value.clone()));
            }
        }
    }
    Ok(doc.to_string())
}

/// Replaces `path` with `text` atomically: a temporary file in the same
/// directory, fsynced, given the old file's permissions, then renamed over
/// the target (and the directory fsynced on Unix).
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("forgebase.toml");
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::{ConfigLoader, ConfigOverrides};

    const EXAMPLE: &str = include_str!("../../../forgebase.example.toml");

    fn keys_of(text: &str) -> Vec<String> {
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let mut out = Vec::new();
        for (k, item) in doc.iter() {
            match item.as_table_like() {
                Some(t) => out.extend(t.iter().map(|(n, _)| format!("{k}.{n}"))),
                None => out.push(k.to_owned()),
            }
        }
        out
    }

    #[test]
    fn registry_lists_every_module_and_every_example_setting() {
        let ids: Vec<&str> = FEATURES.iter().map(|f| f.id).collect();
        for id in [
            "server", "database", "auth", "storage", "functions", "jobs", "queues", "realtime", "vector", "graph",
            "identity", "access", "resp", "billing",
        ] {
            assert!(ids.contains(&id), "{id} listed");
        }
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "ids are unique");
        // Every key of the example file, the commented S3 ones included.
        let uncommented = EXAMPLE
            .lines()
            .map(|l| l.strip_prefix("# ").filter(|r| r.starts_with("s3_")).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let keys = keys_of(&uncommented);
        assert!(keys.len() > 30, "{keys:?}");
        for k in &keys {
            assert!(setting(k).is_some(), "{k} from forgebase.example.toml is in the registry");
        }
        // And every key `forgebase init` writes.
        for k in keys_of(crate::config::CONFIG_TEMPLATE) {
            assert!(setting(&k).is_some(), "{k} from the init template is in the registry");
        }
    }

    #[test]
    fn switchable_modules_list_their_switch_first() {
        for f in FEATURES {
            if f.can_disable {
                let key = f.enable_key.expect("switch key");
                assert_eq!(f.settings.first().map(|s| s.key), Some(key), "{}", f.id);
                assert_eq!(setting(key).unwrap().ty, SettingType::Bool);
            } else {
                assert!(f.enable_key.is_none() && f.routes.is_empty(), "{}", f.id);
            }
            for s in f.settings {
                if s.secret {
                    assert!(!s.editable, "{}", s.key);
                    if let Some(var) = s.env {
                        assert!(s.help.contains(var), "{} names {var}", s.key);
                    }
                }
                if s.ty == SettingType::Enum {
                    assert!(!s.options.is_empty(), "{}", s.key);
                }
            }
        }
        // Every non-secret setting has a value in the defaults.
        let defaults = default_values();
        let cfg = Config::defaults(Path::new("/srv/fb"));
        for s in FEATURES.iter().flat_map(|f| f.settings.iter()) {
            if s.secret {
                assert!(setting_value(&cfg, s.key).is_none(), "{} never has a value", s.key);
            } else {
                assert!(setting_value(&cfg, s.key).is_some(), "{} has a value", s.key);
                assert!(defaults.contains_key(s.key), "{} has a default", s.key);
            }
        }
        // Defaults: on, except the Redis listener (a network listener stays opt-in).
        for f in FEATURES.iter().filter(|f| f.can_disable) {
            assert_eq!(f.enabled(&cfg), f.id != "resp", "{}", f.id);
        }
    }

    #[test]
    fn route_patterns_match_segments() {
        assert!(route_matches("/api/v1/storage/**", "/api/v1/storage"));
        assert!(route_matches("/api/v1/storage/**", "/api/v1/storage/buckets/a/objects"));
        assert!(route_matches("/api/v1/storage/**", "//api/v1//storage/"));
        assert!(!route_matches("/api/v1/storage/**", "/api/v1/storagex"));
        assert!(!route_matches("/api/v1/storage/**", "/api/v1/stats"));
        assert!(route_matches("/api/auth/realms/*/keys/**", "/api/auth/realms/{realm}/keys"));
        assert!(!route_matches("/api/auth/realms/*/keys/**", "/api/auth/realms/default"));
        assert!(!route_matches("/api/auth/realms/*/keys/**", "/api/auth/realms/default/oauth-providers"));
        let cfg = {
            let mut c = Config::defaults(Path::new("/x"));
            c.identity.enabled = false;
            c.storage.enabled = false;
            c
        };
        let off = disabled(&cfg);
        assert_eq!(off.iter().map(|f| f.id).collect::<Vec<_>>(), ["storage", "identity", "resp"]);
        assert_eq!(owner_of(&off, "/api/auth/oidc/default/token").unwrap().id, "identity");
        assert_eq!(owner_of(&off, "/api/v1/storage/buckets").unwrap().id, "storage");
        assert!(owner_of(&off, "/api/auth/realms/default").is_none(), "realm sign-in settings stay with auth");
        assert!(owner_of(&off, "/api/auth/login").is_none());
        assert_eq!(
            feature("storage").unwrap().disabled_message(),
            "Object storage is disabled on this server"
        );
        assert_eq!(feature("billing").unwrap().disabled_message(), "Billing is disabled on this server");
    }

    fn load(dir: &Path, env: &[(&str, &str)], cli: ConfigOverrides) -> crate::config::LoadedConfig {
        let env = ConfigOverrides::from_env(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))).unwrap();
        ConfigLoader::new(dir).env(env).cli(cli).load().unwrap()
    }

    #[test]
    fn sources_are_default_file_env_and_flag() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("forgebase.toml"),
            "[server]\nport = 9000\nhost = \"127.0.0.1\"\n[storage]\nenabled = false\n",
        )
        .unwrap();
        let loaded = load(
            tmp.path(),
            &[("FORGEBASE_HOST", "0.0.0.0"), ("FORGEBASE_STORAGE_S3_SECRET_KEY", "x")],
            ConfigOverrides {
                log_level: Some("debug".into()),
                ..ConfigOverrides::default()
            },
        );
        let s = &loaded.sources;
        let src = |k: &str| source_of(setting(k).unwrap(), s);
        assert_eq!(src("server.port"), Source::File);
        assert_eq!(src("storage.enabled"), Source::File);
        assert_eq!(src("server.host"), Source::Env, "env beats the file");
        assert_eq!(src("log.level"), Source::Flag);
        assert_eq!(src("log.format"), Source::Default);
        assert_eq!(src("storage.s3_secret_key"), Source::Env, "a set secret variable");
        assert_eq!(src("storage.s3_access_key"), Source::Default);
        assert!(editable(setting("server.port").unwrap(), s));
        assert!(!editable(setting("server.host").unwrap(), s));
        assert!(!editable(setting("log.level").unwrap(), s));
        assert!(!editable(setting("storage.s3_secret_key").unwrap(), s));
        assert!(!editable(setting("data_dir").unwrap(), s));
        assert_eq!(loaded.config.server.host, "0.0.0.0");
        assert!(!loaded.config.storage.enabled);
        assert_eq!(setting_value(&loaded.config, "server.port"), Some(Json::from(9000)));
        // A setting equal to its default still reports the file.
        assert_eq!(src("server.host"), Source::Env);
        std::fs::write(tmp.path().join("forgebase.toml"), "[log]\nformat = \"pretty\"\n").unwrap();
        let again = load(tmp.path(), &[], ConfigOverrides::default());
        assert_eq!(source_of(setting("log.format").unwrap(), &again.sources), Source::File);
    }

    #[test]
    fn every_registry_setting_can_come_from_the_file() {
        // keys() and the file shape agree for every setting: write each one
        // and check that the loader reports it from the file.
        let tmp = tempfile::tempdir().unwrap();
        for s in FEATURES.iter().flat_map(|f| f.settings.iter()).filter(|s| s.editable && !s.secret) {
            let v = setting_value(&Config::defaults(tmp.path()), s.key).unwrap();
            let text = set_keys("", &[(s.key, to_toml(s, &v).unwrap())]).unwrap();
            std::fs::write(tmp.path().join("forgebase.toml"), &text).unwrap();
            let loaded = load(tmp.path(), &[], ConfigOverrides::default());
            assert_eq!(source_of(s, &loaded.sources), Source::File, "{} via {text}", s.key);
        }
    }

    fn origin(dir: &Path) -> (ConfigOrigin, PathBuf) {
        let loaded = load(dir, &[("FORGEBASE_PORT", "9100")], ConfigOverrides::default());
        (loaded.origin, loaded.config.data_dir)
    }

    fn set(pairs: &[(&str, Json)]) -> serde_json::Map<String, Json> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }

    #[test]
    fn patch_keeps_comments_and_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("forgebase.toml");
        std::fs::write(&file, EXAMPLE).unwrap();
        let (origin, data_dir) = origin(tmp.path());
        let out = apply_patch(
            &origin,
            &data_dir,
            &set(&[
                ("storage.enabled", Json::from(false)),
                ("storage.max_object_bytes", Json::from(1024)),
                ("log.format", Json::from("json")),
                ("graph.enabled", Json::from(false)),
            ]),
        )
        .unwrap();
        assert_eq!(out.written, file);
        assert_eq!(
            out.keys,
            ["graph.enabled", "log.format", "storage.enabled", "storage.max_object_bytes"]
        );
        assert!(out.restart_required);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# forgebase.example.toml: every configuration key"), "header kept");
        assert!(text.contains("# \"pretty\" (human-readable) or \"json\" (one object per line).\n"));
        assert!(text.contains("format = \"json\""));
        assert!(text.contains("max_object_bytes = 1024"));
        assert!(text.contains("# s3_endpoint = \"https://s3.eu-central-1.amazonaws.com\""), "commented keys kept");
        assert!(text.contains("[graph]\nenabled = false"), "new section appended");
        // Every other line of the example is still there, in order.
        let replaced = ["format = \"pretty\"", "max_object_bytes = 5368709120"];
        let mut have = text.lines();
        for want in EXAMPLE.lines().filter(|l| !replaced.contains(l)) {
            assert!(have.any(|h| h == want), "kept in order: {want:?}\n{text}");
        }
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary file left");
        let loaded = load(tmp.path(), &[], ConfigOverrides::default());
        assert!(!loaded.config.storage.enabled && !loaded.config.graph.enabled);
        assert_eq!(loaded.config.storage.max_object_bytes, 1024);
    }

    #[test]
    fn patch_keeps_trailing_comments_of_replaced_values() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("forgebase.toml");
        std::fs::write(&file, crate::config::CONFIG_TEMPLATE).unwrap();
        let (origin, data_dir) = origin(tmp.path());
        apply_patch(&origin, &data_dir, &set(&[("database.read_pool_size", Json::from(4))])).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("read_pool_size = 4          # FORGEBASE_DB_READ_POOL_SIZE"), "{text}");
    }

    #[test]
    fn patch_validates_every_key_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("forgebase.toml");
        std::fs::write(&file, "# mine\n[server]\nport = 9000\n").unwrap();
        let (origin, data_dir) = origin(tmp.path());
        let before = std::fs::read_to_string(&file).unwrap();
        let Err(PatchError::Invalid(d)) = apply_patch(
            &origin,
            &data_dir,
            &set(&[
                ("nope.key", Json::from(1)),
                ("log.format", Json::from("xml")),
                ("storage.enabled", Json::from("yes")),
                ("server.request_timeout_secs", Json::from(-1)),
                ("resp.port", Json::from(70_000)),
                ("storage.s3_secret_key", Json::from("s")),
                ("data_dir", Json::from("/elsewhere")),
                ("storage.s3_endpoint", Json::from("ftp://x")),
                ("graph.enabled", Json::from(false)),
            ]),
        ) else {
            panic!("refused")
        };
        assert_eq!(d["nope.key"], "unknown setting");
        assert_eq!(d["log.format"], "expected one of pretty, json");
        assert_eq!(d["storage.enabled"], "expected true or false");
        assert_eq!(d["server.request_timeout_secs"], "expected a non-negative integer");
        assert!(d["resp.port"].contains("u16") || d["resp.port"].contains("invalid"), "{}", d["resp.port"]);
        assert!(d["storage.s3_secret_key"].contains("FORGEBASE_STORAGE_S3_SECRET_KEY"));
        assert!(d.contains_key("data_dir"));
        assert!(d.contains_key("storage.s3_endpoint"));
        assert!(!d.contains_key("graph.enabled"), "valid keys are not reported");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before, "nothing written");

        // Range checks of the resolved configuration, all at once.
        let Err(PatchError::Invalid(d)) = apply_patch(
            &origin,
            &data_dir,
            &set(&[
                ("server.request_timeout_secs", Json::from(0)),
                ("database.read_pool_size", Json::from(65)),
                ("storage.driver", Json::from("s3")),
            ]),
        ) else {
            panic!("refused")
        };
        assert_eq!(d["server.request_timeout_secs"], "must be between 1 and 3600");
        assert_eq!(d["database.read_pool_size"], "must be between 1 and 64");
        assert!(d["storage.s3_endpoint"].contains("s3_endpoint and s3_bucket"));
        // Cross-field changes pass together.
        apply_patch(
            &origin,
            &data_dir,
            &set(&[
                ("storage.driver", Json::from("s3")),
                ("storage.s3_endpoint", Json::from("https://s3.example.com")),
                ("storage.s3_bucket", Json::from("b")),
            ]),
        )
        .unwrap();
        assert!(matches!(
            apply_patch(&origin, &data_dir, &serde_json::Map::new()),
            Err(PatchError::Invalid(_))
        ));
    }

    #[test]
    fn patch_refuses_settings_from_env_and_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let env = ConfigOverrides::from_env([("FORGEBASE_PORT".to_owned(), "9100".to_owned())]).unwrap();
        let cli = ConfigOverrides {
            log_level: Some("debug".into()),
            ..ConfigOverrides::default()
        };
        let loaded = ConfigLoader::new(tmp.path()).env(env).cli(cli).load().unwrap();
        let Err(PatchError::Overridden(d)) = apply_patch(
            &loaded.origin,
            &loaded.config.data_dir,
            &set(&[("server.port", Json::from(1)), ("log.level", Json::from("info"))]),
        ) else {
            panic!("409")
        };
        assert!(d["server.port"].contains("FORGEBASE_PORT"));
        assert!(d["log.level"].contains("--log-level"));
        assert!(!tmp.path().join("forgebase.toml").exists(), "nothing written");
    }

    #[test]
    fn patch_creates_a_file_next_to_the_data_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (origin, data_dir) = origin(tmp.path());
        assert!(origin.loaded_file().is_none());
        let out = apply_patch(&origin, &data_dir, &set(&[("vector.enabled", Json::from(false))])).unwrap();
        assert_eq!(out.written, tmp.path().join("forgebase.toml"));
        let (cfg, sources) = origin.resolve(&out.written).unwrap();
        assert!(!cfg.vector.enabled);
        assert_eq!(sources.get("vector.enabled"), Source::File);
        assert_eq!(cfg.server.port, 9100, "env still applies on top");
        // An embedded configuration (built in code) is the base of the file layer.
        let mut code = Config::defaults(tmp.path());
        code.server.port = 0;
        let emb = ConfigOrigin::embedded(&code);
        assert_eq!(emb.target_file(&code.data_dir), out.written);
        let (cfg, _) = emb.resolve(&out.written).unwrap();
        assert_eq!(cfg.server.port, 0);
        assert!(!cfg.vector.enabled);
    }

    #[test]
    fn precedence_is_unchanged_for_the_new_switches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("forgebase.toml"), "[functions]\nenabled = false\n[queue]\nenabled = false\n")
            .unwrap();
        let file_only = load(tmp.path(), &[], ConfigOverrides::default());
        assert!(!file_only.config.functions.enabled && !file_only.config.queue.enabled);
        let env_on = load(
            tmp.path(),
            &[("FORGEBASE_FUNCTIONS_ENABLED", "true"), ("FORGEBASE_QUEUE_ENABLED", "1")],
            ConfigOverrides::default(),
        );
        assert!(env_on.config.functions.enabled && env_on.config.queue.enabled, "env beats file");
        let flag_off = load(
            tmp.path(),
            &[("FORGEBASE_FUNCTIONS_ENABLED", "true")],
            ConfigOverrides {
                functions_enabled: Some(false),
                ..ConfigOverrides::default()
            },
        );
        assert!(!flag_off.config.functions.enabled, "flag beats env");
        assert_eq!(
            source_of(setting("functions.enabled").unwrap(), &flag_off.sources),
            Source::Flag
        );
    }
}
