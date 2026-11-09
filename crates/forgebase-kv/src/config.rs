//! Engine settings.

use std::time::Duration;

/// What the engine does when memory use exceeds [`KvConfig::max_memory_bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvictionPolicy {
    /// Refuse writes that could grow memory (`oom` error) while over the limit.
    NoEviction,
    /// Evict the least recently used key of any kind.
    #[default]
    AllKeysLru,
    /// Evict the least recently used key that has a TTL; refuse writes when
    /// there is none.
    VolatileLru,
}

/// Settings of the KV engine.
#[derive(Debug, Clone)]
pub struct KvConfig {
    /// Memory budget in bytes for all tenants together; `0` means unlimited.
    /// The figure is an estimate of payload plus per-key overhead, not process RSS.
    pub max_memory_bytes: usize,
    /// What to do above the budget.
    pub eviction: EvictionPolicy,
    /// Largest key in bytes.
    pub max_key_bytes: usize,
    /// Largest single string value, hash/list/set member or stream field, in bytes.
    pub max_value_bytes: usize,
    /// `KEYS` refuses to answer when more keys match (use `SCAN`).
    pub keys_limit: usize,
    /// How often changed keys are written to SQLite. `Duration::ZERO` disables
    /// the background flusher (flush explicitly, e.g. on shutdown).
    pub flush_interval: Duration,
    /// How often expired keys are removed in the background.
    pub sweep_interval: Duration,
    /// Buffered messages per pub/sub channel before a slow subscriber lags.
    pub channel_capacity: usize,
    /// Most live pub/sub channels across all tenants.
    pub max_channels: usize,
}

impl Default for KvConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 256 * 1024 * 1024,
            eviction: EvictionPolicy::AllKeysLru,
            max_key_bytes: 512,
            max_value_bytes: 16 * 1024 * 1024,
            keys_limit: 10_000,
            flush_interval: Duration::from_secs(1),
            sweep_interval: Duration::from_secs(1),
            channel_capacity: 256,
            max_channels: 10_000,
        }
    }
}
