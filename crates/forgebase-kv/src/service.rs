//! The service: authorisation, tenant scoping, locking, persistence, pub/sub.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use forgebase_data::{AccessContext, Action, Authorizer, Decision, Principal};
use forgebase_db::Backend;
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::commands::{Class, spec};
use crate::config::{EvictionPolicy, KvConfig};
use crate::error::{KvError, KvResult};
use crate::persist;
use crate::pubsub::{Hub, Subscription};
use crate::reply::Reply;
use crate::store::{Scope, Store};

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

/// Engine-wide statistics.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct KvStats {
    /// Keys of all tenants (expired keys not yet swept included).
    pub keys: usize,
    /// Estimated memory in use, bytes.
    pub used_memory_bytes: usize,
    /// Memory budget in bytes (`0` = unlimited).
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
    /// Keys changed in memory and not yet written to SQLite.
    pub unsaved_keys: usize,
    /// Whether changes are written to SQLite.
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
    backend: Option<Arc<dyn Backend>>,
    flush_lock: tokio::sync::Mutex<()>,
    clock: Clock,
    cfg: KvConfig,
    tasks: Mutex<Vec<JoinHandle<()>>>,
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

/// Namespace names: 1 to 64 characters, start with a letter or digit, then
/// letters, digits, `_`, `.`, `-` or `:`. Names starting with `_` are
/// reserved for the API's own routes.
pub fn validate_namespace(ns: &str) -> KvResult<()> {
    let ok = !ns.is_empty()
        && ns.len() <= 64
        && ns.starts_with(|c: char| c.is_ascii_alphanumeric())
        && ns
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'));
    if ok {
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

    /// Same with an explicit clock.
    pub async fn open_with_clock(
        cfg: KvConfig,
        authorizer: Arc<dyn Authorizer>,
        backend: Option<Arc<dyn Backend>>,
        clock: Clock,
    ) -> KvResult<Self> {
        let mut store = Store::new(cfg.clone());
        if let Some(b) = &backend {
            let now = clock();
            for l in persist::read_all(&**b).await? {
                store.load(l.id, l.data, l.expires_at, l.updated_at, now);
            }
        }
        let hub = Hub::new(cfg.channel_capacity, cfg.max_channels);
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

    fn authorize(&self, ctx: &AccessContext, action: Action) -> KvResult<()> {
        match self.inner.authorizer.authorize(ctx, action, None) {
            Decision::Allow => Ok(()),
            // KV has no row-level predicates: a filter cannot be applied, so it is a denial.
            _ => Err(if ctx.principal == Principal::Anonymous {
                KvError::Unauthenticated
            } else {
                KvError::Forbidden
            }),
        }
    }

    fn scope(ctx: &AccessContext, ns: &str) -> KvResult<Scope> {
        validate_namespace(ns)?;
        Ok(Scope {
            tenant: ctx
                .tenant
                .as_ref()
                .map_or_else(|| "default".to_owned(), |t| format!("t:{t}")),
            ns: ns.to_owned(),
        })
    }

    fn tenant(ctx: &AccessContext) -> String {
        ctx.tenant
            .as_ref()
            .map_or_else(|| "default".to_owned(), |t| format!("t:{t}"))
    }

    fn action_for(class: Class) -> Action {
        match class {
            Class::Read => Action::KvRead,
            Class::Write => Action::KvWrite,
            Class::Admin => Action::KvAdmin,
        }
    }

    /// Authorises every command of `cmds` (the strongest need of each class is
    /// asked once) and returns the scope.
    fn prepare(&self, ctx: &AccessContext, ns: &str, cmds: &[&[Vec<u8>]]) -> KvResult<Scope> {
        let mut classes = [false; 3];
        for c in cmds {
            let name = c
                .first()
                .map(|n| String::from_utf8_lossy(n).to_ascii_uppercase())
                .ok_or_else(|| KvError::Syntax("empty command".into()))?;
            // Unknown commands are rejected after authorisation, so a caller
            // without access learns nothing about the command set.
            if let Some(sp) = spec(&name) {
                classes[sp.class as usize] = true;
            }
        }
        if classes == [false; 3] {
            self.authorize(ctx, Action::KvRead)?;
        }
        for (i, class) in [Class::Read, Class::Write, Class::Admin].into_iter().enumerate() {
            if classes[i] {
                self.authorize(ctx, Self::action_for(class))?;
            }
        }
        Self::scope(ctx, ns)
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
        let scope = self.prepare(ctx, ns, &[cmd])?;
        let mut store = lock(&self.inner.store);
        self.run(&mut store, &scope, cmd)
    }

    /// Runs commands one after another with no other command in between (the
    /// store lock is held). Each command has its own result; a failed command
    /// does not undo the earlier ones.
    pub fn exec_batch(&self, ctx: &AccessContext, ns: &str, cmds: &[Vec<Vec<u8>>]) -> KvResult<Vec<KvResult<Reply>>> {
        let refs: Vec<&[Vec<u8>]> = cmds.iter().map(Vec::as_slice).collect();
        let scope = self.prepare(ctx, ns, &refs)?;
        let mut store = lock(&self.inner.store);
        Ok(cmds.iter().map(|c| self.run(&mut store, &scope, c)).collect())
    }

    /// Subscribes to a channel of a namespace.
    pub fn subscribe(&self, ctx: &AccessContext, ns: &str, channel: &str) -> KvResult<Subscription> {
        self.authorize(ctx, Action::KvRead)?;
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
        self.authorize(ctx, Action::KvRead)?;
        let scope = Self::scope(ctx, ns)?;
        self.inner.hub.subscribe_all(&scope)
    }

    /// Channels of a namespace with at least one subscriber (messages are not
    /// stored, so a channel exists only while someone listens).
    pub fn channels(&self, ctx: &AccessContext, ns: &str) -> KvResult<Vec<ChannelInfo>> {
        self.authorize(ctx, Action::KvRead)?;
        let scope = Self::scope(ctx, ns)?;
        Ok(self
            .inner
            .hub
            .channels_of(&scope)
            .into_iter()
            .map(|(name, subscribers)| ChannelInfo { name, subscribers })
            .collect())
    }

    /// Namespaces that hold keys, for the caller's tenant.
    pub fn namespaces(&self, ctx: &AccessContext) -> KvResult<Vec<NamespaceInfo>> {
        self.authorize(ctx, Action::KvRead)?;
        let tenant = Self::tenant(ctx);
        Ok(lock(&self.inner.store)
            .namespaces(&tenant)
            .into_iter()
            .map(|(name, keys)| NamespaceInfo { name, keys })
            .collect())
    }

    /// Engine-wide statistics (administrators).
    pub fn stats(&self, ctx: &AccessContext) -> KvResult<KvStats> {
        self.authorize(ctx, Action::KvAdmin)?;
        let store = lock(&self.inner.store);
        Ok(KvStats {
            keys: store.key_count(),
            used_memory_bytes: store.used_memory(),
            max_memory_bytes: store.cfg.max_memory_bytes,
            eviction_policy: match store.cfg.eviction {
                EvictionPolicy::NoEviction => "noeviction",
                EvictionPolicy::AllKeysLru => "allkeys-lru",
                EvictionPolicy::VolatileLru => "volatile-lru",
            }
            .to_owned(),
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

    /// Writes keys changed since the last flush to SQLite; returns how many
    /// rows were written. No-op without a backend.
    pub async fn flush(&self) -> KvResult<usize> {
        let Some(backend) = &self.inner.backend else {
            return Ok(0);
        };
        let _guard = self.inner.flush_lock.lock().await;
        let changes = lock(&self.inner.store).take_dirty();
        let n = changes.len();
        // Cancellation safety: `shutdown` aborts the background flusher, which may
        // be in the middle of this very write. The taken keys must then go back
        // to the dirty set (declared after `_guard`, so it drops first, still
        // under the flush lock), or the final flush would find nothing to write.
        let mut pending = Redirty {
            inner: &self.inner,
            ids: changes.iter().map(|(id, _)| id.clone()).collect(),
        };
        persist::write(&**backend, &changes).await?;
        pending.ids.clear();
        Ok(n)
    }

    /// Starts the background expiry sweeper and, with a backend, the SQLite
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
