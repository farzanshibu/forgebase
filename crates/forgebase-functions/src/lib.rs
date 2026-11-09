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

pub mod error;
pub mod fetch;
pub mod hooks;
pub mod host;
pub mod http;
pub mod jobs;
pub mod model;
pub mod runtime;
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
    CAPABILITIES, DeployRequest, Deployed, ExecutionLog, FunctionInfo, HookEvent, HookSpec, InvokeAccess, Limits,
    LimitsInput, LogLine, Outcome, SecretInfo, Trigger, VersionConfig, VersionInfo,
};

use crate::host::HostEnv;
use crate::runtime::Runtime;

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
    cfg: FunctionsConfig,
    registry: RwLock<Arc<Registry>>,
    modules: Mutex<HashMap<(String, i64), wasmtime::Module>>,
    global: Arc<Semaphore>,
    write_lock: tokio::sync::Mutex<()>,
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
        let rows = store::load_all(data.backend().as_ref()).await?;
        let registry = Registry::build(rows, &Registry::default());
        let svc = Self {
            inner: Arc::new(Inner {
                global: Arc::new(Semaphore::new(cfg.max_concurrent.max(1))),
                data,
                kv,
                keyring,
                runtime,
                cfg,
                registry: RwLock::new(Arc::new(registry)),
                modules: Mutex::new(HashMap::new()),
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
        // After-hooks see changes from now on, not the outbox's history.
        let backend = svc.backend();
        if store::cursor(backend.as_ref(), AFTER_CURSOR).await?.is_none() {
            let (_, head) = forgebase_realtime::outbox::bounds(backend.as_ref())
                .await
                .map_err(|e| FunctionError::Internal(e.to_string()))?;
            store::set_cursor(backend.as_ref(), AFTER_CURSOR, head, &now()).await?;
        }
        Ok(svc)
    }

    /// Starts the after-hook dispatcher.
    pub fn start_background(&self) {
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

    pub(crate) fn backend(&self) -> Arc<dyn Backend> {
        self.inner.data.backend().clone()
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
        let wasm = base64::engine::general_purpose::STANDARD
            .decode(req.wasm.trim())
            .map_err(|_| FunctionError::invalid("invalid_wasm_encoding", "wasm must be standard base64"))?;
        if wasm.len() > self.inner.cfg.max_module_bytes {
            return Err(FunctionError::invalid(
                "module_too_large",
                format!("modules are limited to {} bytes", self.inner.cfg.max_module_bytes),
            ));
        }
        let mut caps = req.capabilities.clone();
        caps.sort();
        caps.dedup();
        if let Some(bad) = caps.iter().find(|c| !CAPABILITIES.contains(&c.as_str())) {
            return Err(FunctionError::invalid(
                "invalid_capability",
                format!("unknown capability {bad:?} (one of {})", CAPABILITIES.join(", ")),
            ));
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
        let config = VersionConfig {
            capabilities: caps,
            fetch_hosts: req.fetch_hosts.clone(),
            limits,
            env: req.env.clone(),
            hooks,
        };
        let module = self.inner.runtime.compile(wasm.clone(), &config).await?;
        let sha = hex(&Sha256::digest(&wasm));
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
                "INSERT INTO _forgebase_function_versions (function_id, version, wasm, sha256, config, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![
                    id.clone().into(),
                    version.into(),
                    Value::Blob(wasm.clone()),
                    sha.clone().into(),
                    config_json.clone().into(),
                    at.clone().into(),
                    actor.clone().into(),
                ],
            ))
            .await?;
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
        self.inner
            .modules
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((id.clone(), version), module);
        self.reload().await?;
        let entry = self.entry(&req.name)?;
        tracing::info!(function = %req.name, version, sha256 = %sha, actor = %actor, "function deployed");
        metrics::counter!("forgebase_function_deploys_total").increment(1);
        Ok(Deployed {
            version: VersionInfo {
                version,
                sha256: sha,
                size: i64::try_from(wasm.len()).unwrap_or(i64::MAX),
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

    async fn module(&self, e: &Entry) -> Result<wasmtime::Module> {
        let key = (e.row.id.clone(), e.row.active_version);
        if let Some(m) = self.inner.modules.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
            return Ok(m.clone());
        }
        let wasm = store::wasm(self.backend().as_ref(), &e.row.id, e.row.active_version)
            .await?
            .ok_or_else(|| FunctionError::Internal(format!("module of {} is missing", e.row.name)))?;
        let module = self.inner.runtime.compile(wasm, &e.config).await?;
        let mut cache = self.inner.modules.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, module.clone());
        Ok(module)
    }

    async fn open_secrets(&self, e: &Entry) -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        if !e.config.has("secrets") {
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
        self.execute(&e, trigger, input.to_string().into_bytes(), request_id, |_| None)
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
    pub(crate) async fn execute(
        &self,
        e: &Entry,
        trigger: Trigger,
        input: Vec<u8>,
        request_id: Option<String>,
        judge: impl FnOnce(Option<&[u8]>) -> Option<(&'static str, String)>,
    ) -> Result<Outcome> {
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
        let module = self.module(e).await?;
        let secrets = self.open_secrets(e).await?;
        let mut ident = identity(&e.row.name);
        if let Some(r) = &request_id {
            ident = ident.with_request_id(r.clone());
        }
        let env = Arc::new(HostEnv {
            data: self.inner.data.clone(),
            kv: self.inner.kv.clone(),
            fetcher: self.inner.cfg.fetcher.clone(),
            identity: ident,
            function: e.row.name.clone(),
            config: e.config.clone(),
            secrets,
        });
        let started_at = now();
        let start = Instant::now();
        let run = DEPTH
            .scope(depth + 1, self.inner.runtime.run(&module, &e.config.limits, env, input))
            .await;
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
            version: e.row.active_version,
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
            version: e.row.active_version,
            fuel_used: run.fuel_used,
            logs: run.logs,
        })
    }

    /// Delivers committed record changes from the realtime outbox to
    /// after-hook functions, from the stored cursor on. Returns the number of
    /// hook executions. The background task calls this; tests may call it
    /// directly.
    pub async fn run_after_hooks(&self) -> Result<usize> {
        let backend = self.backend();
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
