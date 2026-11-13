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
    /// A table of text values (`[database.postgres.settings]`).
    Map,
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
            SettingType::Map => "map",
        }
    }
}

/// One setting of the registry.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted key, as in `forgebase.toml` (`storage.max_object_bytes`).
    pub key: &'static str,
    /// Short label, sentence case. Shown as-is in the dashboard.
    pub label: &'static str,
    /// What the setting changes and a sensible value, in plain words with
    /// jargon spelled out. Shown as-is in the dashboard; the environment
    /// variable and flag are separate fields.
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
    /// Example value for messages when the default is empty.
    pub example: Option<&'static str>,
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
            example: None,
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

    const fn example(mut self, example: &'static str) -> Self {
        self.example = Some(example);
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
            example: None,
        }
    }
}

/// One module of the registry.
#[derive(Debug, Clone, Copy)]
pub struct FeatureDef {
    /// Stable id (`storage`).
    pub id: &'static str,
    /// Display name (sentence case), also used in the `feature_disabled`
    /// message.
    pub name: &'static str,
    /// One plain sentence on what it does, for a developer new to ForgeBase.
    pub summary: &'static str,
    /// Whether `[<section>] enabled = false` turns it off. When `false`, the
    /// module is always on (ADR-0026 says why for each).
    pub can_disable: bool,
    /// The setting that switches it (first in [`FeatureDef::settings`]).
    pub enable_key: Option<&'static str>,
    /// Dashboard route of its studio.
    pub studio_path: Option<&'static str>,
    /// Documentation, relative to the repository root, with the anchor of
    /// an existing heading (`docs/adr/0018-object-storage.md#adr-0018-object-storage`).
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

use SettingType::{Bool, Enum, Int, List, Map, String as Text, Url};

/// `[database]` backend choice and `[database.postgres]` (ADR-0028 §6.1,
/// ADR-0029 §4.1). The SQLite keys stay under the server module.
const DATABASE: &[SettingDef] = &[
    SettingDef::new(
        "database.backend",
        "Database engine",
        Enum,
        "Which database holds your data: postgres (PostgreSQL, the default of the standard release) or sqlite (SQLite, one file, the default of the lite release). It is set when the project is created; moving existing data to the other engine needs forgebase db convert, so it cannot be changed here.",
    )
    .options(&["postgres", "sqlite"])
    .env("FORGEBASE_DATABASE_BACKEND")
    .read_only(),
    SettingDef::new(
        "database.postgres.mode",
        "PostgreSQL server",
        Enum,
        "PostgreSQL only: managed runs the PostgreSQL server bundled with ForgeBase inside the data directory, external uses a server you run, named by FORGEBASE_DATABASE_URL. Keep managed unless you already operate PostgreSQL.",
    )
    .options(&["managed", "external"])
    .env("FORGEBASE_DATABASE_POSTGRES_MODE")
    .read_only(),
    SettingDef::secret(
        "database.postgres.url",
        "External server address",
        Some("FORGEBASE_DATABASE_URL"),
        "External PostgreSQL only: the address of your server with its user and password, such as postgres://forgebase:secret@db.example.com/forgebase. It is a secret, so set it with the environment variable FORGEBASE_DATABASE_URL (or in the .env file next to forgebase.toml), not here.",
    ),
    SettingDef::new(
        "database.postgres.pool_size",
        "Read connections (PostgreSQL)",
        Int,
        "PostgreSQL only: number of connections that serve reads in parallel, from 1 to 64; writes always use one connection. 16 suits most servers.",
    )
    .env("FORGEBASE_DATABASE_POSTGRES_POOL_SIZE"),
    SettingDef::new(
        "database.postgres.listen",
        "Direct TCP logins",
        Text,
        "Bundled PostgreSQL only: empty (the default) accepts logins through a Unix socket in the data directory only, and no network port is open. An address with a port, such as 127.0.0.1:5433 (this machine) or 0.0.0.0:5433 (other hosts too), also accepts logins over TCP, always encrypted with TLS (Transport Layer Security). Only named direct logins may log in from other hosts. It opens a network port, so it is set where the server starts, not here.",
    )
    .env("FORGEBASE_DATABASE_POSTGRES_LISTEN")
    .example("127.0.0.1:5433")
    .read_only(),
    SettingDef::new(
        "database.postgres.tls_cert",
        "Certificate for TCP logins",
        Text,
        "Bundled PostgreSQL only, with a listen address: a PEM certificate file (with its chain) for encrypted TCP logins, such as /etc/forgebase/pg.crt. Empty (the default) uses a self-signed certificate ForgeBase generates in the data directory, which clients accept with sslmode=require. Set it together with the key file.",
    )
    .env("FORGEBASE_DATABASE_POSTGRES_TLS_CERT")
    .example("/etc/forgebase/pg.crt")
    .read_only(),
    SettingDef::new(
        "database.postgres.tls_key",
        "Private key for TCP logins",
        Text,
        "Bundled PostgreSQL only, with a listen address: the PEM private key of the certificate, such as /etc/forgebase/pg.key, readable by the server's user only.",
    )
    .env("FORGEBASE_DATABASE_POSTGRES_TLS_KEY")
    .example("/etc/forgebase/pg.key")
    .read_only(),
    SettingDef::new(
        "database.postgres.engine_dir",
        "Engine folder",
        Text,
        "Bundled PostgreSQL only: a folder with an installed PostgreSQL 17 or 18 to run instead of the one inside ForgeBase, such as /opt/forgebase/engine shared by several projects. Empty (the default) uses the bundled engine. ForgeBase runs the programs in this folder, so it is set where the server starts, not here.",
    )
    .env("FORGEBASE_DATABASE_POSTGRES_ENGINE_DIR")
    .example("/opt/forgebase/engine")
    .read_only(),
    SettingDef::new(
        "database.postgres.native_rls",
        "Native row-level security",
        Enum,
        "PostgreSQL only: whether PostgreSQL itself enforces your access rules (row-level security). logins applies them to direct database logins, enforce also to the server's own connections, off never. Keep logins.",
    )
    .options(&["off", "logins", "enforce"])
    .env("FORGEBASE_DATABASE_POSTGRES_NATIVE_RLS"),
    SettingDef::new(
        "database.postgres.settings",
        "Engine settings",
        Map,
        "Bundled PostgreSQL only: extra postgresql.conf parameters, such as shared_buffers = \"256MB\" or work_mem = \"16MB\". Settings ForgeBase manages itself (where the server listens, preloaded libraries, replication and anything that runs a program) are refused. Edit them in the [database.postgres.settings] table of forgebase.toml.",
    )
    .read_only(),
    SettingDef::new(
        "database.postgres.extensions.vector",
        "Vector search on pgvector",
        Enum,
        "PostgreSQL only: auto (the default) stores vectors with the pgvector extension when the server has it, on refuses to start without it, off always uses the built-in vector engine.",
    )
    .options(&["auto", "on", "off"])
    .env("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_VECTOR"),
    SettingDef::new(
        "database.postgres.extensions.graph",
        "Graph queries on Apache AGE",
        Enum,
        "PostgreSQL only: auto (the default) answers openCypher graph queries with the Apache AGE extension when the server has it, on refuses to start without it, off always uses the built-in graph engine.",
    )
    .options(&["auto", "on", "off"])
    .env("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_GRAPH"),
    SettingDef::new(
        "database.postgres.extensions.queue",
        "Queue deliveries on pgmq",
        Enum,
        "PostgreSQL only: auto (the default) delivers queue messages with the pgmq extension when the server has it, on refuses to start without it, off always uses the built-in delivery tables.",
    )
    .options(&["auto", "on", "off"])
    .env("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_QUEUE"),
    SettingDef::new(
        "database.postgres.extensions.cron",
        "Schedules on pg_cron",
        Enum,
        "PostgreSQL only: auto (the default) fires schedules with the pg_cron extension when the server has it, on refuses to start without it, off always uses the built-in scheduler.",
    )
    .options(&["auto", "on", "off"])
    .env("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_CRON"),
    SettingDef::new(
        "database.postgres.extensions.trigram",
        "Fuzzy search on pg_trgm",
        Enum,
        "PostgreSQL only: auto (the default) builds trigram indexes for fuzzy text search with the pg_trgm extension when the server has it, on refuses to start without it, off leaves fuzzy search out.",
    )
    .options(&["auto", "on", "off"])
    .env("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_TRIGRAM"),
    // FB-175 (ADR-0029 §4.1): how replicas and shards behave. The topology
    // itself (servers, shards, tenants) changes through the Database
    // topology screen, not here.
    SettingDef::new(
        "database.replicas.read_routing",
        "Reads from replicas",
        Enum,
        "PostgreSQL with read replicas only: replicas (the default) sends reads that may be slightly behind, such as lists and searches, to a healthy read replica (a copy of the database that follows the primary server), primary sends every read to the primary server. Without replicas nothing changes.",
    )
    .options(&["primary", "replicas"])
    .env("FORGEBASE_DATABASE_REPLICAS_READ_ROUTING"),
    SettingDef::new(
        "database.replicas.read_your_writes",
        "Read your own writes",
        Bool,
        "PostgreSQL with read replicas only: on (the default) makes a caller's reads always see its own earlier writes, waiting briefly for a replica or reading the primary server instead. Leave it on unless every client tolerates reading slightly older data.",
    )
    .env("FORGEBASE_DATABASE_REPLICAS_READ_YOUR_WRITES"),
    SettingDef::new(
        "database.replicas.lsn_wait_ms",
        "Replica catch-up wait",
        Int,
        "PostgreSQL with read replicas only: how long, in milliseconds, a read waits for a replica to catch up with the caller's last write before it reads the primary server instead, from 0 to 10000. 50 suits most servers.",
    )
    .unit("ms")
    .env("FORGEBASE_DATABASE_REPLICAS_LSN_WAIT_MS"),
    SettingDef::new(
        "database.replicas.max_lag_ms",
        "Most replica delay",
        Int,
        "PostgreSQL with read replicas only: a replica more than this many milliseconds behind the primary server stops serving reads until it catches up, from 1 to 3600000. 2000 (two seconds) suits most servers.",
    )
    .unit("ms")
    .env("FORGEBASE_DATABASE_REPLICAS_MAX_LAG_MS"),
    SettingDef::new(
        "database.replicas.max_lag_bytes",
        "Most replica backlog",
        Int,
        "PostgreSQL with read replicas only: a replica that has more than this many bytes of changes left to apply stops serving reads until it catches up. 67108864 (64 MB) suits most servers.",
    )
    .unit("bytes")
    .env("FORGEBASE_DATABASE_REPLICAS_MAX_LAG_BYTES"),
    SettingDef::new(
        "database.replicas.health_interval_ms",
        "Replica health check interval",
        Int,
        "PostgreSQL with read replicas only: how often, in milliseconds, ForgeBase measures each replica's delay and takes it in or out of service, from 50 to 60000. 500 suits most servers.",
    )
    .unit("ms")
    .env("FORGEBASE_DATABASE_REPLICAS_HEALTH_INTERVAL_MS"),
    SettingDef::new(
        "database.replicas.slot_wal_keep_size",
        "Change log kept for replicas",
        Text,
        "Bundled PostgreSQL with replicas only: the most write-ahead log (the change log replicas copy from) the primary server keeps for a replica that falls behind or is switched off, such as 8GB (the default), 512MB or -1 for no limit. A replica that falls further behind must be rebuilt; a bigger value uses more disk space.",
    )
    .env("FORGEBASE_DATABASE_REPLICAS_SLOT_WAL_KEEP_SIZE"),
    SettingDef::new(
        "database.sharding.placement",
        "Where new tenants go",
        Enum,
        "PostgreSQL with shards only: least_loaded (the default) puts each new tenant (a project environment) on the shard with the most room for its weight, control keeps new tenants on the main database. Existing tenants stay where they are.",
    )
    .options(&["least_loaded", "control"])
    .env("FORGEBASE_DATABASE_SHARDING_PLACEMENT"),
    SettingDef::new(
        "database.sharding.project_affinity",
        "Keep a project together",
        Bool,
        "PostgreSQL with shards only: on (the default) puts every environment of a project on the shard of its first environment, so they can be moved and backed up together. Leave it on unless one project is too big for one shard.",
    )
    .env("FORGEBASE_DATABASE_SHARDING_PROJECT_AFFINITY"),
    SettingDef::new(
        "database.sharding.move_freeze_timeout_ms",
        "Most pause during a move",
        Int,
        "PostgreSQL with shards only: the longest time, in milliseconds, a tenant's requests wait while a move switches it to its new shard, from 100 to 600000; a move that would take longer is cancelled and the tenant stays where it was. 10000 (ten seconds) suits most servers.",
    )
    .unit("ms")
    .env("FORGEBASE_DATABASE_SHARDING_MOVE_FREEZE_TIMEOUT_MS"),
    SettingDef::new(
        "database.sharding.move_source_retention_hours",
        "Keep moved data for",
        Int,
        "PostgreSQL with shards only: how many hours a moved tenant's rows stay on its old shard, so the move can be undone, from 0 to 720. 24 suits most servers.",
    )
    .unit("hours")
    .env("FORGEBASE_DATABASE_SHARDING_MOVE_SOURCE_RETENTION_HOURS"),
    SettingDef::new(
        "database.sharding.max_parallel_moves",
        "Moves at the same time",
        Int,
        "PostgreSQL with shards only: how many tenant moves may run at the same time, from 1 to 16. 1 keeps the extra load on your servers lowest.",
    )
    .env("FORGEBASE_DATABASE_SHARDING_MAX_PARALLEL_MOVES"),
    SettingDef::new(
        "database.nodes",
        "Servers declared in the file",
        List,
        "PostgreSQL only: read replicas and shard servers you run yourself, declared as [[database.nodes]] tables in forgebase.toml with a name, a set (control or a shard name), a role, a weight and url_env, the environment variable that holds the server's address, such as name = \"eu-read-1\" with url_env = \"FORGEBASE_DB_NODE_EU_READ_1_URL\". They are added when the server starts and shown as from forgebase.toml. Edit them in the file; add other servers from the Database topology screen.",
    )
    .read_only(),
];

const SERVER: &[SettingDef] = &[
    SettingDef::new(
        "data_dir",
        "Data directory",
        Text,
        "Folder that holds the database, uploaded files, search indexes and backups. The default forgebase-data, next to the config file, suits most installs. Change it when starting the server, not here: a new folder means different data.",
    )
    .env("FORGEBASE_DATA_DIR")
    .flag("--data-dir")
    .read_only(),
    SettingDef::new(
        "server.host",
        "Listen address",
        Text,
        "Network address the HTTP server listens on. Keep 127.0.0.1 and put a reverse proxy that handles HTTPS in front; use 0.0.0.0 only to listen on every network interface.",
    )
    .env("FORGEBASE_HOST")
    .flag("--host"),
    SettingDef::new(
        "server.port",
        "Port",
        Int,
        "TCP port of the HTTP server, such as 8090 (the default). 0 picks a free port.",
    )
    .env("FORGEBASE_PORT")
    .flag("--port"),
    SettingDef::new(
        "server.public_url",
        "Public URL",
        Url,
        "Address people and apps use to reach this server, with no path, such as https://forgebase.example.com. Links the server hands out, such as the setup link, start with it. Leave it empty to use the listen address and port.",
    )
    .env("FORGEBASE_PUBLIC_URL")
    .example("https://forgebase.example.com"),
    SettingDef::new(
        "server.request_timeout_secs",
        "Request timeout",
        Int,
        "Longest time one request may run before it is cancelled, from 1 to 3600 seconds. 30 suits most apps.",
    )
    .unit("seconds")
    .env("FORGEBASE_REQUEST_TIMEOUT_SECS"),
    SettingDef::new(
        "server.shutdown_timeout_secs",
        "Shutdown grace period",
        Int,
        "How long the server waits for running requests to finish when it stops, up to 3600 seconds. 30 is a good value.",
    )
    .unit("seconds")
    .env("FORGEBASE_SHUTDOWN_TIMEOUT_SECS"),
    SettingDef::new(
        "server.ops_access",
        "Who can read metrics",
        Enum,
        "Who may read /metrics and /api/system/info: loopback (administrators and programs on this machine), admin (administrators only) or public (anyone). Keep loopback unless a monitoring tool on another machine needs them.",
    )
    .options(&["loopback", "admin", "public"])
    .env("FORGEBASE_OPS_ACCESS"),
    SettingDef::new(
        "log.level",
        "Log level",
        Text,
        "Which log lines are written: error, warn, info, debug or trace, also per module such as forgebase=debug. Use info day to day and debug while you look into a problem.",
    )
    .env("FORGEBASE_LOG_LEVEL")
    .flag("--log-level"),
    SettingDef::new(
        "log.format",
        "Log format",
        Enum,
        "How log lines look: pretty is easy to read in a terminal; json writes one JSON object per line for log collectors. Use pretty on your machine and json in production.",
    )
    .options(&["pretty", "json"])
    .env("FORGEBASE_LOG_FORMAT")
    .flag("--log-format"),
    SettingDef::new(
        "database.synchronous",
        "Write durability",
        Enum,
        "How safely commits are written: full writes every commit to disk before answering, so a power cut loses nothing; normal is faster, but a power cut can lose the last few commits. Keep full unless losing recent writes is acceptable.",
    )
    .options(&["full", "normal"])
    .env("FORGEBASE_DB_SYNCHRONOUS"),
    SettingDef::new(
        "database.busy_timeout_ms",
        "Lock wait time",
        Int,
        "How long the database waits while another process (the command line tool or a backup) holds a lock, up to 600000 milliseconds. 5000 (5 seconds) is a good value.",
    )
    .unit("ms")
    .env("FORGEBASE_DB_BUSY_TIMEOUT_MS"),
    SettingDef::new(
        "database.read_pool_size",
        "Read connections",
        Int,
        "Number of database connections that serve reads in parallel, from 1 to 64. 8 suits most servers; raise it on machines with many CPU cores.",
    )
    .env("FORGEBASE_DB_READ_POOL_SIZE"),
];

const AUTH: &[SettingDef] = &[
    SettingDef::new(
        "auth.trust_forwarded_for",
        "Trust X-Forwarded-For",
        Bool,
        "Take the client's IP address from the X-Forwarded-For header that a reverse proxy adds. Turn it on only behind a proxy you run; leave it off when clients connect directly, or they can fake their address.",
    )
    .env("FORGEBASE_AUTH_TRUST_FORWARDED_FOR"),
    SettingDef::new(
        "auth.allow_private_outbound",
        "Allow private network calls",
        Bool,
        "Let sign-in providers (OAuth, OpenID Connect), SMS gateways and Cloudflare Turnstile checks reach this machine or your private network. Leave it off unless such a service runs on your own network.",
    )
    .env("FORGEBASE_AUTH_ALLOW_PRIVATE_OUTBOUND"),
    SettingDef::secret(
        "auth.master_key",
        "Master key",
        None,
        "Encrypts stored secrets such as sign-in provider keys. It is the file <data dir>/auth/master.key, created on first start, and cannot be set here. Back it up with the data directory.",
    ),
];

const STORAGE: &[SettingDef] = &[
    SettingDef::new(
        "storage.enabled",
        "Object storage",
        Bool,
        "Turns file storage on or off: buckets, uploads and downloads under /api/v1/storage, and the cleanup of unused files. Leave it on unless your app stores no files.",
    )
    .env("FORGEBASE_STORAGE_ENABLED"),
    SettingDef::new(
        "storage.driver",
        "Storage driver",
        Enum,
        "Where files are kept: local stores them under <data dir>/storage; s3 uses Amazon S3 or an S3-compatible service and needs an endpoint and a bucket. Start with local.",
    )
    .options(&["local", "s3"]),
    SettingDef::new(
        "storage.max_object_bytes",
        "Largest file",
        Int,
        "Largest file one upload may store, in bytes; a bucket can set a lower limit. The default 5368709120 is 5 GiB.",
    )
    .unit("bytes"),
    SettingDef::new(
        "storage.upload_ttl_secs",
        "Upload session lifetime",
        Int,
        "How long an unfinished multipart or resumable upload can be continued, in seconds. 86400 (one day) is a good value.",
    )
    .unit("seconds"),
    SettingDef::new(
        "storage.reconcile_grace_secs",
        "Cleanup grace period",
        Int,
        "How long a file must be unused before the cleanup job deletes it, in seconds, so uploads in progress are never removed. 3600 (one hour) is a good value.",
    )
    .unit("seconds"),
    SettingDef::new(
        "storage.s3_endpoint",
        "S3 endpoint",
        Url,
        "S3 driver only: address of the S3 service, such as https://s3.eu-central-1.amazonaws.com, or the address of an S3-compatible service.",
    )
    .example("https://s3.eu-central-1.amazonaws.com"),
    SettingDef::new(
        "storage.s3_region",
        "S3 region",
        Text,
        "S3 driver only: region of the bucket, used to sign requests, such as us-east-1 (the default) or eu-central-1.",
    ),
    SettingDef::new(
        "storage.s3_bucket",
        "S3 bucket",
        Text,
        "S3 driver only: name of the bucket that holds the files, such as my-app-files. Create the bucket in your S3 service first.",
    ),
    SettingDef::new(
        "storage.s3_prefix",
        "S3 folder prefix",
        Text,
        "S3 driver only: folder inside the bucket for ForgeBase's files, such as forgebase/. Leave it empty to use the whole bucket.",
    ),
    SettingDef::new(
        "storage.s3_part_size",
        "S3 upload part size",
        Int,
        "S3 driver only: size of each part of a large upload, in bytes. S3 needs at least 5242880 (5 MiB); the default 8388608 (8 MiB) suits most services.",
    )
    .unit("bytes"),
    SettingDef::secret(
        "storage.s3_access_key",
        "S3 access key",
        Some("FORGEBASE_STORAGE_S3_ACCESS_KEY"),
        "S3 driver only: the access key id. It is a secret, so set it with the environment variable FORGEBASE_STORAGE_S3_ACCESS_KEY, not here.",
    ),
    SettingDef::secret(
        "storage.s3_secret_key",
        "S3 secret key",
        Some("FORGEBASE_STORAGE_S3_SECRET_KEY"),
        "S3 driver only: the secret access key. It is a secret, so set it with the environment variable FORGEBASE_STORAGE_S3_SECRET_KEY, not here.",
    ),
    SettingDef::secret(
        "storage.s3_session_token",
        "S3 session token",
        Some("FORGEBASE_STORAGE_S3_SESSION_TOKEN"),
        "S3 driver only, optional: a temporary session token. It is a secret, so set it with the environment variable FORGEBASE_STORAGE_S3_SESSION_TOKEN, not here.",
    ),
];

const FUNCTIONS: &[SettingDef] = &[SettingDef::new(
    "functions.enabled",
    "Server functions",
    Bool,
    "Turns server functions on or off: /api/v1/functions, record hooks, and functions run by schedules or queues. When off, record writes skip their hooks. Leave it on if you deploy functions.",
)
.env("FORGEBASE_FUNCTIONS_ENABLED")];

const QUEUES: &[SettingDef] = &[
    SettingDef::new(
        "queue.enabled",
        "Queues and topics",
        Bool,
        "Turns queues on or off: /api/v1/queues, delivery to webhooks and functions, and queue cleanup. Leave it on if your app publishes messages.",
    )
    .env("FORGEBASE_QUEUE_ENABLED"),
    SettingDef::new(
        "queue.max_message_bytes",
        "Largest message",
        Int,
        "Largest message (data, attributes and ordering key) a topic accepts unless it sets its own limit, from 64 to 10485760 bytes. The default 1048576 is 1 MiB.",
    )
    .unit("bytes"),
    SettingDef::new(
        "queue.default_retention_secs",
        "Default retention",
        Int,
        "How long messages are kept in topics that do not set their own retention, at least 60 seconds. The default 604800 is 7 days.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.max_retention_secs",
        "Longest retention",
        Int,
        "Longest retention a topic may ask for, at least the default retention. The default 2678400 is 31 days.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.dedup_window_secs",
        "Duplicate detection window",
        Int,
        "A message sent again with the same dedup_id within this time is stored once, from 1 to 86400 seconds. 600 (10 minutes) is a good value.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.max_pull_wait_ms",
        "Longest pull wait",
        Int,
        "Longest time a pull request waits for new messages before it answers with none, up to 300000 milliseconds. Keep it below the request timeout; the default 20000 is 20 seconds.",
    )
    .unit("ms"),
    SettingDef::new(
        "queue.poll_interval_ms",
        "Maintenance interval",
        Int,
        "How often expired leases and old messages are cleaned up and push deliveries run, from 10 to 60000 milliseconds. 1000 (one second) is a good value.",
    )
    .unit("ms"),
    SettingDef::new(
        "queue.push_timeout_secs",
        "Push timeout",
        Int,
        "Longest time one delivery to a webhook or function may take, from 1 to 300 seconds. 10 is a good value.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.push_concurrency",
        "Parallel pushes",
        Int,
        "Number of push subscriptions delivered at the same time, from 1 to 256. 8 suits most servers.",
    ),
    SettingDef::new(
        "queue.allow_http_push",
        "Allow http:// webhooks",
        Bool,
        "Accept webhook addresses that start with http:// instead of https://. Turn it on only for local development; leave it off in production.",
    ),
    SettingDef::new(
        "queue.dead_letter_retention_days",
        "Dead-letter retention",
        Int,
        "How many days messages that failed delivery too many times (dead letters) are kept for inspection and replay. 14 is a good value.",
    )
    .unit("days"),
    SettingDef::new(
        "queue.push_response_body_bytes",
        "Logged response body",
        Int,
        "How much of a webhook's response body each delivery attempt keeps in the attempt log, from 0 to 65536 bytes. 4096 is a good value.",
    )
    .unit("bytes"),
    SettingDef::new(
        "queue.push_attempts_per_subscription",
        "Attempts kept per subscription",
        Int,
        "How many recent delivery attempts the attempt log keeps for each push subscription, from 1 to 100000. 1000 is a good value.",
    ),
    SettingDef::new(
        "queue.push_attempts_retention_secs",
        "Attempt log retention",
        Int,
        "How long delivery attempts stay in the attempt log, from 60 seconds to 31 days. 604800 (7 days) is a good value.",
    )
    .unit("seconds"),
    SettingDef::new(
        "queue.push_test_sends_per_minute",
        "Test sends per minute",
        Int,
        "How many test events one push subscription may send per minute, from 1 to 1000. 10 is a good value.",
    ),
];

const REALTIME: &[SettingDef] = &[
    SettingDef::new(
        "realtime.enabled",
        "Realtime",
        Bool,
        "Turns live updates on or off: WebSocket, server-sent events (SSE) and channels under /api/v1/realtime. When off, the change log that functions and queues read is still written. Leave it on if your app shows live data.",
    )
    .env("FORGEBASE_REALTIME_ENABLED"),
    SettingDef::new(
        "realtime.max_connections_per_principal",
        "Connections per user or key",
        Int,
        "Most live connections one user or API key may hold at once, from 1 to 1000000. 32 covers several open tabs and devices.",
    )
    .env("FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL"),
];

const KV: &[SettingDef] = &[SettingDef::new(
    "kv.tenant_max_bytes",
    "Memory per tenant",
    Int,
    "Most memory one tenant's keys may use, in bytes; 0 means no per-tenant limit. Above it the tenant's own least recently used keys are evicted, never another tenant's.",
)
.env("FORGEBASE_KV_TENANT_MAX_BYTES")];

const BACKUP: &[SettingDef] = &[SettingDef::new(
    "backup.source",
    "Cluster backup source",
    Enum,
    "Where a cluster backup copies each database set's data from: primary reads the set's primary; replica reads a healthy replica of the set, so the backup does not load the primary. A single-set backup always reads the primary. Use replica once each set has a replica.",
)
.options(&["primary", "replica"])
.env("FORGEBASE_BACKUP_SOURCE")];

const VECTOR: &[SettingDef] = &[SettingDef::new(
    "vector.enabled",
    "Vector search",
    Bool,
    "Turns vector search on or off: /api/v1/vector and loading its search indexes at start. Leave it on if you store embeddings for similarity search.",
)
.env("FORGEBASE_VECTOR_ENABLED")];

const GRAPH: &[SettingDef] = &[SettingDef::new(
    "graph.enabled",
    "Graph",
    Bool,
    "Turns the graph API (nodes and edges under /api/v1/graph) on or off. It runs nothing in the background, so leaving it on costs little.",
)
.env("FORGEBASE_GRAPH_ENABLED")];

const IDENTITY: &[SettingDef] = &[SettingDef::new(
    "identity.enabled",
    "Identity provider",
    Bool,
    "Turns the OpenID Connect (OIDC) provider and realm management on or off: sign-in for your other apps (/api/auth/oidc, clients, consents, signing keys) and /api/v1/realms. Users of existing realms keep signing in when off. Leave it on if other apps sign in through ForgeBase.",
)
.env("FORGEBASE_IDENTITY_ENABLED")];

const ACCESS: &[SettingDef] = &[SettingDef::new(
    "access.enabled",
    "Access control API",
    Bool,
    "Turns the API for policies, roles, access control lists (ACLs), organisations, teams, projects and suspensions on or off. The stored rules are still enforced when off. Leave it on while you manage access.",
)
.env("FORGEBASE_ACCESS_ENABLED")];

const RESP: &[SettingDef] = &[
    SettingDef::new(
        "resp.enabled",
        "Redis protocol listener",
        Bool,
        "Opens a second port that speaks the Redis serialization protocol (RESP), so redis-cli and Redis client libraries can use the key-value store. Off by default because it does not encrypt traffic; turn it on only on a trusted network.",
    )
    .env("FORGEBASE_RESP_ENABLED")
    .flag("--resp"),
    SettingDef::new(
        "resp.host",
        "Listen address",
        Text,
        "Network address the Redis protocol listener listens on. Keep 127.0.0.1 (the default) or reach it through a secure tunnel, because it does not encrypt traffic.",
    )
    .env("FORGEBASE_RESP_HOST")
    .flag("--resp-host"),
    SettingDef::new(
        "resp.port",
        "Port",
        Int,
        "TCP port of the Redis protocol listener, such as 6380 (the default; Redis itself uses 6379). 0 picks a free port.",
    )
    .env("FORGEBASE_RESP_PORT")
    .flag("--resp-port"),
    SettingDef::new(
        "resp.max_connections",
        "Most connections",
        Int,
        "Most clients connected at once, from 1 to 1000000; further ones are refused. 1024 suits most servers.",
    )
    .env("FORGEBASE_RESP_MAX_CONNECTIONS"),
    SettingDef::new(
        "resp.max_bulk_bytes",
        "Largest value",
        Int,
        "Largest single value or argument a client may send, from 1 to 536870912 bytes. The default 16777216 is 16 MiB.",
    )
    .unit("bytes")
    .env("FORGEBASE_RESP_MAX_BULK_BYTES"),
    SettingDef::new(
        "resp.idle_timeout_secs",
        "Idle timeout",
        Int,
        "Closes a connection that sends nothing for this long, up to 86400 seconds; 0 never closes it. Connections waiting on a subscription stay open. 300 (5 minutes) is a good value.",
    )
    .unit("seconds")
    .env("FORGEBASE_RESP_IDLE_TIMEOUT_SECS"),
];

const BILLING: &[SettingDef] = &[
    SettingDef::new(
        "billing.enabled",
        "Billing",
        Bool,
        "Turns billing on or off: plans, subscriptions, invoices and payment provider webhooks under /api/v1/billing (test mode only in this version). Turn it off if you do not charge your users.",
    )
    .env("FORGEBASE_BILLING_ENABLED")
    .flag("--billing"),
    SettingDef::new(
        "billing.webhook_log_retention_days",
        "Webhook log retention",
        Int,
        "How many days every incoming payment provider webhook (headers, body, whether its signature checked out and what it changed) is kept for inspection and replay. Bodies can hold customer names and emails. 30 is a good value.",
    )
    .unit("days")
    .env("FORGEBASE_BILLING_WEBHOOK_LOG_RETENTION_DAYS"),
];

/// `[mcp]`: the MCP server for AI agents (ADR-0030).
const MCP: &[SettingDef] = &[SettingDef::new(
    "mcp.enabled",
    "MCP server for AI agents",
    Bool,
    "Opens POST /api/mcp, a Model Context Protocol server through which AI agents (Claude Code, Cursor and others) can read and change everything their API key allows: data, files, functions, jobs, users, keys and settings. Agents sign in with an API key or personal access token; a scoped key also needs the mcp scope. Off by default: turn it on only if you want agents to manage this server.",
)
.env("FORGEBASE_MCP_ENABLED")
.flag("--mcp")];

/// `[observability]`: trace context and OpenTelemetry export (ADR-0035).
const OBSERVABILITY: &[SettingDef] = &[
    SettingDef::new(
        "observability.trust_incoming_trace",
        "Continue callers' traces",
        Enum,
        "Whether a W3C traceparent header sent by a caller continues the caller's trace. auto (the default) continues it with the caller's sampling choice when the request comes through a proxy you trust (auth.trust_forwarded_for) or from this machine, and otherwise keeps the trace id but lets this server decide whether to record it, so outside callers cannot make you export every request. always trusts every caller; never ignores the header.",
    )
    .options(&["auto", "always", "never"])
    .env("FORGEBASE_TRUST_INCOMING_TRACE"),
    SettingDef::new(
        "observability.otlp.enabled",
        "Export traces",
        Bool,
        "Sends traces (and, if you turn on log export, log lines) to an OpenTelemetry collector over OTLP/HTTP, such as Grafana Tempo, Jaeger, Honeycomb or an OpenTelemetry Collector. Off by default: turn it on to follow requests through functions, queues, jobs and webhooks. Metrics stay on /metrics for Prometheus.",
    )
    .env("FORGEBASE_OTLP_ENABLED"),
    SettingDef::new(
        "observability.otlp.endpoint",
        "Collector address",
        Url,
        "The collector's OTLP/HTTP address; /v1/traces and /v1/logs are added to it. 127.0.0.1:4318 is a collector on this machine, such as http://127.0.0.1:4318 or https://api.honeycomb.io.",
    )
    .env("FORGEBASE_OTLP_ENDPOINT"),
    SettingDef::new(
        "observability.otlp.protocol",
        "Export format",
        Enum,
        "http/protobuf (the default, understood by every collector) or http/json.",
    )
    .options(&["http/protobuf", "http/json"])
    .env("FORGEBASE_OTLP_PROTOCOL"),
    SettingDef::new(
        "observability.otlp.headers",
        "Collector headers",
        Map,
        "Extra headers sent to the collector, such as {\"x-honeycomb-team\" = \"env:HONEYCOMB_API_KEY\"}. A value env:NAME is read from the environment variable NAME when the server starts, so API keys stay out of this file.",
    ),
    SettingDef::secret(
        "observability.otlp.secret_headers",
        "Secret collector headers",
        Some("FORGEBASE_OTLP_HEADERS"),
        "More collector headers as name=value pairs separated by commas, such as authorization=Bearer abc. They are secrets, so set them with the environment variable FORGEBASE_OTLP_HEADERS (or in the .env file next to forgebase.toml), not here.",
    ),
    SettingDef::new(
        "observability.otlp.service_name",
        "Service name",
        Text,
        "The service.name your tracing tool shows for this server, such as forgebase or shop-backend.",
    )
    .env("FORGEBASE_OTLP_SERVICE_NAME"),
    SettingDef::new(
        "observability.otlp.service_instance_id",
        "Instance id",
        Text,
        "The service.instance.id attribute that tells servers of one service apart. Empty picks a random id each time the server starts; set one such as eu-1 to keep it stable.",
    )
    .env("FORGEBASE_OTLP_SERVICE_INSTANCE_ID")
    .example("eu-1"),
    SettingDef::new(
        "observability.otlp.resource_attributes",
        "Resource attributes",
        Map,
        "More attributes describing this server, such as {\"deployment.environment.name\" = \"production\"}.",
    ),
    SettingDef::new(
        "observability.otlp.sampling_ratio",
        "Sampling ratio",
        Text,
        "Share of new traces that are recorded, from 0 to 1: 1.0 records every request, 0.1 one in ten. A trace a caller already samples is always followed. Edit it in forgebase.toml.",
    )
    .env("FORGEBASE_OTLP_SAMPLING_RATIO")
    .read_only(),
    SettingDef::new(
        "observability.otlp.export_traces",
        "Export spans",
        Bool,
        "Sends spans: one per HTTP request, function run, queue publish and delivery, job and outbound call. Leave it on unless you only want log lines.",
    )
    .env("FORGEBASE_OTLP_EXPORT_TRACES"),
    SettingDef::new(
        "observability.otlp.export_logs",
        "Export log lines",
        Bool,
        "Also sends the server's log lines (at log.level) with their trace ids. Off by default: logs can hold names, emails and addresses, so turn it on only if your collector may keep them.",
    )
    .env("FORGEBASE_OTLP_EXPORT_LOGS"),
    SettingDef::new(
        "observability.otlp.timeout_secs",
        "Export timeout",
        Int,
        "Seconds one export request may take before it is retried, from 1 to 120. 10 is a good value.",
    )
    .unit("seconds")
    .env("FORGEBASE_OTLP_TIMEOUT_SECS"),
    SettingDef::new(
        "observability.otlp.ca_file",
        "Collector certificate authority file",
        Text,
        "A PEM file with the certificate authority of an https:// collector that uses a private certificate. Empty uses the public certificate authorities; otherwise give a path such as /etc/forgebase/collector-ca.pem.",
    )
    .env("FORGEBASE_OTLP_CA_FILE")
    .example("/etc/forgebase/collector-ca.pem"),
];

/// `[alerts]`: alerting rules, notifiers and SLOs (ADR-0035).
const ALERTS: &[SettingDef] = &[
    SettingDef::new(
        "alerts.enabled",
        "Alerts and service levels",
        Bool,
        "Turns the alerting engine on or off: built-in and custom rules over this server's metrics and health, notifications (webhook, Slack, email, PagerDuty, a queue topic), silences and service level objectives under /api/v1/alerts and /api/v1/slos. Leave it on: rules that have nothing to watch cost almost nothing.",
    )
    .env("FORGEBASE_ALERTS_ENABLED"),
    SettingDef::new(
        "alerts.evaluation_interval_secs",
        "Evaluation interval",
        Int,
        "Seconds between two evaluations of every rule, from 1 to 3600. 30 is a good value.",
    )
    .unit("seconds")
    .env("FORGEBASE_ALERTS_EVALUATION_INTERVAL_SECS"),
    SettingDef::new(
        "alerts.disabled_rules",
        "Disabled built-in rules",
        List,
        "Built-in rules that never run, by id, such as [\"queue_backlog_high\"]. GET /api/v1/alerts/rules lists them.",
    )
    .env("FORGEBASE_ALERTS_DISABLED_RULES"),
    SettingDef::new(
        "alerts.allow_http",
        "Allow http:// notifiers",
        Bool,
        "Lets webhook, Slack and PagerDuty notifiers use plain http:// addresses. Off: only https://. Turn it on only for receivers you reach over a network you trust; addresses on private networks are refused either way.",
    )
    .env("FORGEBASE_ALERTS_ALLOW_HTTP"),
    SettingDef::new(
        "alerts.history_retention_days",
        "Alert history",
        Int,
        "Days resolved alerts and notification deliveries are kept, from 1 to 3650. 30 is a good value.",
    )
    .unit("days")
    .env("FORGEBASE_ALERTS_HISTORY_RETENTION_DAYS"),
    SettingDef::new(
        "alerts.tls_cert_expiry_metric",
        "Certificate expiry metric",
        Text,
        "The gauge holding the Unix time a TLS (Transport Layer Security) certificate expires; the built-in certificate rules warn 14 days and 3 days before it, such as forgebase_tls_certificate_expiry_timestamp_seconds.",
    )
    .env("FORGEBASE_ALERTS_TLS_CERT_EXPIRY_METRIC"),
    SettingDef::new(
        "alerts.rules",
        "Rules declared in the file",
        List,
        "Custom alert rules declared as [[alerts.rules]] tables in forgebase.toml with a name, an expression, a threshold, for, a severity, labels and annotations, such as name = \"ManyJobFailures\" with expr = \"sum(rate(forgebase_jobs_completed_total{outcome='failed'}[15m]))\" and threshold = 0.1. Edit them in the file; add others from the API.",
    )
    .read_only(),
];

/// Every module, in display order.
pub const FEATURES: &[FeatureDef] = &[
    FeatureDef {
        id: "server",
        name: "Server",
        summary: "Where the server listens, how it logs and how it uses its database. Shared by every module.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/system"),
        docs: "docs/deployment.md#7-configuration-keys",
        routes: &[],
        settings: SERVER,
    },
    FeatureDef {
        id: "database",
        name: "Database",
        summary: "Collections (tables), records, schemas, indexes and SQL queries, on PostgreSQL (bundled or your own server) or SQLite.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/database"),
        docs: "docs/deployment.md#35-database-postgresql-or-sqlite",
        routes: &[],
        settings: DATABASE,
    },
    FeatureDef {
        id: "auth",
        name: "Authentication",
        summary: "Administrators, users, sign-in methods, sessions and API keys.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/auth"),
        docs: "docs/adr/0004-auth.md#adr-0004-authentication-core",
        routes: &[],
        settings: AUTH,
    },
    FeatureDef {
        id: "kv",
        name: "Key-value store",
        summary: "Fast Redis-style keys, hashes, lists and counters under /api/v1/kv. Server functions and the Redis protocol listener use it.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/redis"),
        docs: "docs/adr/0005-kv.md#adr-0005-key-value-engine-and-its-persistence",
        routes: &[],
        settings: KV,
    },
    FeatureDef {
        id: "storage",
        name: "Object storage",
        summary: "Files in buckets on local disk or Amazon S3, with uploads, signed download links and automatic cleanup.",
        can_disable: true,
        enable_key: Some("storage.enabled"),
        studio_path: Some("/storage"),
        docs: "docs/adr/0018-object-storage.md#adr-0018-object-storage",
        routes: &["/api/v1/storage/**"],
        settings: STORAGE,
    },
    FeatureDef {
        id: "functions",
        name: "Server functions",
        summary: "Your own code, compiled to WebAssembly, run on HTTP calls, record changes, schedules and queue messages.",
        can_disable: true,
        enable_key: Some("functions.enabled"),
        studio_path: Some("/functions"),
        docs: "docs/adr/0017-functions.md#adr-0017-server-functions-on-wasmtime",
        routes: &["/api/v1/functions/**"],
        settings: FUNCTIONS,
    },
    FeatureDef {
        id: "jobs",
        name: "Jobs",
        summary: "Background jobs, schedules and workflows that survive restarts. Every module's maintenance runs on them.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/jobs"),
        docs: "docs/adr/0007-jobs.md#adr-0007-durable-jobs",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "queues",
        name: "Queues and topics",
        summary: "Topics that keep messages, with pull and push delivery, retries, dead letters and replay.",
        can_disable: true,
        enable_key: Some("queue.enabled"),
        studio_path: Some("/queues"),
        docs: "docs/adr/0022-queues-pubsub.md#adr-0022-durable-topics-and-queues-publishsubscribe",
        routes: &["/api/v1/queues/**"],
        settings: QUEUES,
    },
    FeatureDef {
        id: "realtime",
        name: "Realtime",
        summary: "Live record changes, channels and presence over WebSocket and server-sent events (SSE).",
        can_disable: true,
        enable_key: Some("realtime.enabled"),
        studio_path: None,
        docs: "docs/adr/0011-realtime.md#adr-0011-realtime-engine",
        routes: &["/api/v1/realtime/**"],
        settings: REALTIME,
    },
    FeatureDef {
        id: "vector",
        name: "Vector search",
        summary: "Collections of embeddings with fast similarity search (approximate nearest neighbours).",
        can_disable: true,
        enable_key: Some("vector.enabled"),
        studio_path: Some("/vector"),
        docs: "docs/adr/0012-vector.md#adr-0012-vector-search",
        routes: &["/api/v1/vector/**"],
        settings: VECTOR,
    },
    FeatureDef {
        id: "graph",
        name: "Graph",
        summary: "Nodes and edges between records, with traversals and shortest paths.",
        can_disable: true,
        enable_key: Some("graph.enabled"),
        studio_path: Some("/graph"),
        docs: "docs/adr/0006-graph.md#adr-0006-graph-model",
        routes: &["/api/v1/graph/**"],
        settings: GRAPH,
    },
    FeatureDef {
        id: "identity",
        name: "Identity provider",
        summary: "An OpenID Connect (OIDC) provider so your other apps can sign users in through ForgeBase, and realms (separate user pools).",
        can_disable: true,
        enable_key: Some("identity.enabled"),
        studio_path: Some("/identity"),
        docs: "docs/adr/0019-identity-provider.md#adr-0019-forgebase-as-identity-provider-oauth-20-authorization-server-openid-connect",
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
        summary: "Policies, roles, access control lists (ACLs), organisations, teams, projects, invitations and suspensions.",
        can_disable: true,
        enable_key: Some("access.enabled"),
        studio_path: Some("/access"),
        docs: "docs/adr/0008-policy.md#adr-0008-authorization-tenancy-and-the-evaluation-pipeline",
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
        summary: "A Redis serialization protocol (RESP) listener so redis-cli and Redis client libraries can use the key-value store.",
        can_disable: true,
        enable_key: Some("resp.enabled"),
        studio_path: Some("/redis"),
        docs: "docs/adr/0013-resp.md#adr-0013-resp-listener-and-the-redisvalkey-adapter",
        routes: &[],
        settings: RESP,
    },
    FeatureDef {
        id: "mcp",
        name: "MCP server",
        summary: "Lets AI agents operate and configure this server through the Model Context Protocol, with an API key's permissions.",
        can_disable: true,
        enable_key: Some("mcp.enabled"),
        studio_path: None,
        docs: "docs/mcp.md#mcp-server",
        routes: &["/api/mcp"],
        settings: MCP,
    },
    FeatureDef {
        id: "observability",
        name: "Tracing",
        summary: "Trace ids in logs and responses, and OpenTelemetry export of traces and logs to your collector.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/observability.md#1-tracing",
        routes: &[],
        settings: OBSERVABILITY,
    },
    FeatureDef {
        id: "alerts",
        name: "Alerts and service levels",
        summary: "Alert rules over metrics and health signals, notifications, silences and service level objectives with error budgets.",
        can_disable: true,
        enable_key: Some("alerts.enabled"),
        studio_path: None,
        docs: "docs/observability.md#2-alerts",
        routes: &["/api/v1/alerts/**", "/api/v1/slos/**"],
        settings: ALERTS,
    },
    FeatureDef {
        id: "billing",
        name: "Billing",
        summary: "Plans, prices, subscriptions and invoices, with Stripe, Razorpay and app store webhooks (test mode only).",
        can_disable: true,
        enable_key: Some("billing.enabled"),
        studio_path: Some("/billing"),
        docs: "docs/adr/0021-billing.md#adr-0021-billing",
        routes: &["/api/v1/billing/**"],
        settings: BILLING,
    },
    FeatureDef {
        id: "entitlements",
        name: "Plans and quotas",
        summary: "Features, plans, add-ons, usage limits and rate limits that depend on a user's plan.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/adr/0010-entitlements.md#adr-0010-entitlements-and-quotas",
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
        docs: "docs/adr/0016-credits.md#adr-0016-credits",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "customize",
        name: "Customization",
        summary: "Custom domains, branding, email templates and the terms your users accept.",
        can_disable: false,
        enable_key: None,
        studio_path: None,
        docs: "docs/adr/0020-product-customization.md#adr-0020-product-customization",
        routes: &[],
        settings: &[],
    },
    FeatureDef {
        id: "backups",
        name: "Backups",
        summary: "Snapshots of the data directory, taken while the server runs, under /api/v1/system/backups.",
        can_disable: false,
        enable_key: None,
        studio_path: Some("/system"),
        docs: "docs/adr/0024-backup-restore.md#adr-0024-backup-restore-snapshots-exportimport-and-doctor",
        routes: &[],
        settings: BACKUP,
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
    FEATURES
        .iter()
        .filter(|f| f.can_disable && !f.enabled(config))
        .collect()
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
        invalid.insert(
            "set".to_owned(),
            "give at least one setting to change, such as {\"log.level\": \"debug\"}".to_owned(),
        );
    }
    for (key, value) in set {
        let Some(def) = setting(key) else {
            invalid.insert(
                key.clone(),
                "is not a setting: use a key listed by GET /api/system/features, such as log.level".to_owned(),
            );
            continue;
        };
        if def.secret {
            invalid.insert(key.clone(), secret_reason(def));
            continue;
        }
        if !def.editable {
            invalid.insert(key.clone(), read_only_reason(def));
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
                    invalid.insert(key.clone(), parse_reason(def, &e));
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
    let restart_required = changes
        .iter()
        .any(|(k, _)| setting(k).is_some_and(|s| s.restart_required));
    Ok(PatchOutcome {
        written: path,
        keys,
        restart_required,
    })
}

fn secret_reason(def: &SettingDef) -> String {
    match def.env {
        Some(var) => format!("is a secret and cannot be saved here: set the environment variable {var} and restart"),
        None => "is a secret kept in <data dir>/auth/master.key and cannot be changed here".to_owned(),
    }
}

fn read_only_reason(def: &SettingDef) -> String {
    match (def.flag, def.env) {
        (Some(flag), Some(env)) => {
            format!("cannot be changed here: start the server with {flag} or the environment variable {env} instead")
        }
        (Some(flag), None) => format!("cannot be changed here: start the server with {flag} instead"),
        (None, Some(env)) => format!("cannot be changed here: set the environment variable {env} instead"),
        (None, None) => "cannot be changed here".to_owned(),
    }
}

fn overridden_reason(def: &SettingDef, what: &str, name: Option<&str>) -> String {
    let name = name.unwrap_or(def.key);
    format!(
        "is set by {what} ({name}), which wins over the config file: change or remove {name} where the server is started, then restart"
    )
}

/// The default of `def` as an example value ("30", "info"), else its
/// example, if it has one.
fn example(def: &SettingDef) -> Option<String> {
    if let Some(e) = def.example {
        return Some(e.to_owned());
    }
    let v = default_values().remove(def.key)?;
    match v {
        Json::String(s) if !s.is_empty() => Some(s),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn such_as(def: &SettingDef) -> String {
    example(def).map(|e| format!(", such as {e}")).unwrap_or_default()
}

/// "seconds" for a unit, as words.
fn unit_words(def: &SettingDef) -> &'static str {
    match def.unit {
        Some("bytes") => " of bytes",
        Some("seconds") => " of seconds",
        Some("ms") => " of milliseconds",
        Some("days") => " of days",
        _ => "",
    }
}

/// `a, b or c`.
fn one_of(options: &[&str]) -> String {
    match options.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn parse_reason(def: &SettingDef, e: &ConfigError) -> String {
    match e {
        ConfigError::Parse { message, .. } => {
            // toml's message starts with the location; the last line is the reason.
            let reason = message
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(message)
                .trim();
            let max = [("u8", "255"), ("u16", "65535"), ("u32", "4294967295")]
                .into_iter()
                .find(|(ty, _)| reason.ends_with(&format!("expected {ty}")))
                .map(|(_, max)| max);
            match max {
                Some(max) => format!("must be a whole number from 0 to {max}{}", such_as(def)),
                None => format!("is not a valid value ({reason}); enter a value{}", such_as(def)),
            }
        }
        other => other.to_string(),
    }
}

/// Checks `value` against `def` and converts it to a TOML value. Each
/// refusal says what to enter instead.
fn to_toml(def: &SettingDef, value: &Json) -> Result<toml_edit::Value, String> {
    match def.ty {
        SettingType::Bool => value
            .as_bool()
            .map(toml_edit::Value::from)
            .ok_or_else(|| "must be true or false".to_owned()),
        SettingType::Int => value
            .as_i64()
            .filter(|n| *n >= 0)
            .map(toml_edit::Value::from)
            .ok_or_else(|| format!("must be a whole number{}{}", unit_words(def), such_as(def))),
        SettingType::String => value
            .as_str()
            .map(toml_edit::Value::from)
            .ok_or_else(|| format!("must be text{}", such_as(def))),
        SettingType::Enum => match value.as_str() {
            Some(s) if def.options.contains(&s) => Ok(toml_edit::Value::from(s)),
            _ => Err(format!("must be one of {}", one_of(def.options))),
        },
        SettingType::Url => match value.as_str() {
            Some(s) if s.is_empty() || s.starts_with("https://") || s.starts_with("http://") => {
                Ok(toml_edit::Value::from(s))
            }
            _ => Err(format!(
                "must be an address that starts with https:// or http://{}",
                such_as(def)
            )),
        },
        SettingType::Duration => match value.as_str() {
            Some(s) if is_duration(s) => Ok(toml_edit::Value::from(s)),
            _ => Err("must be a duration such as 30s, 500ms, 5m or 1h".to_owned()),
        },
        SettingType::Map => {
            let wrong = || "must be a table of text values, such as {\"shared_buffers\": \"256MB\"}".to_owned();
            let items = value.as_object().ok_or_else(wrong)?;
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in items {
                table.insert(k, v.as_str().ok_or_else(wrong)?.into());
            }
            Ok(toml_edit::Value::InlineTable(table))
        }
        SettingType::List => {
            let wrong = || "must be a list of text values, such as [\"a\", \"b\"]".to_owned();
            let items = value.as_array().ok_or_else(wrong)?;
            let mut arr = toml_edit::Array::new();
            for i in items {
                arr.push(i.as_str().ok_or_else(wrong)?);
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
        let (section, name) = match key.rsplit_once('.') {
            Some((s, n)) => (Some(s), n),
            None => (None, *key),
        };
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        // Nested sections (`database.postgres.extensions`) are walked one
        // level at a time; a missing one is appended as a `[a.b]` table.
        let mut path = String::new();
        for part in section.into_iter().flat_map(|s| s.split('.')) {
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(part);
            if !table.contains_key(part) {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                table.insert(part, toml_edit::Item::Table(t));
            }
            table = table
                .get_mut(part)
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| format!("[{path}] is not a table in the config file"))?;
        }
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
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
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
        fn walk(prefix: &str, t: &dyn toml_edit::TableLike, out: &mut Vec<String>) {
            for (k, item) in t.iter() {
                let key = if prefix.is_empty() {
                    k.to_owned()
                } else {
                    format!("{prefix}.{k}")
                };
                // A setting whose value is a table (`database.postgres.settings`).
                if setting(&key).is_some_and(|s| s.ty == SettingType::Map) {
                    out.push(key);
                    continue;
                }
                match item.as_table_like() {
                    Some(t) => walk(&key, t, out),
                    None => out.push(key),
                }
            }
        }
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        let mut out = Vec::new();
        walk("", doc.as_table(), &mut out);
        out
    }

    #[test]
    fn registry_lists_every_module_and_every_example_setting() {
        let ids: Vec<&str> = FEATURES.iter().map(|f| f.id).collect();
        for id in [
            "server",
            "database",
            "auth",
            "storage",
            "functions",
            "jobs",
            "queues",
            "realtime",
            "vector",
            "graph",
            "identity",
            "access",
            "resp",
            "mcp",
            "billing",
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
            assert!(
                setting(k).is_some(),
                "{k} from forgebase.example.toml is in the registry"
            );
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
        // Defaults: on, except the Redis listener and the MCP server (entry
        // points for other programs stay opt-in).
        for f in FEATURES.iter().filter(|f| f.can_disable) {
            assert_eq!(f.enabled(&cfg), !matches!(f.id, "resp" | "mcp"), "{}", f.id);
        }
    }

    #[test]
    fn route_patterns_match_segments() {
        assert!(route_matches("/api/v1/storage/**", "/api/v1/storage"));
        assert!(route_matches("/api/v1/storage/**", "/api/v1/storage/buckets/a/objects"));
        assert!(route_matches("/api/v1/storage/**", "//api/v1//storage/"));
        assert!(!route_matches("/api/v1/storage/**", "/api/v1/storagex"));
        assert!(!route_matches("/api/v1/storage/**", "/api/v1/stats"));
        assert!(route_matches(
            "/api/auth/realms/*/keys/**",
            "/api/auth/realms/{realm}/keys"
        ));
        assert!(!route_matches("/api/auth/realms/*/keys/**", "/api/auth/realms/default"));
        assert!(!route_matches(
            "/api/auth/realms/*/keys/**",
            "/api/auth/realms/default/oauth-providers"
        ));
        let cfg = {
            let mut c = Config::defaults(Path::new("/x"));
            c.identity.enabled = false;
            c.storage.enabled = false;
            c
        };
        let off = disabled(&cfg);
        assert_eq!(
            off.iter().map(|f| f.id).collect::<Vec<_>>(),
            ["storage", "identity", "resp", "mcp"]
        );
        assert_eq!(owner_of(&off, "/api/auth/oidc/default/token").unwrap().id, "identity");
        assert_eq!(owner_of(&off, "/api/v1/storage/buckets").unwrap().id, "storage");
        assert!(
            owner_of(&off, "/api/auth/realms/default").is_none(),
            "realm sign-in settings stay with auth"
        );
        assert!(owner_of(&off, "/api/auth/login").is_none());
        assert_eq!(
            feature("storage").unwrap().disabled_message(),
            "Object storage is disabled on this server"
        );
        assert_eq!(
            feature("billing").unwrap().disabled_message(),
            "Billing is disabled on this server"
        );
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
        for s in FEATURES
            .iter()
            .flat_map(|f| f.settings.iter())
            .filter(|s| s.editable && !s.secret)
        {
            let v = match setting_value(&Config::defaults(tmp.path()), s.key).unwrap() {
                Json::Null => Json::from(s.example.expect("an unset default has an example")),
                v => v,
            };
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
                ("auth.trust_forwarded_for", Json::from(true)),
            ]),
        )
        .unwrap();
        assert_eq!(out.written, file);
        assert_eq!(
            out.keys,
            [
                "auth.trust_forwarded_for",
                "log.format",
                "storage.enabled",
                "storage.max_object_bytes"
            ]
        );
        assert!(out.restart_required);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("# forgebase.example.toml: every configuration key"),
            "header kept"
        );
        assert!(text.contains("# \"pretty\" (human-readable) or \"json\" (one object per line).\n"));
        assert!(text.contains("format = \"json\""));
        assert!(text.contains("max_object_bytes = 1024"));
        assert!(
            text.contains("# s3_endpoint = \"https://s3.eu-central-1.amazonaws.com\""),
            "commented keys kept"
        );
        assert!(
            text.contains("[auth]\ntrust_forwarded_for = true"),
            "new section appended"
        );
        // Every other line of the example is still there, in order.
        let replaced = ["format = \"pretty\"", "max_object_bytes = 5368709120", "enabled = true"];
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
        assert!(!loaded.config.storage.enabled && loaded.config.auth.trust_forwarded_for);
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
        assert!(
            text.contains("read_pool_size = 4          # FORGEBASE_DB_READ_POOL_SIZE"),
            "{text}"
        );
    }

    /// Name, summary, label and help appear as-is in the setup wizard and in
    /// Features & settings (docs/requirements/dashboard-ux.md): sentence
    /// case, jargon spelled out, a sensible value in every help line.
    #[test]
    fn copy_is_plain_and_gives_sensible_values() {
        const PROPER: &[&str] = &[
            "API",
            "HTTP",
            "IP",
            "S3",
            "TCP",
            "URL",
            "X-Forwarded-For",
            "Redis",
            "OpenID",
            "Connect",
            "PostgreSQL",
            "SQLite",
            "Apache",
            "AGE",
            "AI",
        ];
        const JARGON: &[(&str, &str)] = &[
            ("OIDC", "OpenID Connect"),
            ("RESP", "Redis serialization protocol"),
            ("SSE", "server-sent events"),
            ("ACL", "access control list"),
            ("TTL", "time to live"),
            ("RLS", "row-level security"),
            ("WAL", "write-ahead log"),
            ("TLS", "Transport Layer Security"),
            ("ANN", "nearest neighbour"),
            ("JWT", "JSON Web Token"),
        ];
        let sentence_case = |what: &str, text: &str| {
            let mut words = text.split(' ');
            let first = words.next().unwrap_or_default();
            assert!(
                first.starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit()),
                "{what}: {text:?}"
            );
            for w in words {
                assert!(
                    !w.starts_with(char::is_uppercase) || PROPER.contains(&w),
                    "{what}: {text:?} is sentence case ({w})"
                );
            }
        };
        let plain = |what: &str, text: &str| {
            assert!(!text.trim().is_empty(), "{what} is empty");
            for (abbr, long) in JARGON {
                let used = text
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w.trim_end_matches('s') == *abbr);
                if used {
                    assert!(text.contains(long), "{what}: {text:?} spells out {abbr} ({long})");
                }
            }
        };
        let sentence = |what: &str, text: &str| {
            plain(what, text);
            assert!(text.starts_with(char::is_uppercase), "{what}: {text:?}");
            assert!(text.ends_with('.'), "{what}: {text:?} ends with a full stop");
        };
        let defaults = default_values();
        for f in FEATURES {
            sentence_case(f.id, f.name);
            plain(f.id, f.name);
            sentence(f.id, f.summary);
            for s in f.settings {
                sentence_case(s.key, s.label);
                plain(s.key, s.label);
                sentence(s.key, s.help);
                match defaults.get(s.key) {
                    Some(Json::Number(n)) => {
                        assert!(s.help.contains(&n.to_string()), "{} help gives the default {n}", s.key)
                    }
                    Some(Json::String(v)) if !v.is_empty() => {
                        assert!(s.help.contains(v.as_str()), "{} help gives the default {v}", s.key)
                    }
                    Some(Json::Bool(_)) => {
                        let h = s.help.to_lowercase();
                        assert!(
                            ["leave it", "turn it", "leaving it"].iter().any(|p| h.contains(p)),
                            "{} help says whether to turn it on",
                            s.key
                        );
                    }
                    _ => assert!(
                        s.secret || s.help.contains("such as"),
                        "{} help gives an example",
                        s.key
                    ),
                }
            }
        }
    }

    /// GitHub's heading anchor: lower case, spaces to `-`, punctuation dropped.
    fn anchor_of(heading: &str) -> String {
        heading
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_feature_links_to_an_existing_docs_anchor() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for f in FEATURES {
            let (file, anchor) = f
                .docs
                .split_once('#')
                .unwrap_or_else(|| panic!("{} docs has an anchor", f.id));
            let text = std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("{}: {file}: {e}", f.id));
            let anchors: Vec<String> = text
                .lines()
                .filter_map(|l| {
                    l.trim_start_matches('#')
                        .strip_prefix(' ')
                        .filter(|_| l.starts_with('#'))
                })
                .map(anchor_of)
                .collect();
            assert!(
                anchors.iter().any(|a| a == anchor),
                "{}: #{anchor} is a heading of {file}",
                f.id
            );
        }
    }

    /// Every validation message says what to enter instead.
    #[test]
    fn validation_messages_say_what_to_enter() {
        for s in FEATURES.iter().flat_map(|f| f.settings.iter()).filter(|s| !s.secret) {
            let wrong = if s.ty == SettingType::String {
                Json::from(1)
            } else {
                Json::Null
            };
            let msg = to_toml(s, &wrong).unwrap_err();
            assert!(msg.starts_with("must be "), "{}: {msg}", s.key);
            let says_what = ["such as", "one of", "true or false"].iter().any(|p| msg.contains(p))
                || (s.ty == SettingType::String && msg == "must be text");
            assert!(says_what, "{}: {msg}", s.key);
        }
        let mut c = Config::defaults(Path::new("/srv/fb"));
        c.server.host = " ".into();
        c.server.request_timeout_secs = 0;
        c.server.shutdown_timeout_secs = 4000;
        c.database.read_pool_size = 0;
        c.database.busy_timeout_ms = 700_000;
        c.log.level = String::new();
        c.resp.host = String::new();
        c.resp.max_connections = 0;
        c.resp.max_bulk_bytes = 0;
        c.resp.idle_timeout_secs = 90_000;
        c.storage.driver = crate::config::StorageDriverKind::S3;
        c.storage.max_object_bytes = 0;
        c.storage.s3_part_size = 0;
        c.realtime.max_connections_per_principal = 0;
        c.queue.max_message_bytes = 0;
        c.queue.default_retention_secs = 0;
        c.queue.dedup_window_secs = 0;
        c.queue.max_pull_wait_ms = 400_000;
        c.queue.poll_interval_ms = 0;
        c.queue.push_timeout_secs = 0;
        c.queue.push_concurrency = 0;
        let problems = c.problems();
        assert_eq!(problems.len(), 22, "{problems:?}");
        for p in problems {
            let ConfigError::Invalid { field, message } = p else {
                panic!("{p}")
            };
            assert!(setting(field).is_some(), "{field} is a registry key");
            assert!(message.starts_with("must be "), "{field}: {message}");
            assert!(message.contains("such as"), "{field}: {message}");
        }
        let mut c = Config::defaults(Path::new("/srv/fb"));
        c.queue.max_retention_secs = 60;
        let [ConfigError::Invalid { field, message }] = &c.problems()[..] else {
            panic!("one problem")
        };
        assert_eq!(*field, "queue.max_retention_secs");
        assert!(message.contains("such as 2678400"), "{message}");
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
        assert!(d["nope.key"].starts_with("is not a setting"), "{}", d["nope.key"]);
        assert_eq!(d["log.format"], "must be one of pretty or json");
        assert_eq!(d["storage.enabled"], "must be true or false");
        assert_eq!(
            d["server.request_timeout_secs"],
            "must be a whole number of seconds, such as 30"
        );
        assert_eq!(d["resp.port"], "must be a whole number from 0 to 65535, such as 6380");
        assert!(d["data_dir"].contains("--data-dir"), "{}", d["data_dir"]);
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
        assert_eq!(
            d["server.request_timeout_secs"],
            "must be a whole number from 1 to 3600 seconds, such as 30"
        );
        assert_eq!(
            d["database.read_pool_size"],
            "must be a whole number from 1 to 64, such as 8"
        );
        assert!(d["storage.s3_endpoint"].starts_with("must be set when the driver is s3"));
        assert!(d["storage.s3_bucket"].starts_with("must be set when the driver is s3"));
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
        std::fs::write(
            tmp.path().join("forgebase.toml"),
            "[functions]\nenabled = false\n[queue]\nenabled = false\n",
        )
        .unwrap();
        let file_only = load(tmp.path(), &[], ConfigOverrides::default());
        assert!(!file_only.config.functions.enabled && !file_only.config.queue.enabled);
        let env_on = load(
            tmp.path(),
            &[
                ("FORGEBASE_FUNCTIONS_ENABLED", "true"),
                ("FORGEBASE_QUEUE_ENABLED", "1"),
            ],
            ConfigOverrides::default(),
        );
        assert!(
            env_on.config.functions.enabled && env_on.config.queue.enabled,
            "env beats file"
        );
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

    /// FB-148: nested `[database.postgres.*]` keys are written into their
    /// own tables, read-only and secret database keys are refused.
    #[test]
    fn patch_writes_nested_database_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("forgebase.toml");
        std::fs::write(&file, crate::config::CONFIG_TEMPLATE).unwrap();
        let (origin, data_dir) = origin(tmp.path());
        apply_patch(
            &origin,
            &data_dir,
            &set(&[
                ("database.postgres.pool_size", Json::from(4)),
                ("database.postgres.extensions.vector", Json::from("off")),
                ("database.postgres.native_rls", Json::from("enforce")),
            ]),
        )
        .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("[database.postgres]\n") && text.contains("\npool_size = 4\n"),
            "{text}"
        );
        assert!(
            text.contains("[database.postgres.extensions]\nvector = \"off\""),
            "{text}"
        );
        let loaded = ConfigLoader::new(tmp.path()).load().unwrap();
        let pg = &loaded.config.database.postgres;
        assert_eq!(pg.pool_size, 4);
        assert_eq!(pg.native_rls, crate::config::NativeRls::Enforce);
        assert_eq!(pg.extensions.vector, crate::config::ExtensionMode::Off);
        assert_eq!(loaded.sources.get("database.postgres.extensions.vector"), Source::File);
        // A second change keeps the tables it made.
        apply_patch(
            &origin,
            &data_dir,
            &set(&[("database.postgres.extensions.cron", Json::from("on"))]),
        )
        .unwrap();
        let again = ConfigLoader::new(tmp.path()).load().unwrap().config.database.postgres;
        assert_eq!(again.extensions.vector, crate::config::ExtensionMode::Off);
        assert_eq!(again.extensions.cron, crate::config::ExtensionMode::On);
        for (key, value) in [
            ("database.backend", Json::from("sqlite")),
            ("database.postgres.mode", Json::from("external")),
            ("database.postgres.engine_dir", Json::from("/tmp/x")),
            ("database.postgres.listen", Json::from("127.0.0.1:5433")),
            ("database.postgres.url", Json::from("postgres://u:p@h/db")),
            ("database.postgres.settings", serde_json::json!({"work_mem": "8MB"})),
        ] {
            let err = apply_patch(&origin, &data_dir, &set(&[(key, value)])).unwrap_err();
            let PatchError::Invalid(m) = err else {
                panic!("{key}: {err}")
            };
            assert!(m.contains_key(key), "{key}: {m:?}");
        }
        let err = apply_patch(
            &origin,
            &data_dir,
            &set(&[("database.postgres.pool_size", Json::from(65))]),
        )
        .unwrap_err();
        let PatchError::Invalid(m) = err else { panic!("{err}") };
        assert!(m["database.postgres.pool_size"].starts_with("must be "), "{m:?}");
    }

    /// FB-148: the environment layer and the file's settings table.
    #[test]
    fn postgres_settings_come_from_file_and_environment() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("forgebase.toml"),
            "[database]\nbackend = \"postgres\"\n[database.postgres]\nurl = \"postgres://u:not-in-debug-7f3a@db/x\"\nengine_dir = \"engine\"\n[database.postgres.settings]\nshared_buffers = \"256MB\"\nmax_connections = 100\njit = false\n",
        )
        .unwrap();
        let loaded = load(
            tmp.path(),
            &[
                ("FORGEBASE_DATABASE_POSTGRES_MODE", "external"),
                ("FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_GRAPH", "off"),
                ("FORGEBASE_TEST_BACKEND", "postgres"),
                ("FORGEBASE_PG_ENGINE_DIR", "/opt/pg"),
            ],
            ConfigOverrides::default(),
        );
        let db = &loaded.config.database;
        assert_eq!(db.backend, crate::config::DatabaseBackend::Postgres);
        assert_eq!(db.postgres.mode, crate::config::PostgresMode::External);
        assert_eq!(db.postgres.extensions.graph, crate::config::ExtensionMode::Off);
        assert_eq!(
            db.postgres.url.as_ref().unwrap().expose(),
            "postgres://u:not-in-debug-7f3a@db/x"
        );
        assert!(
            !format!("{db:?}").contains("not-in-debug-7f3a"),
            "the URL is redacted in Debug"
        );
        assert_eq!(db.postgres.engine_dir, tmp.path().join("engine").display().to_string());
        let st: Vec<(&str, &str)> = db
            .postgres
            .settings
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            st,
            [("jit", "off"), ("max_connections", "100"), ("shared_buffers", "256MB")]
        );
        assert_eq!(setting_value(&loaded.config, "database.postgres.url"), None, "a secret");
        assert_eq!(
            source_of(setting("database.postgres.url").unwrap(), &loaded.sources),
            Source::File
        );
        assert_eq!(loaded.sources.get("database.postgres.mode"), Source::Env);
        assert_eq!(
            crate::config::redact_url("postgres://u:pw@db/x?host=/s"),
            "postgres://u:***@db/x?host=/s"
        );
        let mut c = loaded.config.clone();
        // FB-153: any address with a port is fine; port 0 is not.
        c.database.postgres.listen = "0.0.0.0:0".into();
        c.database.postgres.settings.insert("bad name".into(), "x".into());
        let fields: Vec<_> = c
            .problems()
            .into_iter()
            .filter_map(|e| match e {
                ConfigError::Invalid { field, .. } => Some(field),
                _ => None,
            })
            .collect();
        assert_eq!(fields, ["database.postgres.listen", "database.postgres.settings"]);
    }
}
