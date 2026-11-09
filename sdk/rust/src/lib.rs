//! # ForgeBase Rust SDK
//!
//! An async client for [ForgeBase](https://github.com/forgebase/forgebase) built on `reqwest` and
//! `rustls` (no OpenSSL). It covers auth, collections and records, realtime subscriptions
//! (WebSocket with SSE fallback), object storage, key-value, server functions, jobs and entitlements.
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

pub mod auth;
pub mod client;
pub mod entitlements;
pub mod error;
pub mod functions;
pub mod generated;
mod http;
pub mod jobs;
pub mod kv;
pub mod realtime;
pub mod records;
pub mod storage;

pub use auth::{AuthApi, FileStore, MemoryStore, MfaChallenge, Session, SessionStore, SignInOptions, SignInResult};
pub use client::{ClientBuilder, ForgeBase};
pub use entitlements::{EntitlementsApi, HasQuery, MeteredRequest, QuotaRequest};
pub use error::{ApiError, Error, ErrorKind, Result};
pub use functions::FunctionsApi;
pub use http::RetryPolicy;
pub use jobs::{JobsApi, ListJobs};
pub use kv::{Kv, KvValue, SetOptions};
pub use realtime::{
    ChannelMessage, PresenceEvent, PresenceMember, Realtime, RealtimeConfig, RealtimeEvent, RealtimeTransport,
    RecordEvent, ResetEvent, SubscribeOptions, Subscription,
};
pub use records::{CollectionsApi, ListOptions, ListResult, Records, TransferFormat, WriteOptions};
pub use storage::{DeleteOutcome, ListObjects, ObjectStream, PutOptions, StorageApi};
