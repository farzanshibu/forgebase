//! The message protocol (ADR-0011 §5).
//!
//! WebSocket: one JSON object per text frame in both directions. SSE: the
//! same server messages, one per `event:`/`data:` block (`id:` = cursor for
//! record events). Unknown fields are ignored (forward compatibility).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

/// WebSocket close codes used by the server.
pub mod close {
    /// Normal closure.
    pub const NORMAL: u16 = 1000;
    /// The server is shutting down.
    pub const GOING_AWAY: u16 = 1001;
    /// The client sent a binary frame.
    pub const UNSUPPORTED_DATA: u16 = 1003;
    /// A message larger than the limit.
    pub const MESSAGE_TOO_BIG: u16 = 1009;
    /// Server error.
    pub const INTERNAL: u16 = 1011;
    /// The credential was revoked, expired or no longer valid.
    pub const UNAUTHENTICATED: u16 = 4001;
    /// No message or pong within the idle timeout.
    pub const HEARTBEAT_TIMEOUT: u16 = 4002;
    /// The client did not read fast enough: its queue stayed full for longer
    /// than the send timeout.
    pub const SLOW_CONSUMER: u16 = 4008;
    /// Repeated protocol violations.
    pub const PROTOCOL_ERROR: u16 = 4400;
}

/// What a subscription listens to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SubscribeSpec {
    /// Client-chosen subscription id (1-64 characters of `[A-Za-z0-9_.:-]`),
    /// echoed in every message of this subscription.
    pub id: String,
    /// Collection name: changes of its records (with `record`: of one record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Record id: only changes of this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// Filter-language expression the changed record must match
    /// (collection subscriptions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Values for `{:name}` parameters in `filter`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub params: HashMap<String, Json>,
    /// Custom channel name (instead of `collection`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Replay every event after this cursor before live events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
}

/// A client message (WebSocket).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// Start a subscription.
    #[serde(rename = "subscribe")]
    Subscribe {
        /// What to subscribe to.
        #[serde(flatten)]
        spec: SubscribeSpec,
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// End a subscription.
    #[serde(rename = "unsubscribe")]
    Unsubscribe {
        /// Subscription id.
        id: String,
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// Broadcast to a channel.
    #[serde(rename = "publish")]
    Publish {
        /// Channel name.
        channel: String,
        /// Payload.
        #[serde(default)]
        data: Json,
        /// Echoed in the ack.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// Join a channel's presence (needs a subscription to the channel).
    #[serde(rename = "presence.join")]
    PresenceJoin {
        /// Channel name.
        channel: String,
        /// Public metadata shown to other members (at most 1 KiB of JSON).
        #[serde(default)]
        meta: Json,
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// Leave a channel's presence.
    #[serde(rename = "presence.leave")]
    PresenceLeave {
        /// Channel name.
        channel: String,
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// Application-level heartbeat; answered with `pong`.
    #[serde(rename = "ping")]
    Ping {
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
    /// Replace the connection's credential (e.g. a refreshed access token).
    #[serde(rename = "auth")]
    Auth {
        /// Access token, API key or personal access token.
        token: String,
        /// Echoed in the reply.
        #[serde(default, rename = "ref")]
        reference: Option<Json>,
    },
}

/// Longest subscription id.
pub const MAX_SUBSCRIPTION_ID: usize = 64;
/// Longest channel name.
pub const MAX_CHANNEL: usize = 128;

fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// Whether `s` is a valid subscription id.
pub fn valid_subscription_id(s: &str) -> bool {
    token_ok(s, MAX_SUBSCRIPTION_ID)
}

/// Whether `s` is a valid channel name (`[A-Za-z0-9_.:-]`, 1-128).
pub fn valid_channel(s: &str) -> bool {
    token_ok(s, MAX_CHANNEL)
}
