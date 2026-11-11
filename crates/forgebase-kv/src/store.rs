//! The in-memory keyspace: entries, expiry index, LRU index, memory accounting.
//!
//! All tenants and namespaces live in one ordered map keyed by
//! `(tenant, namespace, key)`. A [`Scope`] (tenant + namespace) is part of every
//! key, so a command can only ever address keys of its own scope: there is no
//! API that takes a bare key.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Bound;

use crate::config::{EvictionPolicy, KvConfig};
use crate::data::{Data, ENTRY_OVERHEAD};
use crate::error::{KvError, KvResult};

/// Tenant plus namespace: the unit of isolation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Scope {
    pub(crate) tenant: String,
    pub(crate) ns: String,
}

/// A key inside a scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct KeyId {
    pub(crate) scope: Scope,
    pub(crate) key: String,
}

impl KeyId {
    pub(crate) fn new(scope: &Scope, key: &str) -> Self {
        Self {
            scope: scope.clone(),
            key: key.to_owned(),
        }
    }
}

/// One stored key.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) data: Data,
    /// Unix milliseconds at which the key expires.
    pub(crate) expires_at: Option<u64>,
    /// Unix milliseconds of the last write.
    pub(crate) updated_at: u64,
    tick: u64,
    mem: usize,
}

/// Counters since start.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Stats {
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) expired: u64,
    pub(crate) evicted: u64,
}

/// What one tenant uses (FB-138): its keys, memory and counters. The
/// "tenant" is the scope's tenant string: `t:<id>`, `shared:<realm>` or the
/// privileged `default`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TenantUse {
    pub(crate) keys: usize,
    pub(crate) used: usize,
    pub(crate) stats: Stats,
}

fn entry_mem(id: &KeyId, data: &Data) -> usize {
    ENTRY_OVERHEAD + 3 * (id.key.len() + id.scope.tenant.len() + id.scope.ns.len()) + data.mem()
}

/// The keyspace of all tenants.
#[derive(Debug)]
pub(crate) struct Store {
    pub(crate) cfg: KvConfig,
    map: BTreeMap<KeyId, Entry>,
    lru: BTreeMap<u64, KeyId>,
    expiries: BTreeSet<(u64, KeyId)>,
    ns_counts: BTreeMap<Scope, usize>,
    /// Per-tenant LRU index: `(tenant, tick)` of every key (FB-138).
    tenant_lru: BTreeSet<(String, u64)>,
    tenants: HashMap<String, TenantUse>,
    tick: u64,
    used: usize,
    dirty: HashSet<KeyId>,
    pub(crate) stats: Stats,
}

impl Store {
    pub(crate) fn new(cfg: KvConfig) -> Self {
        Self {
            cfg,
            map: BTreeMap::new(),
            lru: BTreeMap::new(),
            expiries: BTreeSet::new(),
            ns_counts: BTreeMap::new(),
            tenant_lru: BTreeSet::new(),
            tenants: HashMap::new(),
            tick: 0,
            used: 0,
            dirty: HashSet::new(),
            stats: Stats::default(),
        }
    }

    pub(crate) fn used_memory(&self) -> usize {
        self.used
    }

    pub(crate) fn key_count(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn scope_count(&self, scope: &Scope) -> usize {
        self.ns_counts.get(scope).copied().unwrap_or(0)
    }

    /// What `tenant` uses (zero when it holds nothing).
    pub(crate) fn tenant_use(&self, tenant: &str) -> TenantUse {
        self.tenants.get(tenant).copied().unwrap_or_default()
    }

    /// Changed keys of `tenant` not yet written to SQLite.
    pub(crate) fn dirty_len_of(&self, tenant: &str) -> usize {
        self.dirty.iter().filter(|k| k.scope.tenant == tenant).count()
    }

    fn tenant_mut(&mut self, tenant: &str) -> &mut TenantUse {
        if !self.tenants.contains_key(tenant) {
            self.tenants.insert(tenant.to_owned(), TenantUse::default());
        }
        let Some(t) = self.tenants.get_mut(tenant) else {
            unreachable!("inserted above")
        };
        t
    }

    /// Whether a key exists and is not expired, without any side effect.
    pub(crate) fn exists_now(&self, id: &KeyId, now: u64) -> bool {
        self.map
            .get(id)
            .is_some_and(|e| !e.expires_at.is_some_and(|t| t <= now))
    }

    /// Namespaces of a tenant with their key counts, in name order.
    pub(crate) fn namespaces(&self, tenant: &str) -> Vec<(String, usize)> {
        self.ns_counts
            .iter()
            .filter(|(s, _)| s.tenant == tenant)
            .map(|(s, n)| (s.ns.clone(), *n))
            .collect()
    }

    // ---- bookkeeping -------------------------------------------------------

    fn insert_entry(&mut self, id: KeyId, data: Data, expires_at: Option<u64>, now: u64) {
        self.remove_entry(&id);
        self.tick += 1;
        let mem = entry_mem(&id, &data);
        self.used += mem;
        self.lru.insert(self.tick, id.clone());
        self.tenant_lru.insert((id.scope.tenant.clone(), self.tick));
        let tu = self.tenant_mut(&id.scope.tenant);
        tu.used += mem;
        tu.keys += 1;
        if let Some(t) = expires_at {
            self.expiries.insert((t, id.clone()));
        }
        *self.ns_counts.entry(id.scope.clone()).or_insert(0) += 1;
        self.dirty.insert(id.clone());
        self.map.insert(
            id,
            Entry {
                data,
                expires_at,
                updated_at: now,
                tick: self.tick,
                mem,
            },
        );
    }

    fn remove_entry(&mut self, id: &KeyId) -> Option<Entry> {
        let e = self.map.remove(id)?;
        self.lru.remove(&e.tick);
        self.tenant_lru.remove(&(id.scope.tenant.clone(), e.tick));
        if let Some(tu) = self.tenants.get_mut(&id.scope.tenant) {
            tu.used = tu.used.saturating_sub(e.mem);
            tu.keys = tu.keys.saturating_sub(1);
        }
        if let Some(t) = e.expires_at {
            self.expiries.remove(&(t, id.clone()));
        }
        self.used = self.used.saturating_sub(e.mem);
        if let Some(n) = self.ns_counts.get_mut(&id.scope) {
            *n -= 1;
            if *n == 0 {
                self.ns_counts.remove(&id.scope);
            }
        }
        self.dirty.insert(id.clone());
        Some(e)
    }

    fn touch(&mut self, id: &KeyId) {
        if let Some(e) = self.map.get_mut(id) {
            self.lru.remove(&e.tick);
            self.tenant_lru.remove(&(id.scope.tenant.clone(), e.tick));
            self.tick += 1;
            e.tick = self.tick;
            self.lru.insert(self.tick, id.clone());
            self.tenant_lru.insert((id.scope.tenant.clone(), self.tick));
        }
    }

    /// Removes the key when its TTL has passed.
    fn expire_if_due(&mut self, id: &KeyId, now: u64) {
        if self.map.get(id).is_some_and(|e| e.expires_at.is_some_and(|t| t <= now)) {
            self.remove_entry(id);
            self.stats.expired += 1;
            self.tenant_mut(&id.scope.tenant).stats.expired += 1;
        }
    }

    // ---- access ------------------------------------------------------------

    /// Whether the key exists (and is not expired). Does not touch LRU or stats.
    pub(crate) fn live(&mut self, id: &KeyId, now: u64) -> bool {
        self.expire_if_due(id, now);
        self.map.contains_key(id)
    }

    /// Reads a key: lazily expires it, counts a hit or miss, marks it recently used.
    pub(crate) fn get_entry(&mut self, id: &KeyId, now: u64) -> Option<&Entry> {
        self.expire_if_due(id, now);
        let hit = self.map.contains_key(id);
        let tu = &mut self.tenant_mut(&id.scope.tenant).stats;
        if hit {
            tu.hits += 1;
            self.stats.hits += 1;
            self.touch(id);
        } else {
            tu.misses += 1;
            self.stats.misses += 1;
        }
        self.map.get(id)
    }

    /// Entry without side effects (expiry is checked by the caller).
    pub(crate) fn peek(&self, id: &KeyId) -> Option<&Entry> {
        self.map.get(id)
    }

    /// Mutates a key in place and keeps the accounting right. `create` is the
    /// value to start from when the key is missing; `None` makes a missing key
    /// `Ok(None)` without calling `f`. A collection left empty is removed.
    /// `f` must not change the value when it returns an error.
    pub(crate) fn modify<R>(
        &mut self,
        id: &KeyId,
        now: u64,
        create: Option<Data>,
        f: impl FnOnce(&mut Data) -> KvResult<R>,
    ) -> KvResult<Option<R>> {
        self.expire_if_due(id, now);
        let mut created = false;
        if !self.map.contains_key(id) {
            let Some(d) = create else { return Ok(None) };
            self.insert_entry(id.clone(), d, None, now);
            created = true;
        }
        let Some(entry) = self.map.get_mut(id) else {
            return Ok(None);
        };
        let res = f(&mut entry.data);
        let new_mem = entry_mem(id, &entry.data);
        let old_mem = entry.mem;
        entry.mem = new_mem;
        let empty = entry.data.is_empty();
        self.used = (self.used + new_mem).saturating_sub(old_mem);
        let tu = self.tenant_mut(&id.scope.tenant);
        tu.used = (tu.used + new_mem).saturating_sub(old_mem);
        if empty || (created && res.is_err()) {
            self.remove_entry(id);
        } else if res.is_ok() {
            if let Some(e) = self.map.get_mut(id) {
                e.updated_at = now;
            }
            self.touch(id);
            self.dirty.insert(id.clone());
        }
        res.map(Some)
    }

    /// Replaces a key with a new value (any type). `keep_ttl` keeps the old expiry.
    pub(crate) fn replace(&mut self, id: &KeyId, data: Data, expires_at: Option<u64>, keep_ttl: bool, now: u64) {
        let expires_at = if keep_ttl {
            self.map.get(id).and_then(|e| e.expires_at)
        } else {
            expires_at
        };
        self.insert_entry(id.clone(), data, expires_at, now);
    }

    pub(crate) fn delete(&mut self, id: &KeyId, now: u64) -> bool {
        self.expire_if_due(id, now);
        self.remove_entry(id).is_some()
    }

    /// Sets or clears the expiry of an existing key; false when it does not exist.
    pub(crate) fn set_expiry(&mut self, id: &KeyId, at: Option<u64>, now: u64) -> bool {
        self.expire_if_due(id, now);
        let Some(e) = self.map.get_mut(id) else {
            return false;
        };
        if let Some(t) = e.expires_at {
            self.expiries.remove(&(t, id.clone()));
        }
        e.expires_at = at;
        if let Some(t) = at {
            self.expiries.insert((t, id.clone()));
        }
        self.dirty.insert(id.clone());
        true
    }

    /// Live entries of a scope in key order, starting after `after`.
    pub(crate) fn scan_scope<'a>(
        &'a self,
        scope: &Scope,
        after: Option<&str>,
        now: u64,
    ) -> impl Iterator<Item = (&'a KeyId, &'a Entry)> + 'a {
        let start = match after {
            Some(k) => Bound::Excluded(KeyId::new(scope, k)),
            None => Bound::Included(KeyId::new(scope, "")),
        };
        let scope = scope.clone();
        self.map
            .range((start, Bound::Unbounded))
            .take_while(move |(k, _)| k.scope == scope)
            .filter(move |(_, e)| !e.expires_at.is_some_and(|t| t <= now))
    }

    /// Deletes every key of a scope; returns how many.
    pub(crate) fn flush_scope(&mut self, scope: &Scope) -> usize {
        let ids: Vec<KeyId> = self
            .map
            .range((Bound::Included(KeyId::new(scope, "")), Bound::Unbounded))
            .take_while(|(k, _)| k.scope == *scope)
            .map(|(k, _)| k.clone())
            .collect();
        for id in &ids {
            self.remove_entry(id);
        }
        ids.len()
    }

    // ---- expiry, eviction --------------------------------------------------

    /// Removes up to `limit` keys whose TTL has passed. Returns how many.
    pub(crate) fn sweep_expired(&mut self, now: u64, limit: usize) -> usize {
        let mut n = 0;
        while n < limit {
            let Some((t, id)) = self.expiries.first().cloned() else {
                break;
            };
            if t > now {
                break;
            }
            self.remove_entry(&id);
            self.stats.expired += 1;
            self.tenant_mut(&id.scope.tenant).stats.expired += 1;
            n += 1;
        }
        n
    }

    fn over_limit(&self) -> bool {
        self.cfg.max_memory_bytes > 0 && self.used > self.cfg.max_memory_bytes
    }

    fn over_tenant_limit(&self, tenant: &str) -> bool {
        self.cfg.tenant_max_bytes > 0
            && self
                .tenants
                .get(tenant)
                .is_some_and(|t| t.used > self.cfg.tenant_max_bytes)
    }

    fn evictable(&self, tick: u64, id: &KeyId) -> bool {
        tick != self.tick
            && (self.cfg.eviction != EvictionPolicy::VolatileLru
                || self.map.get(id).is_some_and(|e| e.expires_at.is_some()))
    }

    fn evict(&mut self, id: &KeyId) {
        self.remove_entry(id);
        self.stats.evicted += 1;
        self.tenant_mut(&id.scope.tenant).stats.evicted += 1;
    }

    /// Evicts least recently used keys of `tenant` (only) until it is under
    /// its budget (FB-138): one tenant never evicts another's keys this way.
    fn evict_tenant_to_limit(&mut self, tenant: &str) {
        if self.cfg.eviction == EvictionPolicy::NoEviction {
            return;
        }
        while self.over_tenant_limit(tenant) {
            let start = (tenant.to_owned(), 0);
            let victim = self
                .tenant_lru
                .range(start..)
                .take_while(|(t, _)| t == tenant)
                .filter_map(|(_, tick)| self.lru.get(tick).map(|id| (*tick, id)))
                .find(|(tick, id)| self.evictable(*tick, id))
                .map(|(_, id)| id.clone());
            let Some(id) = victim else { break };
            self.evict(&id);
        }
    }

    /// Evicts least recently used keys until under the budgets: first those of
    /// `tenant` for the tenant budget, then any for the global one. The most
    /// recently used key is never evicted, so the write that just happened
    /// is not thrown away.
    pub(crate) fn evict_to_limit(&mut self, tenant: Option<&str>) {
        if let Some(t) = tenant {
            self.evict_tenant_to_limit(t);
        }
        if self.cfg.eviction == EvictionPolicy::NoEviction {
            return;
        }
        while self.over_limit() {
            let victim = self
                .lru
                .iter()
                .find(|(tick, id)| self.evictable(**tick, id))
                .map(|(_, id)| id.clone());
            let Some(id) = victim else { break };
            self.evict(&id);
        }
    }

    /// Called before a command of `tenant` that may grow memory: makes room,
    /// or refuses.
    pub(crate) fn make_room(&mut self, tenant: &str) -> KvResult<()> {
        if !self.over_limit() && !self.over_tenant_limit(tenant) {
            return Ok(());
        }
        self.evict_to_limit(Some(tenant));
        if (self.over_limit() || self.over_tenant_limit(tenant)) && self.cfg.eviction != EvictionPolicy::AllKeysLru {
            return Err(KvError::OutOfMemory);
        }
        Ok(())
    }

    // ---- persistence hooks ---------------------------------------------------

    /// Takes the set of keys changed since the last call, with their current
    /// state (`None` = deleted).
    pub(crate) fn take_dirty(&mut self) -> Vec<(KeyId, Option<Entry>)> {
        let ids: Vec<KeyId> = self.dirty.drain().collect();
        ids.into_iter()
            .map(|id| {
                let e = self.map.get(&id).cloned();
                (id, e)
            })
            .collect()
    }

    /// [`Store::take_dirty`] for one tenant's keys only.
    pub(crate) fn take_dirty_of(&mut self, tenant: &str) -> Vec<(KeyId, Option<Entry>)> {
        let ids: Vec<KeyId> = self.dirty.iter().filter(|k| k.scope.tenant == tenant).cloned().collect();
        ids.into_iter()
            .map(|id| {
                self.dirty.remove(&id);
                let e = self.map.get(&id).cloned();
                (id, e)
            })
            .collect()
    }

    /// Marks keys dirty again after a failed write.
    pub(crate) fn redirty(&mut self, ids: impl IntoIterator<Item = KeyId>) {
        self.dirty.extend(ids);
    }

    pub(crate) fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    /// Loads a persisted key (start-up). Expired keys are skipped; the keys
    /// are not marked dirty.
    pub(crate) fn load(&mut self, id: KeyId, data: Data, expires_at: Option<u64>, updated_at: u64, now: u64) {
        if expires_at.is_some_and(|t| t <= now) {
            self.dirty.insert(id);
            return;
        }
        self.insert_entry(id.clone(), data, expires_at, updated_at);
        self.dirty.remove(&id);
        self.evict_to_limit(Some(&id.scope.tenant));
    }
}
