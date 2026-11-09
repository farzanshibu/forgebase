//! What the listener runs commands against.
//!
//! [`KvBackend`] is the seam between the wire protocol and a key-value store.
//! [`LocalKv`] is the built-in engine (`forgebase-kv`); with the
//! `external-redis` cargo feature, [`crate::external::ExternalRedis`] sends
//! the same commands to a Redis or Valkey server instead.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use forgebase_data::AccessContext;
use forgebase_kv::{KvResult, KvService, Reply, Subscription};

/// A boxed, sendable future (keeps the trait object safe without a macro crate).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A key-value store behind the listener. Every method authorises the caller
/// itself (the listener only adds API-key scope checks on top).
pub trait KvBackend: Send + Sync + fmt::Debug {
    /// Runs one command (`["SET", "k", "v"]`) in namespace `ns` of the
    /// caller's tenant. Only commands the engine knows are forwarded; the
    /// listener answers connection commands (`AUTH`, `SELECT`, ...) itself.
    fn exec<'a>(&'a self, ctx: &'a AccessContext, ns: &'a str, cmd: &'a [Vec<u8>]) -> BoxFuture<'a, KvResult<Reply>>;

    /// Subscribes to one channel of a namespace.
    fn subscribe(&self, ctx: &AccessContext, ns: &str, channel: &str) -> KvResult<Subscription>;

    /// Subscribes to every channel of a namespace (pattern subscriptions
    /// filter the messages).
    fn subscribe_all(&self, ctx: &AccessContext, ns: &str) -> KvResult<Subscription>;
}

/// The built-in engine.
#[derive(Debug, Clone)]
pub struct LocalKv(pub KvService);

impl KvBackend for LocalKv {
    fn exec<'a>(&'a self, ctx: &'a AccessContext, ns: &'a str, cmd: &'a [Vec<u8>]) -> BoxFuture<'a, KvResult<Reply>> {
        Box::pin(std::future::ready(self.0.exec(ctx, ns, cmd)))
    }

    fn subscribe(&self, ctx: &AccessContext, ns: &str, channel: &str) -> KvResult<Subscription> {
        self.0.subscribe(ctx, ns, channel)
    }

    fn subscribe_all(&self, ctx: &AccessContext, ns: &str) -> KvResult<Subscription> {
        self.0.subscribe_all(ctx, ns)
    }
}
