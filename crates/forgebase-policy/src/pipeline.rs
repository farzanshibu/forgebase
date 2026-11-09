//! The evaluation pipeline (ADR-0008 §8). Every protected operation — data
//! core calls through [`crate::PolicyAuthorizer`], tenancy and policy APIs,
//! engines through `Authorizer::authorize(.., None)` — runs the same steps in
//! the same order, and the first failing step denies:
//!
//! 1. **authentication** — anonymous callers where the operation needs a
//!    caller; suspended accounts everywhere.
//! 2. **tenant** — realm (cross-realm is denied to all but platform
//!    super-admins), active organization / tenant, active membership.
//! 3. **permission** — the caller's effective permissions; an explicit deny
//!    from any source wins over every grant.
//! 4. **entitlement** — [`EntitlementHook`] (FB-24; default: allow).
//! 5. **resource** — resource ACL (`resource_permissions`) and collection
//!    rules; deny wins here too.
//! 6. **quota** — [`QuotaHook`] (FB-24; default: allow).
//!
//! Denials are written to `_forgebase_authz_audit` with the step that failed.

use std::fmt;
use std::sync::Arc;

use forgebase_data::{AccessContext, Principal, SubjectRef};
use forgebase_db::{Backend, Statement};
use serde::Serialize;
use tokio::sync::mpsc;

/// A pipeline step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 1: who is calling, suspension.
    Authentication,
    /// 2: realm, organization, tenant.
    Tenant,
    /// 3: role and permission.
    Permission,
    /// 4: plan entitlement (hook).
    Entitlement,
    /// 5: resource ACL and record rules.
    Resource,
    /// 6: usage quota (hook).
    Quota,
}

impl Step {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Authentication => "authentication",
            Step::Tenant => "tenant",
            Step::Permission => "permission",
            Step::Entitlement => "entitlement",
            Step::Resource => "resource",
            Step::Quota => "quota",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A named resource (`project:<id>`, `record:<collection>/<id>`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceRef {
    /// Type.
    pub kind: String,
    /// Id.
    pub id: String,
}

impl ResourceRef {
    /// A resource.
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Denial {
    /// The step that failed.
    pub step: Step,
    /// Explanation (safe to show the caller).
    pub reason: String,
}

impl Denial {
    /// A denial at `step`.
    pub fn at(step: Step, reason: impl Into<String>) -> Self {
        Self {
            step,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Denial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "denied at {}: {}", self.step, self.reason)
    }
}

/// What a hook is asked about.
#[derive(Debug, Clone)]
pub struct HookRequest<'a> {
    /// The caller.
    pub ctx: &'a AccessContext,
    /// Who the check is billed and metered against: the organization the
    /// operation acts in (an org's tenant, an org route), else the caller's
    /// personal scope.
    pub subject: SubjectRef,
    /// The permission key being exercised (`data:posts:create`,
    /// `projects:create`, `engine:kv_write`, ...).
    pub permission: &'a str,
    /// The resource, when the operation names one.
    pub resource: Option<&'a ResourceRef>,
}

/// A hook's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Continue.
    Allow,
    /// Stop with this reason.
    Deny(String),
}

/// Step 4: plan entitlement ("is this feature in the subject's plan?").
///
/// Implemented by FB-24. Runs on the request path for every protected
/// operation, after the permission step and before resource checks, so it
/// must answer from memory (no blocking I/O); FB-24 keeps entitlements
/// cached and invalidates them itself. Methods added later get defaults.
pub trait EntitlementHook: Send + Sync + fmt::Debug {
    /// The entitlement decision. Default: allow.
    fn check(&self, req: &HookRequest<'_>) -> HookOutcome {
        let _ = req;
        HookOutcome::Allow
    }
}

/// Step 6: usage quota ("is there usage left?"). Entitlement is not quota:
/// both run.
///
/// Implemented by FB-24. `check` is the admission decision on the request
/// path (no blocking I/O); the atomic reservation, commit and release of
/// usage are FB-24's own API, called by the operation inside its transaction
/// (FB-24 may add them here as default methods).
pub trait QuotaHook: Send + Sync + fmt::Debug {
    /// The quota decision. Default: allow.
    fn check(&self, req: &HookRequest<'_>) -> HookOutcome {
        let _ = req;
        HookOutcome::Allow
    }
}

/// The default hooks: allow everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl EntitlementHook for AllowAll {}
impl QuotaHook for AllowAll {}

/// The two hook points.
#[derive(Debug, Clone)]
pub struct Hooks {
    /// Step 4.
    pub entitlement: Arc<dyn EntitlementHook>,
    /// Step 6.
    pub quota: Arc<dyn QuotaHook>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            entitlement: Arc::new(AllowAll),
            quota: Arc::new(AllowAll),
        }
    }
}

/// One audited denial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    /// Unix seconds.
    pub at: i64,
    /// Realm of the request.
    pub realm_id: String,
    /// Principal key.
    pub principal: String,
    /// Tenant.
    pub tenant: Option<String>,
    /// Active organization.
    pub org_id: Option<String>,
    /// Permission key.
    pub permission: String,
    /// Resource.
    pub resource: Option<String>,
    /// Failed step.
    pub step: Step,
    /// Reason.
    pub reason: String,
    /// Request id.
    pub request_id: Option<String>,
}

/// Writes audit records from synchronous code: a bounded channel drained by
/// a background task. When the channel is full a record is dropped with a
/// warning rather than blocking a request.
#[derive(Debug, Clone)]
pub struct AuditSink {
    tx: Option<mpsc::Sender<AuditRecord>>,
}

impl AuditSink {
    /// Starts the writer task (needs a Tokio runtime).
    pub fn spawn(db: Arc<dyn Backend>) -> Self {
        let (tx, mut rx) = mpsc::channel::<AuditRecord>(4096);
        tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while batch.len() < 256 {
                    match rx.try_recv() {
                        Ok(r) => batch.push(r),
                        Err(_) => break,
                    }
                }
                if let Err(e) = write_batch(db.as_ref(), &batch).await {
                    tracing::warn!(error = %e, dropped = batch.len(), "authorization audit write failed");
                }
            }
        });
        Self { tx: Some(tx) }
    }

    /// A sink that discards (tests of pure evaluation).
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Queues one record.
    pub fn record(&self, r: AuditRecord) {
        tracing::info!(step = %r.step, principal = %r.principal, permission = %r.permission, "authorization denied");
        if let Some(tx) = &self.tx
            && tx.try_send(r).is_err()
        {
            tracing::warn!("authorization audit queue full; record dropped");
        }
    }
}

async fn write_batch(db: &dyn Backend, batch: &[AuditRecord]) -> Result<(), forgebase_db::DbError> {
    let mut tx = db.begin(forgebase_db::TxMode::Normal).await?;
    for r in batch {
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_authz_audit (at, realm_id, principal, tenant, org_id, permission, resource, step, reason, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            vec![
                r.at.into(),
                r.realm_id.clone().into(),
                r.principal.clone().into(),
                r.tenant.clone().into(),
                r.org_id.clone().into(),
                r.permission.clone().into(),
                r.resource.clone().into(),
                r.step.as_str().into(),
                r.reason.clone().into(),
                r.request_id.clone().into(),
            ],
        ))
        .await?;
    }
    tx.commit().await
}

/// Builds the audit record of a denial.
pub(crate) fn audit_record(ctx: &AccessContext, permission: &str, resource: Option<String>, d: &Denial) -> AuditRecord {
    AuditRecord {
        at: crate::now(),
        realm_id: ctx.realm_id.clone(),
        principal: match ctx.principal {
            Principal::Anonymous => "anonymous".into(),
            _ => ctx.principal_key(),
        },
        tenant: ctx.tenant.clone(),
        org_id: ctx.org_id.clone(),
        permission: permission.to_owned(),
        resource,
        step: d.step,
        reason: d.reason.clone(),
        request_id: ctx.request_id.clone(),
    }
}
