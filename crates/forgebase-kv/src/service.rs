//! The service: authorisation, tenant scoping, locking, persistence, pub/sub.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use forgebase_data::{AccessContext, Action, Authorizer, Decision, Principal, ResourceKind, ResourceOp, ResourceRef};
use forgebase_db::{Backend, DbRouter, RouterError, TenantRef};
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::commands::{Class, spec};
use crate::config::{EvictionPolicy, KvConfig};
use crate::error::{KvError, KvResult};
use crate::persist;
use crate::pubsub::{Hub, Subscription};
use crate::reply::Reply;
use crate::store::{KeyId, Scope, Store};

/// Source of "now" in unix milliseconds (injectable for tests).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The wall clock.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    })
}

/// Statistics: engine-wide for a privileged caller without a tenant, else
/// those of the caller's tenant (FB-138).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct KvStats {
    /// `engine` (every tenant) or `tenant` (the caller's tenant only).
    pub scope: String,
    /// The tenant the figures are for (`scope = tenant`).
    pub tenant: Option<String>,
    /// Keys (expired keys not yet swept included).
    pub keys: usize,
    /// Estimated memory in use, bytes.
    pub used_memory_bytes: usize,
    /// Memory budget in bytes (`0` = unlimited): the engine's, or the
    /// per-tenant budget `kv.tenant_max_bytes` for `scope = tenant`.
    pub max_memory_bytes: usize,
    /// Eviction policy: `noeviction`, `allkeys-lru` or `volatile-lru`.
    pub eviction_policy: String,
    /// Reads that found a key.
    pub hits: u64,
    /// Reads that found nothing.
    pub misses: u64,
    /// Keys removed because their TTL passed.
    pub expired_keys: u64,
    /// Keys removed to stay within the memory budget.
    pub evicted_keys: u64,
    /// Live pub/sub channels.
    pub channels: usize,
    /// Keys changed in memory and not yet written to the database.
    pub unsaved_keys: usize,
    /// Whether changes are written to the database.
    pub persistent: bool,
}

/// A live pub/sub channel of a namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ChannelInfo {
    /// Channel name.
    pub name: String,
    /// Connected subscribers.
    pub subscribers: usize,
}

/// One namespace of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct NamespaceInfo {
    /// Namespace name.
    pub name: String,
    /// Keys in it.
    pub keys: usize,
}

struct Inner {
    store: Mutex<Store>,
    hub: Hub,
    authorizer: Arc<dyn Authorizer>,
    backend: Option<Persistence>,
    flush_lock: tokio::sync::Mutex<()>,
    clock: Clock,
    cfg: KvConfig,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// Where persisted keys live.
enum Persistence {
    /// One database (SQLite, one PostgreSQL set).
    Single(Arc<dyn Backend>),
    /// Every set of a sharded cluster (ADR-0029 §3.5, FB-172): a key is
    /// written on its tenant's set (scope `t:<env>`); instance-wide keys
    /// (`default`, `shared:<realm>`) on control.
    Routed(DbRouter),
}

/// The KV engine for one ForgeBase instance. Cheap to clone.
#[derive(Clone)]
pub struct KvService {
    inner: Arc<Inner>,
}

impl fmt::Debug for KvService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KvService")
            .field("persistent", &self.inner.backend.is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Prefix of a realm-wide shared namespace (FB-138): `shared.<name>` holds
/// the same keys for every tenant of the realm and for callers without a
/// tenant. Reaching one needs its own permission key
/// (`engine:kv_<action>:shared.<name>`, or the plain `engine:kv_<action>`).
pub const SHARED_PREFIX: &str = "shared.";

/// Prefix of a server function's private namespace (`fn.<function>`): only
/// privileged callers (the function's own system context, administrators)
/// reach it.
pub const FUNCTION_PREFIX: &str = "fn.";

/// Tells [`KvService::command_needs`] whether a key already exists.
pub type KeyExists<'a> = &'a dyn Fn(&[u8]) -> bool;

/// Namespace names: 1 to 64 characters, start with a letter or digit, then
/// letters, digits, `_`, `.`, `-` or `:`. Names starting with `_` are
/// reserved for the API's own routes. `shared.` must be followed by a name.
pub fn validate_namespace(ns: &str) -> KvResult<()> {
    let ok = !ns.is_empty()
        && ns.len() <= 64
        && ns.starts_with(|c: char| c.is_ascii_alphanumeric())
        && ns
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'));
    if ok && ns != SHARED_PREFIX {
        Ok(())
    } else {
        Err(KvError::InvalidNamespace(
            "namespace must be 1-64 characters: letters and digits first, then letters, digits, _ . - :".into(),
        ))
    }
}

impl KvService {
    /// An engine with the wall clock. With a `backend`, persisted keys are
    /// loaded now and changes are written back (see [`KvService::start_background`]).
    pub async fn open(
        cfg: KvConfig,
        authorizer: Arc<dyn Authorizer>,
        backend: Option<Arc<dyn Backend>>,
    ) -> KvResult<Self> {
        Self::open_with_clock(cfg, authorizer, backend, system_clock()).await
    }

    /// An engine persisted on the sets of `router` (FB-172): start-up loads
    /// every set (a tenant's keys only from the set that owns it, since a
    /// move leaves stale copies on the source until cleanup), and the flusher
    /// writes each dirty key on its tenant's set. With one set this is
    /// [`KvService::open`] over that set.
    pub async fn open_routed(cfg: KvConfig, authorizer: Arc<dyn Authorizer>, router: DbRouter) -> KvResult<Self> {
        Self::open_routed_with_clock(cfg, authorizer, router, system_clock()).await
    }

    /// [`KvService::open_routed`] with an explicit clock.
    pub async fn open_routed_with_clock(
        cfg: KvConfig,
        authorizer: Arc<dyn Authorizer>,
        router: DbRouter,
        clock: Clock,
    ) -> KvResult<Self> {
        Self::open_persisted(cfg, authorizer, Some(Persistence::Routed(router)), clock).await
    }

    /// Same with an explicit clock.
    pub async fn open_with_clock(
        cfg: KvConfig,
        authorizer: Arc<dyn Authorizer>,
        backend: Option<Arc<dyn Backend>>,
        clock: Clock,
    ) -> KvResult<Self> {
        Self::open_persisted(cfg, authorizer, backend.map(Persistence::Single), clock).await
    }

    async fn open_persisted(
        cfg: KvConfig,
        authorizer: Arc<dyn Authorizer>,
        backend: Option<Persistence>,
        clock: Clock,
    ) -> KvResult<Self> {
        let mut store = Store::new(cfg.clone());
        let loaded = match &backend {
            None => Vec::new(),
            Some(Persistence::Single(b)) => persist::read_all(&**b).await?,
            Some(Persistence::Routed(router)) => load_sets(router).await?,
        };
        let now = clock();
        for l in loaded {
            store.load(l.id, l.data, l.expires_at, l.updated_at, now);
        }
        let hub = Hub::new(cfg.channel_capacity, cfg.max_channels);
        // Namespace rules are namespace-level (edge-functions.md §2): they may
        // use `@request` values only, never item fields, so a rule can never
        // come back as a filter the engine cannot apply (FB-138).
        authorizer.register_resource_schema(forgebase_data::ResourceSchema::new(ResourceKind::KvNamespace));
        Ok(Self {
            inner: Arc::new(Inner {
                store: Mutex::new(store),
                hub,
                authorizer,
                backend,
                flush_lock: tokio::sync::Mutex::new(()),
                clock,
                cfg,
                tasks: Mutex::new(Vec::new()),
            }),
        })
    }

    fn now(&self) -> u64 {
        (self.inner.clock)()
    }

    fn denied(ctx: &AccessContext) -> KvError {
        if ctx.principal == Principal::Anonymous {
            KvError::Unauthenticated
        } else {
            KvError::Forbidden
        }
    }

    /// The plain check, for calls that span every namespace (listing
    /// namespaces, statistics): only `engine:kv_<action>` grants those.
    fn authorize(&self, ctx: &AccessContext, action: Action) -> KvResult<()> {
        match self.inner.authorizer.authorize(ctx, action, None) {
            Decision::Allow => Ok(()),
            // KV has no row-level predicates: a filter cannot be applied, so it is a denial.
            _ => Err(Self::denied(ctx)),
        }
    }

    /// The namespace check (FB-134 resource rules, rls-coverage G7):
    /// `engine:kv_<action>:<ns>` or the plain key, else the namespace's
    /// resource rule for `op`.
    fn authorize_ns(&self, ctx: &AccessContext, action: Action, ns: &str, op: ResourceOp) -> KvResult<()> {
        let res = ResourceRef::new(ResourceKind::KvNamespace, ns, op);
        match self.inner.authorizer.authorize_resource(ctx, action, &res) {
            Decision::Allow => Ok(()),
            // Namespace rules are decided on `@request` only (the schema has
            // no item fields); a filter cannot be applied, so it is a denial.
            _ => Err(Self::denied(ctx)),
        }
    }

    /// The scope tenant of a caller (FB-138): `t:<tenant>`; without a tenant,
    /// the legacy shared `default` for privileged callers only.
    fn tenant_of(ctx: &AccessContext) -> KvResult<String> {
        match &ctx.tenant {
            Some(t) => Ok(format!("t:{t}")),
            None if ctx.is_privileged() => Ok("default".to_owned()),
            None => Err(KvError::TenantRequired(
                "select a tenant (X-ForgeBase-Tenant header, or a tenant-bound key), or use a shared.<name> namespace"
                    .into(),
            )),
        }
    }

    fn shared_tenant(ctx: &AccessContext) -> String {
        format!("shared:{}", ctx.realm_id)
    }

    /// The tenant part of the scope of namespace `ns` for `ctx` (FB-138):
    /// `shared:<realm>` for `shared.*`, else `t:<tenant>`, else `default` for
    /// privileged callers; `tenant_required` for anyone else. `fn.*` is
    /// refused to non-privileged callers. Call after authorising, so a
    /// caller without access learns nothing about tenancy.
    pub fn scope_tenant(ctx: &AccessContext, ns: &str) -> KvResult<String> {
        validate_namespace(ns)?;
        if ns.starts_with(FUNCTION_PREFIX) && !ctx.is_privileged() {
            return Err(KvError::Forbidden);
        }
        if ns.starts_with(SHARED_PREFIX) {
            Ok(Self::shared_tenant(ctx))
        } else {
            Self::tenant_of(ctx)
        }
    }

    fn scope(ctx: &AccessContext, ns: &str) -> KvResult<Scope> {
        Ok(Scope {
            tenant: Self::scope_tenant(ctx, ns)?,
            ns: ns.to_owned(),
        })
    }

    fn action_for(class: Class) -> Action {
        match class {
            Class::Read => Action::KvRead,
            Class::Write => Action::KvWrite,
            Class::Admin => Action::KvAdmin,
        }
    }

    /// The (action, operation) pairs one command needs (FB-138): scans are
    /// `List`, other reads `View`, deletions and `FLUSHDB` `Delete`,
    /// `PUBLISH` `Create`, other writes `Update`, and writes that may add a
    /// key `Create` for a key that does not exist yet and `Update` for one
    /// that does (`exists`; without it, both). Pairs already in `out` are not
    /// repeated. Unknown commands need `(KvRead, View)`, so a caller without
    /// access learns nothing about the command set.
    pub fn command_needs(
        cmd: &[Vec<u8>],
        exists: Option<KeyExists<'_>>,
        out: &mut Vec<(Action, ResourceOp)>,
    ) -> KvResult<()> {
        let name = cmd
            .first()
            .map(|n| String::from_utf8_lossy(n).to_ascii_uppercase())
            .ok_or_else(|| KvError::Syntax("empty command".into()))?;
        let Some(sp) = spec(&name) else {
            if !out.contains(&(Action::KvRead, ResourceOp::View)) {
                out.push((Action::KvRead, ResourceOp::View));
            }
            return Ok(());
        };
        let action = Self::action_for(sp.class);
        let mut push = |op| {
            if !out.contains(&(action, op)) {
                out.push((action, op));
            }
        };
        match (sp.class, name.as_str()) {
            (Class::Read, "KEYS" | "SCAN" | "DBSIZE") => push(ResourceOp::List),
            (Class::Read, _) => push(ResourceOp::View),
            (Class::Admin, _) | (Class::Write, "DEL" | "UNLINK" | "GETDEL") => push(ResourceOp::Delete),
            (Class::Write, "PUBLISH") => push(ResourceOp::Create),
            (Class::Write, _) if sp.grows => {
                let args = &cmd[1..];
                let keys: Vec<&Vec<u8>> = if name == "MSET" {
                    args.iter().step_by(2).collect()
                } else {
                    args.first().into_iter().collect()
                };
                for k in keys {
                    match exists {
                        Some(f) if f(k) => push(ResourceOp::Update),
                        Some(_) => push(ResourceOp::Create),
                        None => {
                            push(ResourceOp::Create);
                            push(ResourceOp::Update);
                        }
                    }
                }
            }
            (Class::Write, _) => push(ResourceOp::Update),
        }
        Ok(())
    }

    /// Authorises every command of `cmds` in namespace `ns` (each needed
    /// action and operation is asked once) and returns the scope.
    fn prepare(&self, store: &Store, ctx: &AccessContext, ns: &str, cmds: &[&[Vec<u8>]]) -> KvResult<Scope> {
        let scope = Self::scope(ctx, ns);
        let now = self.now();
        let exists = |k: &[u8]| match (&scope, std::str::from_utf8(k)) {
            (Ok(s), Ok(k)) => store.exists_now(&KeyId::new(s, k), now),
            _ => false,
        };
        let mut needs = Vec::new();
        for c in cmds {
            Self::command_needs(c, Some(&exists), &mut needs)?;
        }
        for (action, op) in needs {
            self.authorize_ns(ctx, action, ns, op)?;
        }
        scope
    }

    fn run(&self, store: &mut Store, scope: &Scope, cmd: &[Vec<u8>]) -> KvResult<Reply> {
        let name = cmd.first().map(|n| String::from_utf8_lossy(n).to_ascii_uppercase());
        if name.as_deref() == Some("PUBLISH") {
            let sp = spec("PUBLISH");
            if sp.is_none_or(|s| cmd.len() - 1 < s.min_args || cmd.len() - 1 > s.max_args) {
                return Err(KvError::WrongArity("publish".into()));
            }
            let channel =
                std::str::from_utf8(&cmd[1]).map_err(|_| KvError::Syntax("channel must be valid UTF-8".into()))?;
            if channel.is_empty() || channel.len() > store.cfg.max_key_bytes {
                return Err(KvError::InvalidKey("invalid channel name".into()));
            }
            if cmd[2].len() > store.cfg.max_value_bytes {
                return Err(KvError::TooLarge(store.cfg.max_value_bytes));
            }
            return Ok(Reply::count(self.inner.hub.publish(scope, channel, cmd[2].clone())));
        }
        store.exec(scope, cmd, self.now())
    }

    /// Runs one command in a namespace of the caller's tenant.
    pub fn exec(&self, ctx: &AccessContext, ns: &str, cmd: &[Vec<u8>]) -> KvResult<Reply> {
        let mut store = lock(&self.inner.store);
        let scope = self.prepare(&store, ctx, ns, &[cmd])?;
        self.run(&mut store, &scope, cmd)
    }

    /// Runs commands one after another with no other command in between (the
    /// store lock is held). Each command has its own result; a failed command
    /// does not undo the earlier ones.
    pub fn exec_batch(&self, ctx: &AccessContext, ns: &str, cmds: &[Vec<Vec<u8>>]) -> KvResult<Vec<KvResult<Reply>>> {
        let refs: Vec<&[Vec<u8>]> = cmds.iter().map(Vec::as_slice).collect();
        let mut store = lock(&self.inner.store);
        let scope = self.prepare(&store, ctx, ns, &refs)?;
        Ok(cmds.iter().map(|c| self.run(&mut store, &scope, c)).collect())
    }

    /// Subscribes to a channel of a namespace.
    pub fn subscribe(&self, ctx: &AccessContext, ns: &str, channel: &str) -> KvResult<Subscription> {
        self.authorize_ns(ctx, Action::KvRead, ns, ResourceOp::View)?;
        let scope = Self::scope(ctx, ns)?;
        if channel.is_empty() || channel.len() > self.inner.cfg.max_key_bytes {
            return Err(KvError::InvalidKey("invalid channel name".into()));
        }
        self.inner.hub.subscribe(&scope, channel)
    }

    /// Subscribes to every channel of a namespace at once. Messages carry
    /// their channel, so the caller can filter (pattern subscriptions of the
    /// RESP listener use this). `PUBLISH` replies count only exact-channel
    /// subscribers, not these.
    pub fn subscribe_all(&self, ctx: &AccessContext, ns: &str) -> KvResult<Subscription> {
        self.authorize_ns(ctx, Action::KvRead, ns, ResourceOp::View)?;
        let scope = Self::scope(ctx, ns)?;
        self.inner.hub.subscribe_all(&scope)
    }

    /// Channels of a namespace with at least one subscriber (messages are not
    /// stored, so a channel exists only while someone listens).
    pub fn channels(&self, ctx: &AccessContext, ns: &str) -> KvResult<Vec<ChannelInfo>> {
        self.authorize_ns(ctx, Action::KvRead, ns, ResourceOp::List)?;
        let scope = Self::scope(ctx, ns)?;
        Ok(self
            .inner
            .hub
            .channels_of(&scope)
            .into_iter()
            .map(|(name, subscribers)| ChannelInfo { name, subscribers })
            .collect())
    }

    /// Namespaces that hold keys: those of the caller's tenant (the
    /// privileged `default` scope without one) and the realm's `shared.*`
    /// namespaces, in name order. A non-privileged caller without a tenant
    /// sees the shared ones only.
    pub fn namespaces(&self, ctx: &AccessContext) -> KvResult<Vec<NamespaceInfo>> {
        self.authorize(ctx, Action::KvRead)?;
        let store = lock(&self.inner.store);
        let mut out: Vec<NamespaceInfo> = Self::tenant_of(ctx)
            .map(|t| store.namespaces(&t))
            .unwrap_or_default()
            .into_iter()
            .chain(store.namespaces(&Self::shared_tenant(ctx)))
            .filter(|(name, _)| !name.starts_with(FUNCTION_PREFIX) || ctx.is_privileged())
            .map(|(name, keys)| NamespaceInfo { name, keys })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Statistics (`kv_admin`): engine-wide for a privileged caller without a
    /// tenant, else those of the caller's tenant only (FB-138, G8).
    pub fn stats(&self, ctx: &AccessContext) -> KvResult<KvStats> {
        self.authorize(ctx, Action::KvAdmin)?;
        let tenant = Self::tenant_of(ctx)?;
        let store = lock(&self.inner.store);
        let policy = match store.cfg.eviction {
            EvictionPolicy::NoEviction => "noeviction",
            EvictionPolicy::AllKeysLru => "allkeys-lru",
            EvictionPolicy::VolatileLru => "volatile-lru",
        }
        .to_owned();
        if ctx.tenant.is_some() {
            let u = store.tenant_use(&tenant);
            return Ok(KvStats {
                scope: "tenant".into(),
                tenant: ctx.tenant.clone(),
                keys: u.keys,
                used_memory_bytes: u.used,
                max_memory_bytes: store.cfg.tenant_max_bytes,
                eviction_policy: policy,
                hits: u.stats.hits,
                misses: u.stats.misses,
                expired_keys: u.stats.expired,
                evicted_keys: u.stats.evicted,
                channels: self.inner.hub.channel_count_of(&tenant),
                unsaved_keys: store.dirty_len_of(&tenant),
                persistent: self.inner.backend.is_some(),
            });
        }
        Ok(KvStats {
            scope: "engine".into(),
            tenant: None,
            keys: store.key_count(),
            used_memory_bytes: store.used_memory(),
            max_memory_bytes: store.cfg.max_memory_bytes,
            eviction_policy: policy,
            hits: store.stats.hits,
            misses: store.stats.misses,
            expired_keys: store.stats.expired,
            evicted_keys: store.stats.evicted,
            channels: self.inner.hub.channel_count(),
            unsaved_keys: store.dirty_len(),
            persistent: self.inner.backend.is_some(),
        })
    }

    /// Removes expired keys now; returns how many.
    pub fn sweep_expired(&self) -> usize {
        lock(&self.inner.store).sweep_expired(self.now(), 100_000)
    }

    /// Writes keys changed since the last flush to the database; returns how
    /// many rows were written. No-op without a backend.
    ///
    /// Sharded (FB-172): dirty keys are grouped by their tenant's set and
    /// each group is written in one transaction on that set. Keys of a
    /// tenant that is frozen by a move, or whose set cannot be reached, stay
    /// dirty for the next round; a failed group stays dirty and the error of
    /// the first failure is returned after the other groups were written.
    pub async fn flush(&self) -> KvResult<usize> {
        let Some(backend) = &self.inner.backend else {
            return Ok(0);
        };
        let _guard = self.inner.flush_lock.lock().await;
        let changes = lock(&self.inner.store).take_dirty();
        match backend {
            Persistence::Single(b) => self.write_group(&**b, changes).await,
            Persistence::Routed(router) => self.write_routed(router, changes).await,
        }
    }

    /// Writes one tenant's (environment id) dirty keys now, on the set the
    /// tenant's placement names, even while the tenant is frozen (ADR-0029
    /// §3.8 step 4.1: the switch of a move flushes the tenant's keys on the
    /// source before the barrier). Returns how many rows were written.
    pub async fn flush_tenant(&self, tenant: &str) -> KvResult<usize> {
        let Some(backend) = &self.inner.backend else {
            return Ok(0);
        };
        let _guard = self.inner.flush_lock.lock().await;
        let changes = lock(&self.inner.store).take_dirty_of(&format!("t:{tenant}"));
        match backend {
            Persistence::Single(b) => self.write_group(&**b, changes).await,
            Persistence::Routed(router) => {
                let set = match router.placement(tenant) {
                    Some(p) => router.set(&p.set_id).ok_or(RouterError::SetUnavailable(p.set_id)),
                    None => Ok(router.control()),
                };
                match set {
                    Ok(set) => self.write_group(&set, changes).await,
                    Err(e) => {
                        lock(&self.inner.store).redirty(changes.into_iter().map(|(id, _)| id));
                        Err(KvError::Storage(e.to_string()))
                    }
                }
            }
        }
    }

    async fn write_routed(&self, router: &DbRouter, changes: Vec<persist::Change>) -> KvResult<usize> {
        let mut groups: std::collections::BTreeMap<String, (forgebase_db::SetHandle, Vec<persist::Change>)> =
            std::collections::BTreeMap::new();
        let mut held: Vec<KeyId> = Vec::new();
        for change in changes {
            match router.try_for_tenant(TenantRef::new(placed_tenant(&change.0.scope.tenant))) {
                Ok(set) => groups
                    .entry(set.id().to_owned())
                    .or_insert_with(|| (set, Vec::new()))
                    .1
                    .push(change),
                Err(e) => {
                    tracing::debug!(tenant = %change.0.scope.tenant, error = %e, "kv flush holds the key back");
                    held.push(change.0);
                }
            }
        }
        if !held.is_empty() {
            lock(&self.inner.store).redirty(held);
        }
        let mut written = 0;
        let mut first_error = None;
        for (_, (set, group)) in groups {
            match self.write_group(&set, group).await {
                Ok(n) => written += n,
                Err(e) => {
                    tracing::warn!(set = %set.id(), error = %e, "kv flush to a set failed; will retry");
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if written == 0 => Err(e),
            _ => Ok(written),
        }
    }

    /// Writes `changes` on `backend` in one transaction; on failure (or
    /// cancellation) the keys are dirty again.
    ///
    /// When the group's transaction fails, each change is retried on its own
    /// so that one row the database refuses cannot hold back every other key
    /// of the set (and lose them all at shutdown). The keys that still fail
    /// stay dirty and are retried on the next flush.
    async fn write_group(&self, backend: &dyn Backend, changes: Vec<persist::Change>) -> KvResult<usize> {
        let n = changes.len();
        // Cancellation safety: `shutdown` aborts the background flusher, which may
        // be in the middle of this very write. The taken keys must then go back
        // to the dirty set (declared after `_guard`, so it drops first, still
        // under the flush lock), or the final flush would find nothing to write.
        let mut pending = Redirty {
            inner: &self.inner,
            ids: changes.iter().map(|(id, _)| id.clone()).collect(),
        };
        let group_error = match persist::write(backend, &changes).await {
            Ok(()) => {
                pending.ids.clear();
                return Ok(n);
            }
            Err(e) if n == 1 => return Err(e),
            Err(e) => e,
        };
        let mut written = 0;
        let mut failed = Vec::new();
        let mut last_error = None;
        for change in changes {
            match persist::write(backend, std::slice::from_ref(&change)).await {
                Ok(()) => written += 1,
                Err(e) => {
                    tracing::warn!(
                        tenant = %change.0.scope.tenant,
                        namespace = %change.0.scope.ns,
                        error = %e,
                        "kv key could not be saved; it stays unsaved and is retried"
                    );
                    failed.push(change.0);
                    last_error = Some(e);
                }
            }
        }
        // Only the keys that failed on their own go back to the dirty set.
        pending.ids = failed;
        if written == 0 {
            return Err(last_error.unwrap_or(group_error));
        }
        Ok(written)
    }

    /// Starts the background expiry sweeper and, with a backend, the database
    /// flusher. Call [`KvService::shutdown`] to stop them with a final flush.
    pub fn start_background(&self) {
        let mut tasks = lock(&self.inner.tasks);
        if !tasks.is_empty() {
            return;
        }
        let sweep = self.inner.cfg.sweep_interval;
        if !sweep.is_zero() {
            let weak = Arc::downgrade(&self.inner);
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(sweep);
                loop {
                    tick.tick().await;
                    let Some(inner) = weak.upgrade() else { break };
                    KvService { inner }.sweep_expired();
                }
            }));
        }
        let every = self.inner.cfg.flush_interval;
        if !every.is_zero() && self.inner.backend.is_some() {
            let weak = Arc::downgrade(&self.inner);
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(every);
                loop {
                    tick.tick().await;
                    let Some(inner) = weak.upgrade() else { break };
                    if let Err(e) = (KvService { inner }).flush().await {
                        tracing::warn!(error = %e, "kv flush failed; will retry");
                    }
                }
            }));
        }
    }

    /// Stops the background tasks and writes everything unsaved.
    pub async fn shutdown(&self) -> KvResult<usize> {
        let tasks: Vec<_> = lock(&self.inner.tasks).drain(..).collect();
        for t in &tasks {
            t.abort();
        }
        // Wait until each task is really gone: one aborted mid-write has then
        // put its keys back in the dirty set before the final flush looks.
        for t in tasks {
            let _ = t.await;
        }
        self.flush().await
    }
}

/// Puts keys back in the dirty set when a flush is dropped before it finished
/// (the write failed or the future was cancelled).
struct Redirty<'a> {
    inner: &'a Inner,
    ids: Vec<crate::store::KeyId>,
}

impl Drop for Redirty<'_> {
    fn drop(&mut self) {
        if !self.ids.is_empty() {
            lock(&self.inner.store).redirty(std::mem::take(&mut self.ids));
        }
    }
}

/// The environment a stored scope tenant belongs to: `t:<env>` is a
/// tenant's; `default` and `shared:<realm>` are instance-wide (control).
fn placed_tenant(stored: &str) -> Option<&str> {
    stored.strip_prefix("t:").filter(|t| !t.is_empty())
}

/// Every persisted key of every set, oldest write first. A tenant's keys are
/// taken only from the set that owns it (its placement's set, else control):
/// a move leaves the tenant's rows on the source until cleanup, and a target
/// holds a partial copy until the switch.
async fn load_sets(router: &DbRouter) -> KvResult<Vec<persist::Loaded>> {
    let control = router.control().id().to_owned();
    let mut all = Vec::new();
    for set in router.sets() {
        let rows = persist::read_all(&set).await?;
        let owned = rows.into_iter().filter(|l| {
            let owner = match placed_tenant(&l.id.scope.tenant).and_then(|t| router.placement(t)) {
                Some(p) => p.set_id,
                None => control.clone(),
            };
            owner == set.id()
        });
        all.extend(owned);
    }
    all.sort_by_key(|l| l.updated_at);
    Ok(all)
}
