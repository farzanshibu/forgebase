//! The in-memory keyspace: entries, expiry index, LRU index, memory accounting.
//!
//! All tenants and namespaces live in one ordered map keyed by
//! `(tenant, namespace, key)`. A [`Scope`] (tenant + namespace) is part of every
//! key, so a command can only ever address keys of its own scope: there is no
//! API that takes a bare key.

use std::collections::{BTreeMap, BTreeSet, HashSet};
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
            self.tick += 1;
            e.tick = self.tick;
            self.lru.insert(self.tick, id.clone());
        }
    }

    /// Removes the key when its TTL has passed.
    fn expire_if_due(&mut self, id: &KeyId, now: u64) {
        if self.map.get(id).is_some_and(|e| e.expires_at.is_some_and(|t| t <= now)) {
            self.remove_entry(id);
            self.stats.expired += 1;
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
        if self.map.contains_key(id) {
            self.stats.hits += 1;
            self.touch(id);
        } else {
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
            n += 1;
        }
        n
    }

    fn over_limit(&self) -> bool {
        self.cfg.max_memory_bytes > 0 && self.used > self.cfg.max_memory_bytes
    }

    /// Evicts least recently used keys until under the budget. The most
    /// recently used key is never evicted, so the write that just happened
    /// is not thrown away.
    pub(crate) fn evict_to_limit(&mut self) {
        if self.cfg.eviction == EvictionPolicy::NoEviction {
            return;
        }
        while self.over_limit() {
            let volatile_only = self.cfg.eviction == EvictionPolicy::VolatileLru;
            let newest = self.tick;
            let victim = self
                .lru
                .iter()
                .find(|(tick, id)| {
                    **tick != newest && (!volatile_only || self.map.get(*id).is_some_and(|e| e.expires_at.is_some()))
                })
                .map(|(_, id)| id.clone());
            let Some(id) = victim else { break };
            self.remove_entry(&id);
            self.stats.evicted += 1;
        }
    }

    /// Called before a command that may grow memory: makes room, or refuses.
    pub(crate) fn make_room(&mut self) -> KvResult<()> {
        if !self.over_limit() {
            return Ok(());
        }
        self.evict_to_limit();
        if self.over_limit() && self.cfg.eviction != EvictionPolicy::AllKeysLru {
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
        self.evict_to_limit();
    }
}
