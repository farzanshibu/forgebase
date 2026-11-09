//! Channels and presence (rooms).
//!
//! A channel is a named pub/sub topic for messages that are not record
//! changes (chat, cursors, typing indicators). Messages are ephemeral:
//! delivered at most once to the connections subscribed at that moment, never
//! stored, never replayed. Presence is the set of connections that joined a
//! channel, with a small public `meta` object each; members of every instance
//! are tracked from the join/leave messages that cross the fan-out.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

/// A member of a channel's presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Member {
    /// Connection id (unique per instance).
    pub connection_id: String,
    /// Who: `user:<collection>/<id>`, `admin:<id>` or `anonymous`.
    pub actor: String,
    /// Public metadata set by the member.
    #[serde(default)]
    pub meta: Json,
    /// When the member joined (UTC).
    pub joined_at: String,
    /// Instance the connection is on.
    pub instance: String,
}

/// A channel message or presence change, local or from another instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ephemeral {
    /// Instance that produced it.
    pub origin: String,
    /// Channel name.
    pub channel: String,
    /// What happened.
    #[serde(flatten)]
    pub kind: EphemeralKind,
}

/// Kinds of [`Ephemeral`] messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EphemeralKind {
    /// A broadcast message.
    Message {
        /// Sender actor.
        from: String,
        /// Payload.
        data: Json,
        /// When (UTC).
        timestamp: String,
    },
    /// A member joined.
    Join {
        /// The member.
        member: Member,
    },
    /// A member left.
    Leave {
        /// The member.
        member: Member,
    },
}

/// Members of one channel by (instance, connection id).
type Room = BTreeMap<(String, String), Member>;

/// Presence of every channel.
#[derive(Debug, Default)]
pub(crate) struct Rooms {
    inner: Mutex<HashMap<String, Room>>,
}

impl Rooms {
    /// Adds or replaces a member; returns the channel's members afterwards.
    pub(crate) fn join(&self, channel: &str, member: Member) -> Vec<Member> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let room = g.entry(channel.to_owned()).or_default();
        room.insert((member.instance.clone(), member.connection_id.clone()), member);
        room.values().cloned().collect()
    }

    /// Removes a member; returns it if it was present.
    pub(crate) fn leave(&self, channel: &str, instance: &str, connection_id: &str) -> Option<Member> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let room = g.get_mut(channel)?;
        let m = room.remove(&(instance.to_owned(), connection_id.to_owned()));
        if room.is_empty() {
            g.remove(channel);
        }
        m
    }

    /// Current members of a channel.
    pub(crate) fn members(&self, channel: &str) -> Vec<Member> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.get(channel).map(|r| r.values().cloned().collect()).unwrap_or_default()
    }

    /// Applies a join/leave seen on the bus (from another instance).
    pub(crate) fn apply(&self, e: &Ephemeral) {
        match &e.kind {
            EphemeralKind::Join { member } => {
                self.join(&e.channel, member.clone());
            }
            EphemeralKind::Leave { member } => {
                self.leave(&e.channel, &member.instance, &member.connection_id);
            }
            EphemeralKind::Message { .. } => {}
        }
    }
}
