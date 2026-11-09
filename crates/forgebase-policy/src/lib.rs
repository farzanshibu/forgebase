//! ForgeBase authorization: policy engine, RBAC/ABAC and tenancy (FB-21,
//! ADR-0008).
//!
//! * [`PolicyService`] owns the authorization model of migration 0004 —
//!   realms, organizations, projects, environments (tenants), teams,
//!   memberships, roles, role sets, overrides, resource ACL, suspensions,
//!   collection rules, invitations — in an in-memory [`Snapshot`] that is
//!   replaced after every change (cache invalidation).
//! * [`PolicyAuthorizer`] implements the data core's `Authorizer` with the
//!   six-step [`pipeline`]; the trait is unchanged (one method with a
//!   default was added: `field_access`).
//! * [`rules`]: PocketBase-style rules compiled to parameterised SQL
//!   predicates, fail closed.
//! * [`http`]: `/api/v1` routes for policies and tenancy, and the tenancy
//!   layer that resolves `X-ForgeBase-Realm`, `X-ForgeBase-Org` and
//!   `X-ForgeBase-Tenant` into the request's `AccessContext`.

pub mod engine;
pub mod http;
pub mod perm;
pub mod pipeline;
pub mod rules;
pub mod snapshot;
pub mod store;

use std::sync::{Arc, OnceLock, RwLock};

use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::{AccessContext, Action, Authorizer, DataError, DataService, Decision, FieldAccess, Principal};
use forgebase_db::{Backend, DbError};
use forgebase_schema::Collection;

pub use engine::Check;
pub use pipeline::{
    AllowAll, AuditRecord, AuditSink, Denial, EntitlementHook, HookOutcome, HookRequest, Hooks, QuotaHook, ResourceRef,
    Step,
};
pub use rules::{Rule, RuleError};
pub use snapshot::Snapshot;

/// Unix seconds now.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Errors of the policy service.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// Storage failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// Data-core failure (schema lookups).
    #[error(transparent)]
    Data(#[from] DataError),
    /// A rule does not parse or does not fit the collection.
    #[error(transparent)]
    Rule(#[from] RuleError),
    /// The pipeline denied the operation.
    #[error("{0}")]
    Denied(Denial),
    /// Bad input.
    #[error("{message}")]
    Invalid {
        /// Stable code.
        code: &'static str,
        /// Explanation.
        message: String,
    },
    /// No such object (or not visible to the caller).
    #[error("{0} not found")]
    NotFound(String),
    /// Name already taken.
    #[error("{0} already exists")]
    Conflict(String),
    /// Bug or corrupt data.
    #[error("internal error: {0}")]
    Internal(String),
}

impl PolicyError {
    pub(crate) fn invalid(code: &'static str, message: impl Into<String>) -> Self {
        PolicyError::Invalid {
            code,
            message: message.into(),
        }
    }
}

impl From<serde_json::Error> for PolicyError {
    fn from(e: serde_json::Error) -> Self {
        PolicyError::Internal(format!("json: {e}"))
    }
}

impl ErrorCode for PolicyError {
    fn code(&self) -> &'static str {
        match self {
            PolicyError::Db(e) => e.code(),
            PolicyError::Data(e) => e.code(),
            PolicyError::Rule(_) => "invalid_rule",
            PolicyError::Denied(d) if d.step == Step::Authentication && d.reason == "authentication required" => {
                "unauthenticated"
            }
            PolicyError::Denied(_) => "forbidden",
            PolicyError::Invalid { code, .. } => code,
            PolicyError::NotFound(_) => "not_found",
            PolicyError::Conflict(_) => "already_exists",
            PolicyError::Internal(_) => "internal_error",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            PolicyError::Db(e) => e.kind(),
            PolicyError::Data(e) => e.kind(),
            PolicyError::Rule(_) | PolicyError::Invalid { .. } => ErrorKind::InvalidInput,
            PolicyError::Denied(d) if d.step == Step::Authentication && d.reason == "authentication required" => {
                ErrorKind::Unauthenticated
            }
            PolicyError::Denied(_) => ErrorKind::Forbidden,
            PolicyError::NotFound(_) => ErrorKind::NotFound,
            PolicyError::Conflict(_) => ErrorKind::Conflict,
            PolicyError::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// The policy service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PolicyService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    db: Arc<dyn Backend>,
    snap: RwLock<Arc<Snapshot>>,
    hooks: RwLock<Hooks>,
    audit: AuditSink,
    data: OnceLock<DataService>,
    mailer: OnceLock<(Arc<dyn forgebase_auth::Mailer>, String)>,
    realms: OnceLock<forgebase_auth::realm::RealmDirectory>,
}

impl PolicyService {
    /// Loads the model from a migrated database and starts the audit writer.
    pub async fn open(db: Arc<dyn Backend>) -> Result<Self, PolicyError> {
        let snap = Snapshot::load(db.as_ref()).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                audit: AuditSink::spawn(db.clone()),
                db,
                snap: RwLock::new(Arc::new(snap)),
                hooks: RwLock::new(Hooks::default()),
                data: OnceLock::new(),
                mailer: OnceLock::new(),
                realms: OnceLock::new(),
            }),
        })
    }

    /// Attaches the data service built with [`PolicyService::authorizer`]
    /// (needed to validate collection rules against schemas).
    pub fn attach_data(&self, data: DataService) {
        let _ = self.inner.data.set(data);
    }

    /// The attached data service.
    pub fn data(&self) -> Result<&DataService, PolicyError> {
        self.inner
            .data
            .get()
            .ok_or_else(|| PolicyError::Internal("data service not attached".into()))
    }

    /// Attaches the auth service's mailer and public URL for invitations.
    pub fn attach_mailer(&self, mailer: Arc<dyn forgebase_auth::Mailer>, public_url: impl Into<String>) {
        let _ = self.inner.mailer.set((mailer, public_url.into()));
    }

    /// Mails an invitation link (`<public_url>/auth/accept-invitation?token=`);
    /// failures are logged, the invitation stays valid.
    pub async fn mail_invitation(&self, email: &str, org: &str, token: &str) {
        let Some((mailer, url)) = self.inner.mailer.get() else {
            return;
        };
        let name = self
            .snapshot()
            .orgs
            .get(org)
            .map(|o| o.name.clone())
            .unwrap_or_default();
        let link = format!("{}/auth/accept-invitation?token={token}", url.trim_end_matches('/'));
        let mail = forgebase_auth::Mail {
            to: email.to_owned(),
            subject: format!("You are invited to {name}"),
            text: format!("You have been invited to join {name}. Accept the invitation: {link}\n"),
            kind: forgebase_auth::MailKind::Invitation,
            secret: token.to_owned(),
            link: Some(link),
        };
        if let Err(e) = mailer.send(&mail).await {
            tracing::warn!(error = %e, "invitation mail failed");
        }
    }

    /// Installs the entitlement (step 4) and quota (step 6) hooks (FB-24).
    pub fn set_hooks(&self, hooks: Hooks) {
        *self.inner.hooks.write().unwrap_or_else(|p| p.into_inner()) = hooks;
    }

    fn hooks(&self) -> Hooks {
        self.inner.hooks.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The storage backend.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.db
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner.snap.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Attaches the auth service's realm directory: users and service
    /// accounts then act in the realm of their identity (FB-25).
    pub async fn attach_realms(&self, dir: forgebase_auth::realm::RealmDirectory) -> Result<(), PolicyError> {
        let _ = self.inner.realms.set(dir);
        self.reload().await
    }

    /// Reloads the model (after any change; drops every cached permission).
    pub async fn reload(&self) -> Result<(), PolicyError> {
        let mut snap = Snapshot::load(self.inner.db.as_ref()).await?;
        snap.realm_dir = self.inner.realms.get().cloned();
        *self.inner.snap.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(snap);
        Ok(())
    }

    /// The data core's authorizer over this service.
    pub fn authorizer(&self) -> Arc<PolicyAuthorizer> {
        Arc::new(PolicyAuthorizer { svc: self.clone() })
    }

    /// Runs the pipeline for a protected operation that is not a record
    /// call; denials are audited.
    pub fn check(&self, ctx: &AccessContext, check: &Check) -> Result<(), PolicyError> {
        engine::run_check(&self.snapshot(), &self.hooks(), ctx, check).map_err(|d| {
            self.inner.audit.record(pipeline::audit_record(
                ctx,
                &check.permission,
                check.resource.as_ref().map(ToString::to_string),
                &d,
            ));
            PolicyError::Denied(d)
        })
    }

    /// Runs the pipeline without auditing (dry runs, UI hints).
    pub fn dry_run(&self, ctx: &AccessContext, check: &Check) -> Result<(), Denial> {
        engine::run_check(&self.snapshot(), &self.hooks(), ctx, check)
    }

    /// `has({permission})` for the caller in `org` (no audit).
    pub fn has(&self, ctx: &AccessContext, permission: &str, org: Option<&str>) -> bool {
        if ctx.is_privileged() {
            return true;
        }
        let snap = self.snapshot();
        engine::step_authentication(&snap, ctx, false).is_ok()
            && engine::step_tenant(&snap, ctx, org).is_ok()
            && snap
                .effective(&ctx.principal_key(), &ctx.realm_id, org)
                .perms
                .has(permission)
    }

    pub(crate) fn audit(&self) -> &AuditSink {
        &self.inner.audit
    }
}

/// The data core's [`Authorizer`]: the pipeline over the current snapshot.
#[derive(Debug, Clone)]
pub struct PolicyAuthorizer {
    svc: PolicyService,
}

impl Authorizer for PolicyAuthorizer {
    fn authorize(&self, ctx: &AccessContext, action: Action, collection: Option<&Collection>) -> Decision {
        if ctx.principal == Principal::System {
            return Decision::Allow;
        }
        match engine::decide(&self.svc.snapshot(), &self.svc.hooks(), ctx, action, collection) {
            Ok(d) => d,
            Err((denial, key)) => {
                self.svc.audit().record(pipeline::audit_record(
                    ctx,
                    &key,
                    collection.map(|c| format!("collection:{}", c.name)),
                    &denial,
                ));
                Decision::Deny
            }
        }
    }

    fn field_access(&self, ctx: &AccessContext, collection: &Collection) -> FieldAccess {
        engine::field_access(&self.svc.snapshot(), ctx, collection)
    }
}
