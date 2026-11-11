//! ForgeBase server functions (FB-60, SPEC §13, ADR-0017).
//!
//! Functions are WebAssembly modules run in Wasmtime with no ambient
//! authority: no WASI, no filesystem, no clock, no network, no processes.
//! A module gets only the `forgebase` host functions of the capabilities its
//! version was granted ([`model::CAPABILITIES`]): database reads and writes
//! (as the function's own identity, through the authorizer), key-value data
//! in its own namespace, logging, plain environment values, sealed secrets,
//! and SSRF-safe outbound HTTP ([`fetch`]).
//!
//! * **Limits** per execution: fuel, memory and wall clock ([`runtime`]);
//!   concurrency per function and per engine.
//! * **Versions**: every deploy adds an immutable version (module plus
//!   configuration); one is active; a rollback re-activates an older one.
//! * **Triggers**: HTTP (`/api/v1/functions/{name}/invoke`, [`http`]),
//!   record hooks before (can reject) and after (from the realtime outbox)
//!   create/update/delete ([`hooks`]), background jobs and schedules
//!   (`function.invoke`, [`jobs`]).
//! * **Execution logs** and metrics for every run.

pub mod compile;
pub mod error;
pub mod fetch;
pub mod hooks;
pub mod host;
pub mod http;
pub mod jobs;
pub mod js;
mod jsops;
pub mod model;
pub mod runtime;
pub mod services;
mod store;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use forgebase_auth::crypto::Keyring;
use forgebase_data::{AccessContext, Action, DataService, Principal};
use forgebase_db::{Backend, Statement, TxMode, Value};
use forgebase_kv::KvService;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

pub use error::{ExecError, FunctionError, Result};
pub use fetch::{FetchConfig, Fetcher};
pub use http::{extension, openapi, router};
pub use jobs::{INVOKE_JOB, register_jobs};
pub use model::{
    CAPABILITIES, DeployRequest, Deployed, ExecutionLog, FunctionInfo, FunctionRuntime, HookEvent, HookSpec,
    InvokeAccess, KvAccess, KvRule, Language, Limits, LimitsInput, LogLine, Outcome, SERVICE_ROLE, SecretInfo, Trigger,
    VersionConfig, VersionInfo, VersionSource,
};
pub use services::{QueueMessage, QueuePublisher, ServiceError, Services};

use crate::host::HostEnv;
use crate::runtime::Runtime;

/// The `forgebase` module declarations (`sdk/functions/forgebase.d.ts`),
/// served at `GET /api/v1/functions/types`.
pub const FORGEBASE_DTS: &str = include_str!("../../../sdk/functions/forgebase.d.ts");

/// Nested executions allowed (a function whose database write runs a
/// before-hook function, ...).
pub const MAX_DEPTH: u32 = 4;

/// Name of the after-hook cursor over the realtime outbox.
const AFTER_CURSOR: &str = "after_hooks";

tokio::task_local! {
    /// Nesting depth of the execution running on this task.
    static DEPTH: u32;
}

/// The nesting depth of the current task (0 outside executions).
pub(crate) fn current_depth() -> u32 {
    DEPTH.try_with(|d| *d).unwrap_or(0)
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct FunctionsConfig {
    /// Largest module.
    pub max_module_bytes: usize,
    /// Executions at the same time across all functions.
    pub max_concurrent: usize,
    /// Limits for fields a deploy leaves out.
    pub default_limits: Limits,
    /// How often the after-hook dispatcher polls the outbox.
    pub after_hook_interval: Duration,
    /// Execution-log rows kept per function.
    pub log_retention: i64,
    /// Outbound requests (`None`: the `fetch` capability fails with `fetch_unavailable`).
    pub fetcher: Option<Fetcher>,
}

impl Default for FunctionsConfig {
    fn default() -> Self {
        Self {
            max_module_bytes: 16 * 1024 * 1024,
            max_concurrent: 64,
            default_limits: Limits::default(),
            after_hook_interval: Duration::from_millis(250),
            log_retention: 1000,
            fetcher: None,
        }
    }
}

/// A loaded function with its active configuration.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) row: store::FunctionRow,
    pub(crate) config: Arc<VersionConfig>,
    pub(crate) sem: Arc<Semaphore>,
}

impl Entry {
    fn info(&self) -> FunctionInfo {
        let r = &self.row;
        FunctionInfo {
            id: r.id.clone(),
            name: r.name.clone(),
            description: r.description.clone(),
            access: r.access,
            active_version: r.active_version,
            latest_version: r.latest_version,
            principal: identity(&r.name).principal_key(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            created_by: r.created_by.clone(),
        }
    }
}

/// Functions by name and the hook index.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    pub(crate) by_name: BTreeMap<String, Entry>,
    /// `(collection, event)` → function names, sorted.
    pub(crate) hooks: HashMap<(String, HookEvent), Vec<String>>,
}

impl Registry {
    fn build(rows: Vec<store::FunctionRow>, old: &Registry) -> Self {
        let mut by_name = BTreeMap::new();
        let mut hooks: HashMap<(String, HookEvent), Vec<String>> = HashMap::new();
        for row in rows {
            let config = Arc::new(row.config.clone());
            let permits = config.limits.max_concurrency as usize;
            let sem = old
                .by_name
                .get(&row.name)
                .filter(|e| e.row.id == row.id && e.config.limits.max_concurrency == config.limits.max_concurrency)
                .map_or_else(|| Arc::new(Semaphore::new(permits)), |e| e.sem.clone());
            for h in &config.hooks {
                for ev in &h.events {
                    hooks
                        .entry((h.collection.clone(), *ev))
                        .or_default()
                        .push(row.name.clone());
                }
            }
            by_name.insert(row.name.clone(), Entry { row, config, sem });
        }
        for names in hooks.values_mut() {
            names.sort();
            names.dedup();
        }
        Self { by_name, hooks }
    }
}

/// The identity a function acts as: a non-admin service principal.
pub fn identity(function: &str) -> AccessContext {
    AccessContext::service(format!("function.{function}"), false)
}

/// The privileged principal of `ctx.admin.*` (versions granted `service_role`),
/// in the caller's realm, organization and tenant.
pub fn admin_identity(function: &str, caller: &AccessContext) -> AccessContext {
    let mut ctx = AccessContext::service(format!("function.{function}"), true).with_realm(caller.realm_id.clone());
    if let Some(o) = &caller.org_id {
        ctx = ctx.with_org(o.clone());
    }
    if let Some(t) = &caller.tenant {
        ctx = ctx.with_tenant(t.clone());
    }
    if let Some(r) = &caller.request_id {
        ctx = ctx.with_request_id(r.clone());
    }
    ctx
}

/// A version to run: the active one or a specific one (`invoke?version=`).
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) version: i64,
    pub(crate) config: Arc<VersionConfig>,
}

/// The JavaScript of a JS/TS version and its source map.
#[derive(Debug)]
pub(crate) struct Script {
    pub(crate) js: Vec<u8>,
    pub(crate) map: Option<compile::SourceMap>,
}

/// Code of a deploy.
enum Code {
    Wasm(Vec<u8>),
    Js { language: Language, source: String },
}

/// The server-functions engine. Cheap to clone.
#[derive(Debug, Clone)]
pub struct FunctionService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
pub(crate) struct Inner {
    data: DataService,
    kv: Option<KvService>,
    keyring: Keyring,
    runtime: Runtime,
    js: js::JsRuntime,
    cfg: FunctionsConfig,
    registry: RwLock<Arc<Registry>>,
    modules: Mutex<HashMap<(String, i64), wasmtime::Module>>,
    /// JavaScript of `(function id, version)`.
    scripts: Mutex<HashMap<(String, i64), Arc<Script>>>,
    /// Key-value namespace rules by function id.
    kv_rules: RwLock<HashMap<String, Arc<Vec<KvRule>>>>,
    services: RwLock<Arc<Services>>,
    global: Arc<Semaphore>,
    write_lock: tokio::sync::Mutex<()>,
    /// Held for a whole [`FunctionService::run_after_hooks`] pass: the
    /// cursor is read, advanced and stored under it, so a manual call racing
    /// the background task cannot deliver an event twice.
    after_lock: tokio::sync::Mutex<()>,
    log_counter: AtomicU64,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in self.tasks.get_mut().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }
}

fn now() -> String {
    forgebase_core::now_rfc3339()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn secret_aad(function_id: &str, name: &str) -> Vec<u8> {
    format!("forgebase/functions/secret/v1\0{function_id}\0{name}").into_bytes()
}

fn validate_key(kind: &str, key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(FunctionError::invalid(
            "invalid_key",
            format!("{kind} names are 1-64 characters of [A-Za-z0-9_], not starting with a digit"),
        ))
    }
}

fn validate_fetch_host(h: &str) -> Result<()> {
    let bare = h.strip_prefix("*.").unwrap_or(h);
    let ok = !bare.is_empty()
        && bare.len() <= 253
        && bare
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    if ok {
        Ok(())
    } else {
        Err(FunctionError::invalid(
            "invalid_fetch_host",
            format!("{h:?} is not a host name (use api.example.com or *.example.com)"),
        ))
    }
}

impl FunctionService {
    /// Opens the engine on `data` (database migrated to 0016): loads the
    /// functions and installs the before-hook guard on `data` (so `data` must
    /// be the process's one data service). `keyring` seals secrets (derive it
    /// from the data directory's master key: `Keyring::load_or_create`).
    pub async fn open(
        data: DataService,
        kv: Option<KvService>,
        keyring: Keyring,
        cfg: FunctionsConfig,
    ) -> Result<Self> {
        let runtime = Runtime::new()?;
        let js = js::JsRuntime::new()?;
        let rows = store::load_all(data.backend().as_ref()).await?;
        let registry = Registry::build(rows, &Registry::default());
        let kv_rules = store::kv_rules(data.backend().as_ref()).await?;
        let svc = Self {
            inner: Arc::new(Inner {
                global: Arc::new(Semaphore::new(cfg.max_concurrent.max(1))),
                data,
                kv,
                keyring,
                runtime,
                js,
                cfg,
                registry: RwLock::new(Arc::new(registry)),
                modules: Mutex::new(HashMap::new()),
                scripts: Mutex::new(HashMap::new()),
                kv_rules: RwLock::new(kv_rules),
                services: RwLock::new(Arc::new(Services::default())),
                after_lock: tokio::sync::Mutex::new(()),
                write_lock: tokio::sync::Mutex::new(()),
                log_counter: AtomicU64::new(0),
                tasks: Mutex::new(Vec::new()),
            }),
        };
        let guard = hooks::HookGuard::new(&svc);
        if !svc.inner.data.set_write_guard(Arc::new(guard)) {
            return Err(FunctionError::Internal(
                "a write guard is already installed on this data service".into(),
            ));
        }
        // After-hooks see changes from now on, not the outbox's history. The
        // cursor is per set (FB-171: `_forgebase_function_cursors` is a
        // per-set table, next to the set's outbox); a set opened later has
        // no cursor yet and is read from its start.
        for (set, backend) in svc.outbox_sets() {
            if store::cursor(backend.as_ref(), AFTER_CURSOR).await?.is_none() {
                let (_, head) = forgebase_realtime::outbox::bounds(backend.as_ref())
                    .await
                    .map_err(|e| FunctionError::Internal(format!("set {set}: {e}")))?;
                store::set_cursor(backend.as_ref(), AFTER_CURSOR, head, &now()).await?;
            }
        }
        Ok(svc)
    }

    /// Starts the after-hook dispatcher, and compiles and initialises the
    /// JavaScript engine in the background when JavaScript functions exist
    /// (so their first request does not wait for it).
    pub fn start_background(&self) {
        if self
            .registry()
            .by_name
            .values()
            .any(|e| e.config.runtime == FunctionRuntime::Js)
        {
            let weak = Arc::downgrade(&self.inner);
            let warm = tokio::spawn(async move {
                let Some(inner) = weak.upgrade() else { return };
                if let Err(e) = inner.js.snapshot().await {
                    tracing::warn!(error = %e, "JavaScript engine warm-up failed; the first JavaScript call retries");
                }
            });
            self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).push(warm);
        }
        let weak = Arc::downgrade(&self.inner);
        let every = self.inner.cfg.after_hook_interval;
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let svc = FunctionService { inner };
                if let Err(e) = svc.run_after_hooks().await {
                    tracing::warn!(error = %e, "after-hook dispatch failed; retrying");
                }
            }
        });
        self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).push(task);
    }

    /// Stops the background tasks.
    pub fn shutdown(&self) {
        for t in self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }

    /// The data service.
    pub fn data(&self) -> &DataService {
        &self.inner.data
    }

    /// Attaches the engines JavaScript functions reach through `ctx`
    /// (replacing earlier ones).
    pub fn attach_services(&self, services: Services) {
        *self.inner.services.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(services);
    }

    /// The engines attached with [`FunctionService::attach_services`].
    pub fn attached_services(&self) -> Arc<Services> {
        self.services()
    }

    pub(crate) fn services(&self) -> Arc<Services> {
        self.inner.services.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn backend(&self) -> Arc<dyn Backend> {
        self.inner.data.backend().clone()
    }

    /// Every set's outbox (FB-171): the data service's router sets, control
    /// first (control as the service's own backend), or the one backend.
    fn outbox_sets(&self) -> Vec<(String, Arc<dyn Backend>)> {
        let data = &self.inner.data;
        match data.shard_router() {
            Some(r) => r
                .sets()
                .into_iter()
                .map(|h| {
                    let b: Arc<dyn Backend> = if h.is_control() {
                        data.backend().clone()
                    } else {
                        Arc::new(h.clone())
                    };
                    (h.id().to_owned(), b)
                })
                .collect(),
            None => vec![(forgebase_db::topology::CONTROL_SET_ID.to_owned(), self.backend())],
        }
    }

    pub(crate) fn registry(&self) -> Arc<Registry> {
        self.inner.registry.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Re-reads functions from the database.
    pub async fn reload(&self) -> Result<()> {
        let rows = store::load_all(self.backend().as_ref()).await?;
        let mut guard = self.inner.registry.write().unwrap_or_else(|p| p.into_inner());
        let next = Registry::build(rows, &guard);
        *guard = Arc::new(next);
        Ok(())
    }

    fn authorize(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        Ok(self.inner.data.authorize_action(ctx, action)?)
    }

    fn entry(&self, name: &str) -> Result<Entry> {
        self.registry()
            .by_name
            .get(name)
            .cloned()
            .ok_or_else(|| FunctionError::NotFound(name.to_owned()))
    }

    /// Deploys a new version (see [`DeployRequest`]).
    pub async fn deploy(&self, ctx: &AccessContext, req: DeployRequest) -> Result<Deployed> {
        self.authorize(ctx, Action::ManageFunctions)?;
        model::validate_name(&req.name)?;
        let code = match (&req.wasm, &req.source) {
            (Some(_), Some(_)) => {
                return Err(FunctionError::invalid(
                    "invalid_deploy",
                    "send either wasm or source, not both",
                ));
            }
            (None, None) => {
                return Err(FunctionError::invalid(
                    "invalid_deploy",
                    "send wasm (a WebAssembly module, base64) or source (JavaScript or TypeScript)",
                ));
            }
            (Some(w), None) => {
                if req.language.is_some_and(|l| l != Language::Wasm) {
                    return Err(FunctionError::invalid(
                        "invalid_language",
                        "language javascript or typescript goes with source, not wasm",
                    ));
                }
                let wasm = base64::engine::general_purpose::STANDARD
                    .decode(w.trim())
                    .map_err(|_| FunctionError::invalid("invalid_wasm_encoding", "wasm must be standard base64"))?;
                if wasm.len() > self.inner.cfg.max_module_bytes {
                    return Err(FunctionError::invalid(
                        "module_too_large",
                        format!("modules are limited to {} bytes", self.inner.cfg.max_module_bytes),
                    ));
                }
                Code::Wasm(wasm)
            }
            (None, Some(src)) => {
                let language = req.language.unwrap_or(Language::Typescript);
                if language == Language::Wasm {
                    return Err(FunctionError::invalid(
                        "invalid_language",
                        "source must be javascript or typescript (send WebAssembly as wasm)",
                    ));
                }
                if src.len() > compile::MAX_SOURCE_BYTES {
                    return Err(FunctionError::invalid(
                        "source_too_large",
                        format!("sources are limited to {} bytes", compile::MAX_SOURCE_BYTES),
                    ));
                }
                Code::Js {
                    language,
                    source: src.clone(),
                }
            }
        };
        let mut caps = req.capabilities.clone();
        caps.sort();
        caps.dedup();
        if let Some(bad) = caps.iter().find(|c| !CAPABILITIES.contains(&c.as_str())) {
            return Err(FunctionError::invalid(
                "invalid_capability",
                format!("unknown capability {bad:?} (one of {})", CAPABILITIES.join(", ")),
            ));
        }
        if caps.iter().any(|c| c == SERVICE_ROLE) {
            if matches!(code, Code::Wasm(_)) {
                return Err(FunctionError::invalid(
                    "invalid_capability",
                    "service_role applies to JavaScript and TypeScript functions (ctx.admin)",
                ));
            }
            if !ctx.is_privileged() {
                return Err(FunctionError::Forbidden {
                    code: "service_role_requires_admin",
                    message: "only an administrator can grant the service_role capability".into(),
                });
            }
        }
        if !req.fetch_hosts.is_empty() && !caps.iter().any(|c| c == "fetch") {
            return Err(FunctionError::invalid(
                "invalid_fetch_host",
                "fetch_hosts needs the fetch capability",
            ));
        }
        if req.fetch_hosts.len() > 32 {
            return Err(FunctionError::invalid("invalid_fetch_host", "at most 32 fetch_hosts"));
        }
        req.fetch_hosts.iter().try_for_each(|h| validate_fetch_host(h))?;
        if req.env.len() > 64 {
            return Err(FunctionError::invalid("invalid_env", "at most 64 environment values"));
        }
        for (k, v) in &req.env {
            validate_key("environment", k)?;
            if v.len() > 4096 {
                return Err(FunctionError::invalid(
                    "invalid_env",
                    "environment values are limited to 4096 bytes",
                ));
            }
        }
        let mut hooks = Vec::with_capacity(req.hooks.len());
        for h in &req.hooks {
            if h.collection.is_empty() || h.collection.len() > 64 || h.events.is_empty() {
                return Err(FunctionError::invalid(
                    "invalid_hook",
                    "a hook names a collection and at least one event",
                ));
            }
            let mut events = h.events.clone();
            events.sort();
            events.dedup();
            hooks.push(HookSpec {
                collection: h.collection.clone(),
                events,
            });
        }
        let limits = req.limits.resolve(self.inner.cfg.default_limits);
        limits.validate()?;
        let (runtime, language) = match &code {
            Code::Wasm(_) => (FunctionRuntime::Wasm, Language::Wasm),
            Code::Js { language, .. } => (FunctionRuntime::Js, *language),
        };
        let config = VersionConfig {
            capabilities: caps,
            fetch_hosts: req.fetch_hosts.clone(),
            limits,
            env: req.env.clone(),
            hooks,
            runtime,
            language,
        };
        let grants_service_role = config.has(SERVICE_ROLE);
        // (module, stored wasm, source, javascript, source map)
        let (module, wasm, source, script, map) = match code {
            Code::Wasm(wasm) => (
                Some(self.inner.runtime.compile(wasm.clone(), &config).await?),
                wasm,
                None,
                None,
                None,
            ),
            Code::Js { language, source } => {
                if config.limits.memory_bytes < js::MIN_JS_MEMORY {
                    return Err(FunctionError::invalid(
                        "memory_limit_too_small",
                        format!(
                            "JavaScript functions need limits.memory_bytes of at least {}",
                            js::MIN_JS_MEMORY
                        ),
                    ));
                }
                let src = source.clone();
                let compiled = tokio::task::spawn_blocking(move || compile::compile(&src, language))
                    .await
                    .map_err(|e| FunctionError::Internal(format!("compile task: {e}")))?
                    .map_err(FunctionError::Compile)?;
                let unmet = compile::unmet(&compiled, |c| config.has(c));
                if !unmet.is_empty() {
                    return Err(FunctionError::Unmet(unmet));
                }
                // Fail the deploy, not the first call, if the engine cannot load.
                self.inner.js.module().await?;
                (None, Vec::new(), Some(source), Some(compiled.js), compiled.map)
            }
        };
        let map_json = map
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| FunctionError::Internal(format!("source map encode: {e}")))?;
        let digest_of = source.as_ref().map_or(wasm.as_slice(), |s| s.as_bytes());
        let sha = hex(&Sha256::digest(digest_of));
        let size = i64::try_from(digest_of.len()).unwrap_or(i64::MAX);
        let config_json =
            serde_json::to_string(&config).map_err(|e| FunctionError::Internal(format!("config encode: {e}")))?;

        let _lock = self.inner.write_lock.lock().await;
        let backend = self.backend();
        let mut tx = backend.begin(TxMode::Normal).await?;
        let at = now();
        let actor = ctx.actor();
        let res = async {
            let (id, version) = match store::find_in_tx(&mut *tx, &req.name).await? {
                Some((id, active, latest)) => {
                    let version = latest + 1;
                    let active = if req.activate { version } else { active };
                    tx.execute(&Statement::new(
                        "UPDATE _forgebase_functions SET latest_version = ?2, active_version = ?3,
                             description = coalesce(?4, description), access = coalesce(?5, access), updated_at = ?6
                         WHERE id = ?1",
                        vec![
                            id.clone().into(),
                            version.into(),
                            active.into(),
                            req.description.clone().into(),
                            req.access.map(InvokeAccess::as_str).into(),
                            at.clone().into(),
                        ],
                    ))
                    .await?;
                    (id, version)
                }
                None => {
                    let id = new_id();
                    tx.execute(&Statement::new(
                        "INSERT INTO _forgebase_functions
                             (id, name, description, access, active_version, latest_version, created_at, updated_at, created_by)
                         VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?5, ?6)",
                        vec![
                            id.clone().into(),
                            req.name.clone().into(),
                            req.description.clone().unwrap_or_default().into(),
                            req.access.unwrap_or_default().as_str().into(),
                            at.clone().into(),
                            actor.clone().into(),
                        ],
                    ))
                    .await?;
                    (id, 1)
                }
            };
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_function_versions
                     (function_id, version, wasm, sha256, config, created_at, created_by, source, js, source_map)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                vec![
                    id.clone().into(),
                    version.into(),
                    Value::Blob(wasm.clone()),
                    sha.clone().into(),
                    config_json.clone().into(),
                    at.clone().into(),
                    actor.clone().into(),
                    source.clone().into(),
                    script.clone().into(),
                    map_json.clone().into(),
                ],
            ))
            .await?;
            if grants_service_role {
                // Every deploy that grants service_role is in the audit log.
                let detail = serde_json::json!({
                    "function": req.name,
                    "version": version,
                    "sha256": sha,
                    "capabilities": config.capabilities,
                });
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, impersonator, realm)
                     VALUES (?1, 'functions.service_role_granted', 'success', ?2, ?3, ?4, ?5, ?6)",
                    vec![
                        unix_now().into(),
                        actor.clone().into(),
                        ctx.request_id.clone().into(),
                        detail.to_string().into(),
                        ctx.impersonator.clone().into(),
                        ctx.realm_id.clone().into(),
                    ],
                ))
                .await?;
            }
            Ok::<_, FunctionError>((id, version))
        }
        .await;
        let (id, version) = match res {
            Ok(v) => {
                tx.commit().await?;
                v
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if let Some(module) = module {
            self.inner
                .modules
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert((id.clone(), version), module);
        }
        if let Some(js) = script {
            self.inner.scripts.lock().unwrap_or_else(|p| p.into_inner()).insert(
                (id.clone(), version),
                Arc::new(Script {
                    js: js.into_bytes(),
                    map,
                }),
            );
        }
        self.reload().await?;
        let entry = self.entry(&req.name)?;
        tracing::info!(function = %req.name, version, sha256 = %sha, actor = %actor, runtime = ?runtime, "function deployed");
        if grants_service_role {
            tracing::warn!(function = %req.name, version, actor = %actor, "service_role granted to a function version");
        }
        metrics::counter!("forgebase_function_deploys_total").increment(1);
        Ok(Deployed {
            version: VersionInfo {
                version,
                sha256: sha,
                size,
                config,
                active: entry.row.active_version == version,
                created_at: at,
                created_by: actor,
            },
            function: entry.info(),
        })
    }

    /// Every function.
    pub fn list(&self, ctx: &AccessContext) -> Result<Vec<FunctionInfo>> {
        self.authorize(ctx, Action::ReadFunctions)?;
        Ok(self.registry().by_name.values().map(Entry::info).collect())
    }

    /// One function.
    pub fn get(&self, ctx: &AccessContext, name: &str) -> Result<FunctionInfo> {
        self.authorize(ctx, Action::ReadFunctions)?;
        Ok(self.entry(name)?.info())
    }

    /// A function's versions, newest first.
    pub async fn versions(&self, ctx: &AccessContext, name: &str) -> Result<Vec<VersionInfo>> {
        self.authorize(ctx, Action::ReadFunctions)?;
        let e = self.entry(name)?;
        store::versions(self.backend().as_ref(), &e.row.id, e.row.active_version).await
    }

    /// Activates `version`, or the newest version before the active one.
    pub async fn rollback(&self, ctx: &AccessContext, name: &str, version: Option<i64>) -> Result<FunctionInfo> {
        self.authorize(ctx, Action::ManageFunctions)?;
        let _lock = self.inner.write_lock.lock().await;
        let e = self.entry(name)?;
        let backend = self.backend();
        let target = match version {
            Some(v) => {
                if !store::version_exists(backend.as_ref(), &e.row.id, v).await? {
                    return Err(FunctionError::VersionNotFound {
                        name: name.to_owned(),
                        version: v,
                    });
                }
                v
            }
            None => store::previous_version(backend.as_ref(), &e.row.id, e.row.active_version)
                .await?
                .ok_or_else(|| {
                    FunctionError::invalid("no_previous_version", "there is no version before the active one")
                })?,
        };
        store::write(
            backend.as_ref(),
            vec![Statement::new(
                "UPDATE _forgebase_functions SET active_version = ?2, updated_at = ?3 WHERE id = ?1",
                vec![e.row.id.clone().into(), target.into(), now().into()],
            )],
        )
        .await?;
        self.reload().await?;
        tracing::info!(function = %name, from = e.row.active_version, to = target, actor = %ctx.actor(), "function rolled back");
        Ok(self.entry(name)?.info())
    }

    /// Deletes a function with its versions, secrets and logs.
    pub async fn delete(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.authorize(ctx, Action::ManageFunctions)?;
        let _lock = self.inner.write_lock.lock().await;
        let e = self.entry(name)?;
        let id = e.row.id.clone();
        store::write(
            self.backend().as_ref(),
            [
                "_forgebase_function_logs",
                "_forgebase_function_secrets",
                "_forgebase_function_kv_rules",
                "_forgebase_function_versions",
            ]
            .iter()
            .map(|t| {
                Statement::new(
                    format!("DELETE FROM {t} WHERE function_id = ?1"),
                    vec![id.clone().into()],
                )
            })
            .chain([Statement::new(
                "DELETE FROM _forgebase_functions WHERE id = ?1",
                vec![id.clone().into()],
            )])
            .collect(),
        )
        .await?;
        self.inner
            .modules
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(fid, _), _| *fid != id);
        self.inner
            .scripts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(fid, _), _| *fid != id);
        self.inner
            .kv_rules
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        self.reload().await
    }

    /// Execution logs, newest first.
    pub async fn logs(&self, ctx: &AccessContext, name: &str, limit: u32) -> Result<Vec<ExecutionLog>> {
        self.authorize(ctx, Action::ReadFunctions)?;
        let e = self.entry(name)?;
        store::logs(self.backend().as_ref(), &e.row.id, i64::from(limit.clamp(1, 500))).await
    }

    /// Sets (creates or replaces) a secret; the value is sealed at rest.
    pub async fn put_secret(&self, ctx: &AccessContext, name: &str, key: &str, value: &str) -> Result<SecretInfo> {
        self.authorize(ctx, Action::ManageFunctions)?;
        validate_key("secret", key)?;
        if value.len() > 16 * 1024 {
            return Err(FunctionError::invalid(
                "invalid_secret",
                "secret values are limited to 16 KiB",
            ));
        }
        let e = self.entry(name)?;
        let sealed = self
            .inner
            .keyring
            .seal(value.as_bytes(), &secret_aad(&e.row.id, key))
            .map_err(|e| FunctionError::Internal(e.to_string()))?;
        let at = now();
        store::write(
            self.backend().as_ref(),
            vec![Statement::new(
                "INSERT INTO _forgebase_function_secrets (function_id, name, sealed, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (function_id, name) DO UPDATE SET sealed = excluded.sealed, updated_at = excluded.updated_at",
                vec![e.row.id.clone().into(), key.into(), Value::Blob(sealed), at.clone().into()],
            )],
        )
        .await?;
        tracing::info!(function = %name, secret = %key, actor = %ctx.actor(), "function secret set");
        let infos = store::secret_infos(self.backend().as_ref(), &e.row.id).await?;
        infos
            .into_iter()
            .find(|s| s.name == key)
            .ok_or_else(|| FunctionError::Internal("secret vanished".into()))
    }

    /// Names and timestamps of a function's secrets.
    pub async fn secrets(&self, ctx: &AccessContext, name: &str) -> Result<Vec<SecretInfo>> {
        self.authorize(ctx, Action::ReadFunctions)?;
        let e = self.entry(name)?;
        store::secret_infos(self.backend().as_ref(), &e.row.id).await
    }

    /// Deletes a secret.
    pub async fn delete_secret(&self, ctx: &AccessContext, name: &str, key: &str) -> Result<()> {
        self.authorize(ctx, Action::ManageFunctions)?;
        let e = self.entry(name)?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let res = tx
            .execute(&Statement::new(
                "DELETE FROM _forgebase_function_secrets WHERE function_id = ?1 AND name = ?2",
                vec![e.row.id.clone().into(), key.into()],
            ))
            .await;
        match res {
            Ok(r) => {
                tx.commit().await?;
                if r.rows_affected == 0 {
                    return Err(FunctionError::SecretNotFound(key.to_owned()));
                }
                Ok(())
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err.into())
            }
        }
    }

    async fn module(&self, e: &Entry, t: &Target) -> Result<wasmtime::Module> {
        let key = (e.row.id.clone(), t.version);
        if let Some(m) = self.inner.modules.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return Ok(m.clone());
        }
        let wasm = store::wasm(self.backend().as_ref(), &e.row.id, t.version)
            .await?
            .ok_or_else(|| FunctionError::Internal(format!("module of {} is missing", e.row.name)))?;
        let module = self.inner.runtime.compile(wasm, &t.config).await?;
        let mut cache = self.inner.modules.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, module.clone());
        Ok(module)
    }

    /// The JavaScript of a JS/TS version.
    async fn script(&self, e: &Entry, t: &Target) -> Result<Arc<Script>> {
        let key = (e.row.id.clone(), t.version);
        if let Some(m) = self.inner.scripts.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return Ok(m.clone());
        }
        let (js, map) = store::script(self.backend().as_ref(), &e.row.id, t.version)
            .await?
            .ok_or_else(|| FunctionError::Internal(format!("JavaScript of {} is missing", e.row.name)))?;
        let js = Arc::new(Script {
            js: js.into_bytes(),
            map: map.and_then(|m| serde_json::from_str(&m).ok()),
        });
        let mut cache = self.inner.scripts.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, js.clone());
        Ok(js)
    }

    /// The active version of `e`.
    pub(crate) fn active(e: &Entry) -> Target {
        Target {
            version: e.row.active_version,
            config: e.config.clone(),
        }
    }

    /// Version `version` of `e` (its stored configuration).
    pub(crate) async fn target(&self, e: &Entry, version: i64) -> Result<Target> {
        if version == e.row.active_version {
            return Ok(Self::active(e));
        }
        let config = store::version_config(self.backend().as_ref(), &e.row.id, version)
            .await?
            .ok_or_else(|| FunctionError::VersionNotFound {
                name: e.row.name.clone(),
                version,
            })?;
        Ok(Target {
            version,
            config: Arc::new(config),
        })
    }

    /// The source of a JavaScript or TypeScript version (`404` for WebAssembly).
    pub async fn version_source(&self, ctx: &AccessContext, name: &str, version: i64) -> Result<VersionSource> {
        self.authorize(ctx, Action::ReadFunctions)?;
        let e = self.entry(name)?;
        let t = self.target(&e, version).await?;
        let source = match t.config.runtime {
            FunctionRuntime::Js => store::source(self.backend().as_ref(), &e.row.id, version).await?,
            FunctionRuntime::Wasm => None,
        };
        match source {
            Some(source) => Ok(VersionSource {
                language: t.config.language,
                source,
            }),
            None => Err(FunctionError::NoSource {
                name: name.to_owned(),
                version,
            }),
        }
    }

    /// The key-value namespace rules of a function.
    pub fn kv_rules(&self, ctx: &AccessContext, name: &str) -> Result<Vec<KvRule>> {
        self.authorize(ctx, Action::ReadFunctions)?;
        let e = self.entry(name)?;
        Ok(self.rules_of(&e.row.id).as_ref().clone())
    }

    fn rules_of(&self, function_id: &str) -> Arc<Vec<KvRule>> {
        self.inner
            .kv_rules
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(function_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Replaces the key-value namespace rules of a function (administrators only).
    pub async fn set_kv_rules(&self, ctx: &AccessContext, name: &str, rules: Vec<KvRule>) -> Result<Vec<KvRule>> {
        self.authorize(ctx, Action::ManageFunctions)?;
        if !ctx.is_privileged() {
            return Err(FunctionError::Forbidden {
                code: "kv_rules_require_admin",
                message: "only an administrator can change a function's key-value namespace rules".into(),
            });
        }
        if rules.len() > 64 {
            return Err(FunctionError::invalid("invalid_kv_rule", "at most 64 namespace rules"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for r in &rules {
            let bare = r.namespace.strip_suffix('*').unwrap_or(&r.namespace);
            let ok = r.namespace.len() <= 64
                && (r.namespace.ends_with('*') || !bare.is_empty())
                && bare.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-:".contains(&b));
            if !ok {
                return Err(FunctionError::invalid(
                    "invalid_kv_rule",
                    format!("{:?} is not a namespace or a prefix ending in *", r.namespace),
                ));
            }
            if !seen.insert(r.namespace.clone()) {
                return Err(FunctionError::invalid(
                    "invalid_kv_rule",
                    format!("namespace {:?} is listed twice", r.namespace),
                ));
            }
        }
        let _lock = self.inner.write_lock.lock().await;
        let e = self.entry(name)?;
        let at = now();
        let actor = ctx.actor();
        let mut stmts = vec![Statement::new(
            "DELETE FROM _forgebase_function_kv_rules WHERE function_id = ?1",
            vec![e.row.id.clone().into()],
        )];
        for r in &rules {
            stmts.push(Statement::new(
                "INSERT INTO _forgebase_function_kv_rules (function_id, namespace, access, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![
                    e.row.id.clone().into(),
                    r.namespace.clone().into(),
                    match r.access {
                        KvAccess::Read => "read",
                        KvAccess::Write => "write",
                    }
                    .into(),
                    at.clone().into(),
                    actor.clone().into(),
                ],
            ));
        }
        store::write(self.backend().as_ref(), stmts).await?;
        let all = store::kv_rules(self.backend().as_ref()).await?;
        *self.inner.kv_rules.write().unwrap_or_else(|p| p.into_inner()) = all;
        tracing::info!(function = %name, rules = rules.len(), actor = %actor, "function key-value rules set");
        Ok(self.rules_of(&e.row.id).as_ref().clone())
    }

    async fn open_secrets(&self, e: &Entry, config: &VersionConfig) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        if !config.has("secrets") {
            return Ok(out);
        }
        for (name, sealed) in store::sealed_secrets(self.backend().as_ref(), &e.row.id).await? {
            let plain = self
                .inner
                .keyring
                .open(&sealed, &secret_aad(&e.row.id, &name))
                .map_err(|e| FunctionError::Internal(e.to_string()))?;
            let value =
                String::from_utf8(plain).map_err(|_| FunctionError::Internal("a secret is not UTF-8".into()))?;
            out.insert(name, value);
        }
        Ok(out)
    }

    /// Runs the active version of `name` with `input` (JSON) as `trigger`,
    /// without access checks (internal callers: hooks, jobs, tests). HTTP
    /// callers go through [`FunctionService::invoke_http`].
    pub async fn invoke(
        &self,
        name: &str,
        trigger: Trigger,
        input: &Json,
        request_id: Option<String>,
    ) -> Result<Outcome> {
        let e = self.entry(name)?;
        self.execute(
            &e,
            None,
            trigger,
            input.to_string().into_bytes(),
            request_id,
            None,
            |_| None,
        )
        .await
    }

    /// Checks that `ctx` may invoke `name` over HTTP and returns the entry.
    pub(crate) fn http_entry(&self, ctx: &AccessContext, name: &str) -> Result<Entry> {
        let e = match self.entry(name) {
            Ok(e) => e,
            // Unknown and forbidden look alike to callers who may not invoke.
            Err(nf) => {
                if ctx.principal == Principal::Anonymous {
                    return Err(forgebase_data::DataError::Unauthenticated.into());
                }
                return Err(nf);
            }
        };
        match e.row.access {
            InvokeAccess::Public => {}
            InvokeAccess::Authenticated => {
                if ctx.principal == Principal::Anonymous {
                    return Err(forgebase_data::DataError::Unauthenticated.into());
                }
            }
            InvokeAccess::Permission => self.authorize(ctx, Action::InvokeFunction)?,
        }
        Ok(e)
    }

    /// Runs one execution and records it. `judge` may turn a successful run
    /// into a recorded rejection (`Some((status, message))`).
    /// `target` defaults to the active version; `caller` (whom JavaScript
    /// `ctx` services run as) defaults to the function's own identity.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute(
        &self,
        e: &Entry,
        target: Option<Target>,
        trigger: Trigger,
        input: Vec<u8>,
        request_id: Option<String>,
        caller: Option<AccessContext>,
        judge: impl FnOnce(Option<&[u8]>) -> Option<(&'static str, String)>,
    ) -> Result<Outcome> {
        let target = target.unwrap_or_else(|| Self::active(e));
        let depth = current_depth();
        if depth >= MAX_DEPTH {
            return Err(FunctionError::invalid(
                "function_depth_exceeded",
                format!("functions may nest at most {MAX_DEPTH} levels"),
            ));
        }
        let _global =
            self.inner.global.clone().try_acquire_owned().map_err(|_| {
                FunctionError::Busy("the functions engine is running as many executions as allowed".into())
            })?;
        let _local = e.sem.clone().try_acquire_owned().map_err(|_| {
            FunctionError::Busy(format!(
                "function {} is running {} executions, its limit",
                e.row.name, e.config.limits.max_concurrency
            ))
        })?;
        let config = target.config.clone();
        let secrets = self.open_secrets(e, &config).await?;
        let mut ident = identity(&e.row.name);
        if let Some(r) = &request_id {
            ident = ident.with_request_id(r.clone());
        }
        let caller = caller.unwrap_or_else(|| ident.clone());
        let services = self.services();
        let mut input = input;
        if config.runtime == FunctionRuntime::Js {
            // ctx.auth, ctx.function: the caller's identity and the version.
            if let Ok(Json::Object(mut o)) = serde_json::from_slice::<Json>(&input) {
                o.insert("auth".into(), jsops::auth_info(services.permissions(&caller), &caller));
                o.insert(
                    "function".into(),
                    serde_json::json!({"name": e.row.name, "version": target.version}),
                );
                input = Json::Object(o).to_string().into_bytes();
            }
        }
        let env = Arc::new(HostEnv {
            data: self.inner.data.clone(),
            kv: self.inner.kv.clone(),
            fetcher: self.inner.cfg.fetcher.clone(),
            identity: ident,
            function: e.row.name.clone(),
            config: config.clone(),
            secrets,
            admin: admin_identity(&e.row.name, &caller),
            caller,
            services,
            kv_rules: self.rules_of(&e.row.id),
        });
        let started_at = now();
        let start = Instant::now();
        let run = match config.runtime {
            FunctionRuntime::Wasm => {
                let module = self.module(e, &target).await?;
                DEPTH
                    .scope(depth + 1, self.inner.runtime.run(&module, &config.limits, env, input))
                    .await
            }
            FunctionRuntime::Js => {
                let script = self.script(e, &target).await?;
                let mut run = DEPTH
                    .scope(
                        depth + 1,
                        self.inner.js.run(&config.limits, env, script.js.clone(), input),
                    )
                    .await;
                // Positions in traces point at the user's source.
                let file = if config.language == Language::Typescript {
                    "index.ts"
                } else {
                    "index.js"
                };
                let fix = |t: &str| compile::remap_trace(t, script.map.as_ref(), file);
                for l in &mut run.logs {
                    if l.message.contains("index.js:") {
                        l.message = fix(&l.message);
                    }
                }
                if let Err(ExecError::Failed { status: 1, message }) = &run.result {
                    let trace = fix(message.as_deref().unwrap_or("an exception"));
                    if run.logs.len() < host::MAX_LOG_LINES {
                        run.logs.push(LogLine {
                            level: "error".into(),
                            message: format!("Uncaught {trace}"),
                        });
                    }
                    run.result = Err(ExecError::Uncaught(trace));
                }
                run
            }
        };
        let elapsed = start.elapsed();
        let (status, error) = match &run.result {
            Ok(out) => match judge(out.as_deref()) {
                Some((status, msg)) => (status, Some(msg)),
                None => ("ok", None),
            },
            Err(err) => (err.status(), Some(err.to_string())),
        };
        let n = self.inner.log_counter.fetch_add(1, Ordering::Relaxed);
        let keep = (n % 64 == 63).then_some(self.inner.cfg.log_retention);
        let log = store::NewLog {
            function_id: &e.row.id,
            function: &e.row.name,
            version: target.version,
            trigger: trigger.as_str(),
            status,
            duration_ms: i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            fuel_used: i64::try_from(run.fuel_used).unwrap_or(i64::MAX),
            error,
            logs: &run.logs,
            request_id,
            created_at: started_at,
        };
        if let Err(err) = store::insert_log(self.backend().as_ref(), log, keep).await {
            tracing::warn!(function = %e.row.name, error = %err, "execution log not written");
        }
        metrics::counter!("forgebase_function_executions_total", "function" => e.row.name.clone(), "status" => status)
            .increment(1);
        metrics::histogram!("forgebase_function_duration_seconds", "function" => e.row.name.clone())
            .record(elapsed.as_secs_f64());
        let output = run.result?;
        Ok(Outcome {
            output,
            version: target.version,
            fuel_used: run.fuel_used,
            logs: run.logs,
        })
    }

    /// Delivers committed record changes from the realtime outbox to
    /// after-hook functions, from the stored cursor on. Returns the number of
    /// hook executions. The background task calls this; tests may call it
    /// directly.
    ///
    /// Each database set (FB-171) has its own outbox and its own cursor,
    /// stored on that set; sets are read one after the other, so hooks run
    /// in commit order within a set. A set that cannot be read is skipped
    /// (and retried next time); control's failure is returned.
    pub async fn run_after_hooks(&self) -> Result<usize> {
        // Once-only delivery: a second caller waits, then starts from the
        // cursor the first one stored.
        let _pass = self.inner.after_lock.lock().await;
        let mut runs = 0;
        for (set, backend) in self.outbox_sets() {
            match self.run_after_hooks_on(&backend).await {
                Ok(n) => runs += n,
                Err(e) if set == forgebase_db::topology::CONTROL_SET_ID => return Err(e),
                Err(e) => tracing::warn!(set = %set, error = %e, "after-hooks of a database set failed; retrying"),
            }
        }
        Ok(runs)
    }

    /// One set's pass of [`FunctionService::run_after_hooks`].
    async fn run_after_hooks_on(&self, backend: &Arc<dyn Backend>) -> Result<usize> {
        let mut cursor = store::cursor(backend.as_ref(), AFTER_CURSOR).await?.unwrap_or(0);
        let mut runs = 0;
        loop {
            let events = forgebase_realtime::outbox::read_after(backend.as_ref(), cursor, 200)
                .await
                .map_err(|e| FunctionError::Internal(e.to_string()))?;
            if events.is_empty() {
                return Ok(runs);
            }
            for ev in &events {
                runs += hooks::after(self, ev).await;
                cursor = ev.seq;
            }
            store::set_cursor(backend.as_ref(), AFTER_CURSOR, cursor, &now()).await?;
        }
    }
}
