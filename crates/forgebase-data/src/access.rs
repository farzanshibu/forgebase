//! Access context and the authorisation hook (ADR-0003 §7).
//!
//! Every service call takes an [`AccessContext`]. Before touching data the
//! service asks the [`Authorizer`] for a [`Decision`]; a `Decision::Filter`
//! predicate is compiled into the same SQL statement as the caller's filter, so
//! rows the caller may not see are never read. FB-21 plugs its policy engine
//! in here without changing any service signature.
//!
//! The default, [`DefaultAuthorizer`], fails closed: only the system and
//! administrators are allowed anything.

use std::collections::HashMap;
use std::fmt;

use forgebase_query::Expr;
use forgebase_schema::Collection;
use serde_json::Value as Json;

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Principal {
    /// ForgeBase itself (migrations, internal jobs). Bypasses authorisation and
    /// may change system collections.
    System,
    /// An authenticated administrator.
    Admin {
        /// Administrator id.
        id: String,
    },
    /// An authenticated user record of an auth collection.
    User {
        /// Auth collection name.
        collection: String,
        /// Record id.
        id: String,
    },
    /// No credentials.
    Anonymous,
}

/// Identity and request metadata passed to every service call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessContext {
    /// Caller.
    pub principal: Principal,
    /// Tenant the request is scoped to (multi-tenancy, FB-21).
    pub tenant: Option<String>,
    /// Request id, for audit and logs.
    pub request_id: Option<String>,
}

impl AccessContext {
    /// The system principal.
    pub fn system() -> Self {
        Self::of(Principal::System)
    }

    /// An administrator.
    pub fn admin(id: impl Into<String>) -> Self {
        Self::of(Principal::Admin { id: id.into() })
    }

    /// A user record.
    pub fn user(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self::of(Principal::User {
            collection: collection.into(),
            id: id.into(),
        })
    }

    /// No credentials.
    pub fn anonymous() -> Self {
        Self::of(Principal::Anonymous)
    }

    fn of(principal: Principal) -> Self {
        Self {
            principal,
            tenant: None,
            request_id: None,
        }
    }

    /// Same context with a request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// System or administrator: may see hidden fields and use admin tools.
    pub fn is_privileged(&self) -> bool {
        matches!(self.principal, Principal::System | Principal::Admin { .. })
    }

    /// The system principal.
    pub fn is_system(&self) -> bool {
        self.principal == Principal::System
    }

    /// Short actor string recorded in history tables.
    pub fn actor(&self) -> String {
        match &self.principal {
            Principal::System => "system".into(),
            Principal::Admin { id } => format!("admin:{id}"),
            Principal::User { collection, id } => format!("user:{collection}/{id}"),
            Principal::Anonymous => "anonymous".into(),
        }
    }
}

/// What the caller wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Read collection definitions, indexes, plans and history.
    ReadSchema,
    /// Create, alter or drop collections.
    ManageSchema,
    /// Create, drop or rebuild indexes; change the slow-query threshold.
    ManageIndexes,
    /// List records.
    List,
    /// Read one record.
    View,
    /// Create a record.
    Create,
    /// Update a record.
    Update,
    /// Delete a record.
    Delete,
    /// Run read-only SQL.
    ReadSql,
    /// Run SQL that writes or changes the schema.
    WriteSql,
    /// Read jobs, job history, schedules, workers and queue statistics (FB-61).
    ReadJobs,
    /// Enqueue a background job (FB-61).
    EnqueueJob,
    /// Cancel, retry or delete jobs; create, change or delete schedules (FB-61).
    ManageJobs,
    /// Act as a job worker: register, claim, heartbeat, complete, fail (FB-61).
    WorkJobs,
}

/// A row-level predicate in the filter language, with its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    /// Expression evaluated against the collection's rows.
    pub filter: Expr,
    /// Values for `{:name}` parameters in `filter`.
    pub params: HashMap<String, Json>,
}

impl Predicate {
    /// Parses a predicate from filter syntax.
    pub fn parse(filter: &str, params: HashMap<String, Json>) -> Result<Self, forgebase_query::QueryError> {
        Ok(Self {
            filter: forgebase_query::parse_filter(filter)?.unwrap_or(Expr::Const(true)),
            params,
        })
    }
}

/// Outcome of an authorisation check.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Decision {
    /// Allowed without restriction.
    Allow,
    /// Allowed only for rows matching the predicate. For `Create` and
    /// `Update` the predicate is also checked on the written row before
    /// commit.
    Filter(Predicate),
    /// Not allowed.
    Deny,
}

/// Decides what a caller may do. Called once per service call.
pub trait Authorizer: Send + Sync + fmt::Debug {
    /// The decision for `action` on `collection` (`None` for actions that are
    /// not about one collection, e.g. SQL and index-wide operations).
    fn authorize(&self, ctx: &AccessContext, action: Action, collection: Option<&Collection>) -> Decision;
}

/// Fail-closed default: the system and administrators may do everything,
/// everybody else nothing. FB-21 replaces it with the policy engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultAuthorizer;

impl Authorizer for DefaultAuthorizer {
    fn authorize(&self, ctx: &AccessContext, _action: Action, _collection: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

/// Hashes secrets (password fields). Provided by the auth package (FB-20,
/// Argon2id). Without one, writing a password field fails closed.
pub trait SecretHasher: Send + Sync + fmt::Debug {
    /// Returns the encoded hash of `secret`.
    fn hash(&self, secret: &str) -> Result<String, String>;
}
