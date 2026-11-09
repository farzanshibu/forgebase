//! ForgeBase's built-in key-value engine (FB-31, SPEC 4.4).
//!
//! * **Data types**: strings, hashes, lists, sets, sorted sets, streams.
//! * **TTL** with lazy and background expiry; **LRU eviction** under a memory
//!   budget ([`KvConfig`]).
//! * **Namespaces** inside **tenants**: every command runs in a scope taken
//!   from the caller's [`AccessContext`](forgebase_data::AccessContext), so a
//!   tenant can only ever reach its own keys.
//! * **Pub/sub** inside the process.
//! * **Persistence**: memory is authoritative; changed keys are written to
//!   SQLite in batches (`docs/adr/0004-kv.md`).
//! * A protocol-neutral **command layer** ([`KvService::exec`]): Redis-style
//!   commands as byte arguments, [`Reply`] values shaped like RESP replies, so a
//!   RESP listener (FB-32) maps straight onto it.
//! * A **REST API** under `/api/v1/kv` ([`http::router`]) with an OpenAPI
//!   fragment ([`http::openapi`]).
//!
//! Every operation asks the [`Authorizer`](forgebase_data::Authorizer) first
//! (`KvRead`, `KvWrite`, `KvAdmin`); the fail-closed default allows only
//! administrators and the system.
//!
//! This is not a Redis replacement: see the compatibility notes in the ADR.

pub mod commands;
pub mod config;
mod data;
pub mod error;
mod glob;
pub mod http;
mod persist;
pub mod pubsub;
pub mod reply;
mod service;
mod store;

pub use commands::{Class, Spec, spec};
pub use config::{EvictionPolicy, KvConfig};
pub use error::{KvError, KvResult};
pub use glob::matches as glob_matches;
pub use pubsub::{Message, Subscription};
pub use reply::Reply;
pub use service::{Clock, KvService, KvStats, NamespaceInfo, system_clock, validate_namespace};

#[cfg(test)]
mod tests;
