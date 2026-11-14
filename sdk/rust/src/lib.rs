//! # ForgeBase Rust SDK
//!
//! An async client for [ForgeBase](https://github.com/farzanshibu/forgebase) built on `reqwest` and
//! `rustls` (no OpenSSL). It covers auth, collections and records, realtime subscriptions
//! (WebSocket with SSE fallback), object storage, key-value, server functions, jobs, queues and
//! webhook tooling, billing and entitlements (including their admin sides), credits, vector search,
//! the property graph with openCypher, the SQL editor, indexes, documents, access control
//! (roles, rules, ACLs), organizations and teams, identity administration, customization, alerts
//! and SLOs, operations insights, instance administration and webhook signature verification.
//!
//! ```no_run
//! use forgebase_sdk::{ForgeBase, SignInOptions};
//! use serde_json::json;
//!
//! # async fn run() -> forgebase_sdk::Result<()> {
//! let fb = ForgeBase::new("http://localhost:8090")?;
//! fb.auth().sign_in("me@example.com", "a long passphrase", &SignInOptions::default()).await?;
//!
//! let posts = fb.collection("posts");
//! let created = posts.create(&json!({"title": "hello"})).await?;
//! let page = posts.list(&forgebase_sdk::ListOptions::new().filter(r#"title = "hello""#)).await?;
//! assert!(!page.items.is_empty());
//! # let _ = created; Ok(()) }
//! ```
//!
//! Failures are [`Error`]s with the server's error code, status and request id. Transient failures
//! are retried with exponential backoff ([`RetryPolicy`]); a rotated or expired access token is
//! refreshed transparently.
//!
//! Wire models in [`generated`] come from the server's OpenAPI document (see
//! `scripts/generate_models.py`); the request/response ergonomics are hand-written.

pub mod access;
pub mod alerts;
pub mod auth;
pub mod billing;
pub mod billing_admin;
pub mod client;
pub mod consistency;
pub mod credits;
pub mod customize;
pub mod documents;
pub mod entitlements;
pub mod entitlements_admin;
pub mod error;
pub mod functions;
pub mod generated;
pub mod graph;
mod http;
pub mod identity;
pub mod indexes;
pub mod insights;
pub mod jobs;
pub mod kv;
pub mod orgs;
pub mod queue_webhooks;
pub mod queues;
pub mod realtime;
pub mod records;
pub mod sql;
pub mod storage;
pub mod system;
pub mod vector;
pub mod webhooks;

pub use access::AccessApi;
pub use alerts::{AlertsApi, SlosApi};
pub use auth::{AuthApi, FileStore, MemoryStore, MfaChallenge, Session, SessionStore, SignInOptions, SignInResult};
pub use billing::{
    BillingApi, BillingStoreApi, HostedCheckout, StoreCheckout, StoreProvider, StorePurchase, StoreReceipt,
};
pub use client::{ClientBuilder, ForgeBase};
pub use consistency::{CommitTokens, Consistency};
pub use credits::CreditsApi;
pub use customize::CustomizeApi;
pub use documents::DocumentsApi;
pub use entitlements::{EntitlementsApi, HasQuery, MeteredRequest, QuotaRequest};
pub use error::{ApiError, Error, ErrorKind, Result};
pub use functions::FunctionsApi;
pub use graph::GraphApi;
pub use http::RetryPolicy;
pub use identity::IdentityApi;
pub use indexes::IndexesApi;
pub use insights::InsightsApi;
pub use jobs::{JobWatch, JobsApi, ListJobs};
pub use kv::{Kv, KvValue, SetOptions};
pub use orgs::OrgsApi;
pub use queue_webhooks::QueueWebhooksApi;
pub use queues::{
    AckResult, MessagePage, NewMessage, NewSubscription, NewTopic, Published, PullOptions, QueueMessage, QueueSnapshot,
    QueueStream, QueueSubscription, QueuesApi, ReceivedMessage, SeekResult, SeekTo, StreamOptions, Topic,
};
pub use realtime::{
    ChannelMessage, PresenceEvent, PresenceMember, Realtime, RealtimeConfig, RealtimeEvent, RealtimeTransport,
    RecordEvent, ResetEvent, SubscribeOptions, Subscription,
};
pub use records::{CollectionsApi, ListOptions, ListResult, Records, TransferFormat, WriteOptions};
pub use sql::SqlApi;
pub use storage::{ListObjects, ObjectStream, PutOptions, StorageApi};
pub use system::SystemApi;
pub use vector::VectorApi;
