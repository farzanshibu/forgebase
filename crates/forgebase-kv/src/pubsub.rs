//! In-process publish/subscribe. Channels are scoped like keys: a message
//! published in one tenant's namespace is never seen by another's.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::broadcast;

use crate::error::{KvError, KvResult};
use crate::store::Scope;

/// A published message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Channel it was published on.
    pub channel: String,
    /// Payload.
    pub payload: Vec<u8>,
}

/// Receiving end of a subscription.
pub type Subscription = broadcast::Receiver<Message>;

#[derive(Debug)]
pub(crate) struct Hub {
    channels: Mutex<HashMap<(Scope, String), broadcast::Sender<Message>>>,
    capacity: usize,
    max_channels: usize,
}

impl Hub {
    pub(crate) fn new(capacity: usize, max_channels: usize) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            max_channels,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Scope, String), broadcast::Sender<Message>>> {
        self.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends to current subscribers; returns how many received it.
    pub(crate) fn publish(&self, scope: &Scope, channel: &str, payload: Vec<u8>) -> usize {
        let mut map = self.lock();
        let key = (scope.clone(), channel.to_owned());
        let Some(tx) = map.get(&key) else { return 0 };
        if tx.receiver_count() == 0 {
            map.remove(&key);
            return 0;
        }
        tx.send(Message {
            channel: channel.to_owned(),
            payload,
        })
        .unwrap_or(0)
    }

    pub(crate) fn subscribe(&self, scope: &Scope, channel: &str) -> KvResult<Subscription> {
        let mut map = self.lock();
        let key = (scope.clone(), channel.to_owned());
        if let Some(tx) = map.get(&key) {
            return Ok(tx.subscribe());
        }
        if map.len() >= self.max_channels {
            map.retain(|_, tx| tx.receiver_count() > 0);
            if map.len() >= self.max_channels {
                return Err(KvError::TooManyChannels);
            }
        }
        let (tx, rx) = broadcast::channel(self.capacity);
        map.insert(key, tx);
        Ok(rx)
    }

    pub(crate) fn channel_count(&self) -> usize {
        self.lock().len()
    }
}
