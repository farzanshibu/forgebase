//! Wire and storage types of the functions engine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::error::{FunctionError, Result};

/// Capabilities a function version may be granted. A module may import only
/// the host functions of granted capabilities (checked at deploy), and every
/// host call checks its capability again at run time. The service
/// capabilities (`vector.*`, `storage.*`, `queues.publish`, `jobs.enqueue`,
/// `realtime.publish`) and `service_role` exist for JavaScript and TypeScript
/// functions (ADR-0027); `service_role` can be granted only by an
/// administrator.
pub const CAPABILITIES: &[&str] = &[
    "db.read",
    "db.write",
    "kv",
    "log",
    "env",
    "secrets",
    "fetch",
    "vector.read",
    "vector.write",
    "storage.read",
    "storage.write",
    "queues.publish",
    "jobs.enqueue",
    "realtime.publish",
    "service_role",
];

/// The capability that lets a JavaScript function use `ctx.admin.*` (rules bypassed).
pub const SERVICE_ROLE: &str = "service_role";

/// Language a version was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// A WebAssembly module.
    #[default]
    Wasm,
    /// JavaScript (ES2023 module).
    Javascript,
    /// TypeScript, type-stripped to JavaScript at deploy.
    Typescript,
}

/// How a version runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FunctionRuntime {
    /// The module itself runs in the sandbox.
    #[default]
    Wasm,
    /// The embedded JavaScript engine runs the version's JavaScript in the sandbox.
    Js,
}

/// Names that collide with routes under `/api/v1/functions/`.
pub const RESERVED_NAMES: &[&str] = &["types"];

/// Longest function name.
pub const MAX_NAME_LEN: usize = 48;

/// Validates a function name: 1-48 characters, a lower-case letter first,
/// then lower-case letters, digits, `_` or `-`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok && RESERVED_NAMES.contains(&name) {
        return Err(FunctionError::invalid(
            "invalid_function_name",
            format!("{name:?} is reserved (GET /api/v1/functions/{name} is another route)"),
        ));
    }
    if ok {
        Ok(())
    } else {
        Err(FunctionError::invalid(
            "invalid_function_name",
            "function names are 1-48 characters: a lower-case letter, then lower-case letters, digits, _ or -",
        ))
    }
}

/// Who may invoke a function over HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvokeAccess {
    /// Anyone, including anonymous callers.
    Public,
    /// Any authenticated caller.
    Authenticated,
    /// Callers the authorizer allows `invoke_function` (`engine:invoke_function`).
    #[default]
    Permission,
}

impl InvokeAccess {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            InvokeAccess::Public => "public",
            InvokeAccess::Authenticated => "authenticated",
            InvokeAccess::Permission => "permission",
        }
    }

    /// Parses the stored form.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "public" => InvokeAccess::Public,
            "authenticated" => InvokeAccess::Authenticated,
            "permission" => InvokeAccess::Permission,
            _ => return None,
        })
    }
}

/// Resource limits of one execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Limits {
    /// Fuel (roughly: WebAssembly instructions, plus a charge per host call).
    pub fuel: u64,
    /// Largest linear memory in bytes.
    pub memory_bytes: u64,
    /// Wall-clock limit in milliseconds (including host calls such as fetch).
    pub timeout_ms: u64,
    /// Executions of this function at the same time; more get `429 function_busy`.
    pub max_concurrency: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 100_000_000,
            memory_bytes: 16 * 1024 * 1024,
            timeout_ms: 5_000,
            max_concurrency: 8,
        }
    }
}

impl Limits {
    /// The largest limits an administrator may configure.
    pub fn ceiling() -> Self {
        Self {
            fuel: 20_000_000_000,
            memory_bytes: 512 * 1024 * 1024,
            timeout_ms: 60_000,
            max_concurrency: 256,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let max = Self::ceiling();
        let bad = |what: &str, max: u64| {
            FunctionError::invalid("invalid_limits", format!("{what} must be between 1 and {max}"))
        };
        if self.fuel == 0 || self.fuel > max.fuel {
            return Err(bad("limits.fuel", max.fuel));
        }
        if self.memory_bytes < 65_536 || self.memory_bytes > max.memory_bytes {
            return Err(FunctionError::invalid(
                "invalid_limits",
                format!("limits.memory_bytes must be between 65536 and {}", max.memory_bytes),
            ));
        }
        if self.timeout_ms == 0 || self.timeout_ms > max.timeout_ms {
            return Err(bad("limits.timeout_ms", max.timeout_ms));
        }
        if self.max_concurrency == 0 || self.max_concurrency > max.max_concurrency {
            return Err(bad("limits.max_concurrency", u64::from(max.max_concurrency)));
        }
        Ok(())
    }
}

/// Limits in a deploy request: every field optional (defaults apply).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitsInput {
    /// Fuel per execution.
    pub fuel: Option<u64>,
    /// Memory limit in bytes.
    pub memory_bytes: Option<u64>,
    /// Wall-clock limit in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Concurrent executions.
    pub max_concurrency: Option<u32>,
}

impl LimitsInput {
    pub(crate) fn resolve(self, defaults: Limits) -> Limits {
        Limits {
            fuel: self.fuel.unwrap_or(defaults.fuel),
            memory_bytes: self.memory_bytes.unwrap_or(defaults.memory_bytes),
            timeout_ms: self.timeout_ms.unwrap_or(defaults.timeout_ms),
            max_concurrency: self.max_concurrency.unwrap_or(defaults.max_concurrency),
        }
    }
}

/// A record event a hook function runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Before a record is created; may reject it.
    BeforeCreate,
    /// After a create committed.
    AfterCreate,
    /// Before a record is updated; may reject it.
    BeforeUpdate,
    /// After an update committed.
    AfterUpdate,
    /// Before a record is deleted; may reject it.
    BeforeDelete,
    /// After a delete committed.
    AfterDelete,
}

impl HookEvent {
    /// Wire name (`before_create`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::BeforeCreate => "before_create",
            HookEvent::AfterCreate => "after_create",
            HookEvent::BeforeUpdate => "before_update",
            HookEvent::AfterUpdate => "after_update",
            HookEvent::BeforeDelete => "before_delete",
            HookEvent::AfterDelete => "after_delete",
        }
    }

    /// Runs before the write (can reject it).
    pub fn is_before(self) -> bool {
        matches!(
            self,
            HookEvent::BeforeCreate | HookEvent::BeforeUpdate | HookEvent::BeforeDelete
        )
    }

    /// The event for a change kind.
    pub fn of(op: forgebase_data::ChangeOp, before: bool) -> Self {
        use forgebase_data::ChangeOp;
        match (op, before) {
            (ChangeOp::Create, true) => HookEvent::BeforeCreate,
            (ChangeOp::Create, false) => HookEvent::AfterCreate,
            (ChangeOp::Update, true) => HookEvent::BeforeUpdate,
            (ChangeOp::Update, false) => HookEvent::AfterUpdate,
            (ChangeOp::Delete, true) => HookEvent::BeforeDelete,
            (ChangeOp::Delete, false) => HookEvent::AfterDelete,
        }
    }
}

/// Record events of one collection that run a function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    /// Collection name.
    pub collection: String,
    /// Events.
    pub events: Vec<HookEvent>,
}

/// Everything about one version except the module itself. Stored as JSON in
/// `_forgebase_function_versions.config`; a rollback restores all of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VersionConfig {
    /// Granted capabilities (see [`CAPABILITIES`]).
    pub capabilities: Vec<String>,
    /// With `fetch`: hosts the function may call (`api.example.com`, or
    /// `*.example.com` for subdomains). Empty = any public host.
    #[serde(default)]
    pub fetch_hosts: Vec<String>,
    /// Limits.
    pub limits: Limits,
    /// Plain (non-secret) environment values, readable with the `env` capability.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Record hooks.
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// How the version runs (`wasm` for versions deployed before FB-93).
    #[serde(default)]
    pub runtime: FunctionRuntime,
    /// The language it was written in.
    #[serde(default)]
    pub language: Language,
}

impl VersionConfig {
    /// Whether capability `cap` is granted.
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

/// Body of `POST /api/v1/functions`: deploys a new version (creating the
/// function on its first deploy) and, unless `activate` is `false`, makes it
/// the active version. Send either `wasm` or `source` (JavaScript or
/// TypeScript, with `language`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    /// Function name.
    pub name: String,
    /// The WebAssembly module (binary format), base64.
    #[serde(default)]
    pub wasm: Option<String>,
    /// JavaScript or TypeScript source of a single-file ES module that calls
    /// `serve()` from `"forgebase"` (at most 1 MiB).
    #[serde(default)]
    pub source: Option<String>,
    /// Language of `source` (default `typescript`).
    #[serde(default)]
    pub language: Option<Language>,
    /// Description (kept from the previous deploy when omitted).
    #[serde(default)]
    pub description: Option<String>,
    /// HTTP access mode (kept from the previous deploy when omitted; `permission` on create).
    #[serde(default)]
    pub access: Option<InvokeAccess>,
    /// Granted capabilities.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Allowed fetch hosts.
    #[serde(default)]
    pub fetch_hosts: Vec<String>,
    /// Limits (defaults for missing fields).
    #[serde(default)]
    pub limits: LimitsInput,
    /// Plain environment values.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Record hooks.
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// Make the new version active (default `true`).
    #[serde(default = "yes")]
    pub activate: bool,
}

fn yes() -> bool {
    true
}

impl Default for DeployRequest {
    /// An empty request (no code) that activates the new version.
    fn default() -> Self {
        Self {
            name: String::new(),
            wasm: None,
            source: None,
            language: None,
            description: None,
            access: None,
            capabilities: Vec::new(),
            fetch_hosts: Vec::new(),
            limits: LimitsInput::default(),
            env: BTreeMap::new(),
            hooks: Vec::new(),
            activate: true,
        }
    }
}

/// One version of a function. `runtime` and `language` are part of the
/// flattened configuration.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct VersionInfo {
    /// Version number (1, 2, ...).
    pub version: i64,
    /// SHA-256 of the module (WebAssembly) or of the source (JavaScript, TypeScript), hex.
    pub sha256: String,
    /// Size of the module or of the source in bytes.
    pub size: i64,
    /// Configuration.
    #[serde(flatten)]
    pub config: VersionConfig,
    /// Whether this is the active version.
    pub active: bool,
    /// When it was deployed (RFC 3339).
    pub created_at: String,
    /// Who deployed it.
    pub created_by: String,
}

/// A function.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct FunctionInfo {
    /// Id.
    pub id: String,
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// HTTP access mode.
    pub access: InvokeAccess,
    /// The version that runs.
    pub active_version: i64,
    /// The newest version.
    pub latest_version: i64,
    /// Identity the function acts as (`service:function.<name>`): grant it
    /// permissions with the policy API.
    pub principal: String,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Last deploy or rollback (RFC 3339).
    pub updated_at: String,
    /// Who created it.
    pub created_by: String,
}

/// A deploy result.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Deployed {
    /// The function after the deploy.
    pub function: FunctionInfo,
    /// The new version.
    pub version: VersionInfo,
}

/// A secret's metadata (values are never returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SecretInfo {
    /// Name.
    pub name: String,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// A line the function logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogLine {
    /// `debug`, `info`, `warn` or `error`.
    pub level: String,
    /// Text.
    pub message: String,
}

/// One recorded execution.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ExecutionLog {
    /// Id (increasing).
    pub id: i64,
    /// Function name.
    pub function: String,
    /// Version that ran.
    pub version: i64,
    /// `http`, `hook`, `job` or `test`.
    pub trigger: String,
    /// `ok`, `rejected`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout` or `bad_output`.
    pub status: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Fuel consumed.
    pub fuel_used: i64,
    /// Error message, if any.
    pub error: Option<String>,
    /// Lines the function logged (capped).
    pub logs: Vec<LogLine>,
    /// Request id of the triggering request.
    pub request_id: Option<String>,
    /// When it started (RFC 3339).
    pub created_at: String,
}

/// One recorded execution with its profile (ADR-0036 §3).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct InvocationDetail {
    /// The execution log row.
    #[serde(flatten)]
    pub log: ExecutionLog,
    /// What the host-call boundary measured: cold start, fuel and memory
    /// against their limits, per-operation counts and times, and a timeline
    /// of the first host calls. `null` for executions recorded before
    /// profiling existed or while it was off.
    #[schema(value_type = Option<Object>)]
    pub profile: Option<serde_json::Value>,
}

/// What started an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `/api/v1/functions/{name}/invoke`.
    Http,
    /// A record hook.
    Hook,
    /// A background job (`function.invoke`).
    Job,
    /// A direct call through the service (tests, tools).
    Direct,
    /// An administrator's test run of a specific version (`invoke?version=`).
    Test,
}

impl Trigger {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Http => "http",
            Trigger::Hook => "hook",
            Trigger::Job => "job",
            Trigger::Direct => "test",
            Trigger::Test => "test",
        }
    }
}

/// The result of one successful execution.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Bytes the function passed to `output` (`None`: it never called it).
    pub output: Option<Vec<u8>>,
    /// Version that ran.
    pub version: i64,
    /// Fuel consumed.
    pub fuel_used: u64,
    /// Lines logged.
    pub logs: Vec<LogLine>,
}

impl Outcome {
    /// The output parsed as JSON (`None` without output).
    pub fn json(&self) -> Result<Option<Json>> {
        match &self.output {
            None => Ok(None),
            Some(b) if b.is_empty() => Ok(None),
            Some(b) => serde_json::from_slice(b)
                .map(Some)
                .map_err(|_| crate::error::ExecError::BadOutput("the output is not JSON".into()).into()),
        }
    }
}

/// What a key-value namespace rule allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KvAccess {
    /// Read commands only (`GET`, `HGETALL`, `SCAN`, ...).
    Read,
    /// Reads and writes.
    Write,
}

/// Lets a JavaScript function reach an application key-value namespace
/// (beyond its private `fn.<name>`). Deny by default; administrators manage
/// the rules; the caller's own key-value permissions still apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KvRule {
    /// Namespace name, or a prefix ending in `*` (`app.*`, `*` for every namespace).
    pub namespace: String,
    /// What is allowed.
    pub access: KvAccess,
}

/// The source of a JavaScript or TypeScript version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct VersionSource {
    /// `javascript` or `typescript`.
    pub language: Language,
    /// The source as deployed.
    pub source: String,
}
