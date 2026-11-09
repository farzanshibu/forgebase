//! ForgeBase durable topics and queues (FB-63, ADR-0022).
//!
//! A message broker in the style of Google Pub/Sub, SQS + SNS or NATS
//! JetStream, stored in the system database:
//!
//! | Piece | Where |
//! |---|---|
//! | Topics with retention (time, size), message size limit, JSON Schema | [`QueueService::create_topic`] |
//! | Single and batch publish, attributes, ordering key, dedup window | [`QueueService::publish`] |
//! | Publish inside a caller's database transaction | [`QueueService::publish_in_tx`] |
//! | Record changes relayed from the FB-40 transactional outbox | [`RecordSource`], [`QueueService::relay_outbox`] |
//! | Subscriptions: fan-out (own cursor each), competing consumers inside one | [`QueueService::create_subscription`] |
//! | Attribute filters | [`filter`] |
//! | Ack deadline, ack, nack, deadline changes, backoff, max attempts, dead-letter topic | [`QueueService::ack`], [`QueueService::nack`], [`QueueService::modify_deadline`] |
//! | Ordering per key, one at a time | [`NewSubscription::ordering`] |
//! | REST long-poll pull; WebSocket and SSE streaming pull with flow control | [`QueueService::pull`], [`router`] |
//! | Push to signed HTTPS webhooks (SSRF-safe) and to server functions | [`push`] |
//! | Seek to a time or a snapshot | [`QueueService::seek`], [`QueueService::create_snapshot`] |
//! | Metrics (backlog, oldest unacked age, rates, dead letters) | [`QueueService::subscription_metrics`] |
//! | Metering (`queue.messages`, `queue.bytes`) and quotas (FB-24) | [`QueueService::attach_entitlements`] |
//!
//! Delivery is at least once: a message is redelivered after a missed ack
//! deadline, after a nack (with backoff), or after a crash (leases are rows,
//! so they expire across restarts). Every operation asks the data core's
//! authorizer first ([`forgebase_data::Action::QueueRead`],
//! [`forgebase_data::Action::QueuePublish`],
//! [`forgebase_data::Action::QueueConsume`],
//! [`forgebase_data::Action::QueueAdmin`]).

mod consume;
mod error;
pub mod filter;
mod http;
mod model;
mod ops;
pub mod push;
mod row;
pub mod schema;
mod service;

pub use error::QueueError;
pub use http::{StreamClientMessage, StreamServerMessage, api, extension, openapi, router};
pub use model::*;
pub use ops::QueueBacklog;
pub use service::QueueService;
