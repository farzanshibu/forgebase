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
    /// One tap per scope that carries every message published in it (used by
    /// pattern subscriptions, which filter on the receiving side).
    taps: Mutex<HashMap<Scope, broadcast::Sender<Message>>>,
    capacity: usize,
    max_channels: usize,
}

impl Hub {
    pub(crate) fn new(capacity: usize, max_channels: usize) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            taps: Mutex::new(HashMap::new()),
            capacity: capacity.max(1),
            max_channels,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(Scope, String), broadcast::Sender<Message>>> {
        self.channels.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_taps(&self) -> std::sync::MutexGuard<'_, HashMap<Scope, broadcast::Sender<Message>>> {
        self.taps.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sends to current subscribers; returns how many received it on the
    /// exact channel (pattern taps are not counted).
    pub(crate) fn publish(&self, scope: &Scope, channel: &str, payload: Vec<u8>) -> usize {
        {
            let mut taps = self.lock_taps();
            if let Some(tx) = taps.get(scope) {
                if tx.receiver_count() == 0 {
                    taps.remove(scope);
                } else {
                    let _ = tx.send(Message {
                        channel: channel.to_owned(),
                        payload: payload.clone(),
                    });
                }
            }
        }
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

    /// Receives every message published in `scope`, whatever the channel.
    pub(crate) fn subscribe_all(&self, scope: &Scope) -> KvResult<Subscription> {
        let mut taps = self.lock_taps();
        if let Some(tx) = taps.get(scope) {
            return Ok(tx.subscribe());
        }
        if taps.len() >= self.max_channels {
            taps.retain(|_, tx| tx.receiver_count() > 0);
            if taps.len() >= self.max_channels {
                return Err(KvError::TooManyChannels);
            }
        }
        let (tx, rx) = broadcast::channel(self.capacity);
        taps.insert(scope.clone(), tx);
        Ok(rx)
    }

    pub(crate) fn channel_count(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn scope(t: &str) -> Scope {
        Scope {
            tenant: t.into(),
            ns: "n".into(),
        }
    }

    #[tokio::test]
    async fn a_scope_tap_sees_every_channel_of_its_scope_only() {
        let hub = Hub::new(8, 8);
        let mut tap = hub.subscribe_all(&scope("t:a")).unwrap();
        let mut other = hub.subscribe_all(&scope("t:b")).unwrap();
        assert_eq!(hub.publish(&scope("t:a"), "news.1", b"x".to_vec()), 0);
        assert_eq!(hub.publish(&scope("t:a"), "news.2", b"y".to_vec()), 0);
        assert_eq!(tap.recv().await.unwrap().channel, "news.1");
        assert_eq!(tap.recv().await.unwrap().payload, b"y");
        assert!(other.try_recv().is_err());
    }

    #[tokio::test]
    async fn taps_are_bounded_and_dropped_taps_are_reclaimed() {
        let hub = Hub::new(8, 1);
        let rx = hub.subscribe_all(&scope("t:a")).unwrap();
        assert!(matches!(
            hub.subscribe_all(&scope("t:b")),
            Err(KvError::TooManyChannels)
        ));
        drop(rx);
        assert!(hub.subscribe_all(&scope("t:b")).is_ok());
    }
}
