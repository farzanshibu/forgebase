//! Job tenancy, visibility, reserved kinds and recorded invocation contexts
//! (FB-142, `rls-coverage.md` G3, G4, G5).
//!
//! * **Tenant.** Every job, schedule and workflow carries the tenant of the
//!   caller that created it (`NULL` = none). Schedule firings take the
//!   schedule's tenant, child jobs and compensations their parent's.
//! * **Visibility** ([`Scope`]). Privileged callers see every job. Others see
//!   only jobs of their own tenant (`NULL` for callers without one), and of
//!   those only the ones they created (or that a schedule they created fired)
//!   unless they hold [`READ_ALL_KEY`]. Every read, management call and job
//!   channel subscription applies it; a job outside it is "not found".
//! * **Kinds** ([`KindAccess`]). An in-process handler registers its kind as
//!   reserved (the default: only the system and privileged callers may
//!   enqueue it), public, or guarded (a [`EnqueueGuard`] checks the caller
//!   and the payload). Kinds of ForgeBase modules ([`RESERVED_PREFIXES`]) are
//!   reserved even when their module is off. Non-privileged workers never
//!   claim reserved or guarded kinds.
//! * **Recorded context** ([`RecordedContext`]). The enqueuer's identity
//!   (principal, realm, organization, tenant, scopes, impersonator) is stored
//!   with the job, so a handler that acts for the enqueuer
//!   ([`crate::JobContext::enqueuer`]) runs as that caller, never as the
//!   system. Roles and permissions are not stored: the policy engine resolves
//!   them again when the handler acts, so a revoked grant takes effect.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use forgebase_data::{AccessContext, DataError, Principal};
use forgebase_db::Value;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::JobError;

/// Permission key that lets a non-privileged caller see every job of its
/// tenant, not only the ones it created. `engine:read_jobs` does not grant it.
pub const READ_ALL_KEY: &str = "engine:read_jobs:all";

/// Kind prefixes of ForgeBase modules. Kinds starting with one are reserved
/// unless a handler registered them otherwise, so a module that is switched
/// off cannot be handed jobs that would run when it is switched on again.
pub const RESERVED_PREFIXES: [&str; 9] = [
    "function.",
    // FB-163: `graph.materialise` (the derived openCypher graph).
    "graph.",
    "storage.",
    "billing.",
    "credits.",
    "entitlements.",
    "customize.",
    "forgebase.",
    "system.",
];

/// Checks the enqueuer of a guarded kind: called with the caller and the
/// job's payload before a non-privileged caller's job, schedule or workflow
/// step of that kind is stored.
pub trait EnqueueGuard: Send + Sync + 'static {
    /// `Ok` admits the job; an error refuses it and is returned to the caller.
    fn admit(&self, ctx: &AccessContext, payload: &Json) -> Result<(), JobError>;
}

impl<F> EnqueueGuard for F
where
    F: Fn(&AccessContext, &Json) -> Result<(), JobError> + Send + Sync + 'static,
{
    fn admit(&self, ctx: &AccessContext, payload: &Json) -> Result<(), JobError> {
        self(ctx, payload)
    }
}

/// Who may enqueue a job kind.
#[derive(Clone)]
pub enum KindAccess {
    /// Only the system and privileged callers (administrators, admin service
    /// accounts). The default of [`crate::JobRegistry::register`].
    Reserved,
    /// Every caller allowed to enqueue jobs (`engine:enqueue_job`).
    Public,
    /// Callers allowed to enqueue jobs whom the guard admits.
    Guarded(Arc<dyn EnqueueGuard>),
}

impl fmt::Debug for KindAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KindAccess::Reserved => "Reserved",
            KindAccess::Public => "Public",
            KindAccess::Guarded(_) => "Guarded",
        })
    }
}

/// Answers whether a caller holds a permission key that is not a data-core
/// action ([`READ_ALL_KEY`]). The server wires the policy engine in.
pub type PermissionCheck = Arc<dyn Fn(&AccessContext, &str) -> bool + Send + Sync>;

/// Most job owners remembered for job channel checks.
const MAX_OWNERS: usize = 100_000;

/// Owner of a job, workflow or schedule: its tenant and creator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Owner {
    pub tenant: Option<String>,
    pub created_by: String,
}

#[derive(Default)]
struct Shared {
    permissions: RwLock<Option<PermissionCheck>>,
    kinds: RwLock<BTreeMap<String, KindAccess>>,
    /// Owners of jobs and workflows (channel checks are synchronous).
    owners: Mutex<(HashMap<String, Owner>, VecDeque<String>)>,
}

/// Job access state shared by a [`crate::JobService`] and the
/// [`crate::JobChannelAuthorizer`] of the same server: the permission check
/// for [`READ_ALL_KEY`], the access of registered kinds, and the owners of
/// recently seen jobs. Cheap to clone.
#[derive(Clone, Default)]
pub struct JobAccess {
    shared: Arc<Shared>,
}

impl fmt::Debug for JobAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobAccess")
            .field(
                "permissions",
                &self.shared.permissions.read().map(|p| p.is_some()).unwrap_or(false),
            )
            .field("kinds", &self.kinds())
            .finish_non_exhaustive()
    }
}

impl JobAccess {
    /// Fresh state: no permission check (only privileged callers see other
    /// callers' jobs), no registered kinds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers [`READ_ALL_KEY`] (and other plain keys) with `check` from now on.
    pub fn attach_permissions(&self, check: PermissionCheck) {
        *self.shared.permissions.write().unwrap_or_else(|p| p.into_inner()) = Some(check);
    }

    /// Whether `ctx` holds `key` (privileged callers hold every key).
    pub fn has(&self, ctx: &AccessContext, key: &str) -> bool {
        if ctx.is_privileged() {
            return true;
        }
        if ctx.principal == Principal::Anonymous {
            return false;
        }
        let check = self
            .shared
            .permissions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        check.is_some_and(|c| c(ctx, key))
    }

    /// Records the access of every kind of `kinds` (replacing earlier ones).
    pub fn set_kinds(&self, kinds: impl IntoIterator<Item = (String, KindAccess)>) {
        let mut map = self.shared.kinds.write().unwrap_or_else(|p| p.into_inner());
        map.extend(kinds);
    }

    pub(crate) fn kinds_raw(&self) -> Vec<(String, KindAccess)> {
        self.shared
            .kinds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(k, a)| (k.clone(), a.clone()))
            .collect()
    }

    /// Registered kinds with their access, sorted.
    pub fn kinds(&self) -> Vec<(String, String)> {
        self.shared
            .kinds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(k, a)| (k.clone(), format!("{a:?}")))
            .collect()
    }

    /// The access of `kind`: its registration, else reserved for a ForgeBase
    /// module prefix, else public (kinds of external workers).
    pub fn kind_access(&self, kind: &str) -> KindAccess {
        if let Some(a) = self.shared.kinds.read().unwrap_or_else(|p| p.into_inner()).get(kind) {
            return a.clone();
        }
        if RESERVED_PREFIXES.iter().any(|p| kind.starts_with(p)) {
            KindAccess::Reserved
        } else {
            KindAccess::Public
        }
    }

    /// Refuses a job of `kind` that `ctx` may not enqueue.
    pub fn admit(&self, ctx: &AccessContext, kind: &str, payload: &Json) -> Result<(), JobError> {
        if ctx.is_privileged() {
            return Ok(());
        }
        match self.kind_access(kind) {
            KindAccess::Public => Ok(()),
            KindAccess::Reserved => Err(JobError::Access(DataError::Forbidden(format!(
                "job kind {kind:?} is reserved for the system and administrators"
            )))),
            KindAccess::Guarded(g) => g.admit(ctx, payload),
        }
    }

    /// Kinds a non-privileged worker may not claim: registered reserved and
    /// guarded ones (the prefixes are excluded separately).
    pub(crate) fn unclaimable(&self) -> Vec<String> {
        self.shared
            .kinds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, a)| !matches!(a, KindAccess::Public))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Kinds with a reserved prefix that a registration made public.
    pub(crate) fn public_with_prefix(&self) -> Vec<String> {
        self.shared
            .kinds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(k, a)| matches!(a, KindAccess::Public) && RESERVED_PREFIXES.iter().any(|p| k.starts_with(p)))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Remembers the owner of job or workflow `id` for channel checks.
    pub(crate) fn remember(&self, id: &str, owner: Owner) {
        let mut g = self.shared.owners.lock().unwrap_or_else(|p| p.into_inner());
        let (map, order) = &mut *g;
        if map.insert(id.to_owned(), owner).is_none() {
            order.push_back(id.to_owned());
            while order.len() > MAX_OWNERS {
                if let Some(old) = order.pop_front() {
                    map.remove(&old);
                }
            }
        }
    }

    /// The remembered owner of job or workflow `id`.
    pub(crate) fn owner(&self, id: &str) -> Option<Owner> {
        self.shared
            .owners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .get(id)
            .cloned()
    }

    /// What `ctx` may see.
    pub(crate) fn scope(&self, ctx: &AccessContext) -> Scope {
        if ctx.is_privileged() {
            Scope::All
        } else if self.has(ctx, READ_ALL_KEY) {
            Scope::Tenant(ctx.tenant.clone())
        } else {
            Scope::Own {
                tenant: ctx.tenant.clone(),
                actor: ctx.actor(),
            }
        }
    }
}

/// The jobs a caller may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Privileged: everything.
    All,
    /// [`READ_ALL_KEY`]: every job of the tenant.
    Tenant(Option<String>),
    /// The caller's own jobs in its tenant.
    Own { tenant: Option<String>, actor: String },
}

impl Scope {
    fn tenant(&self) -> Option<Option<&str>> {
        match self {
            Scope::All => None,
            Scope::Tenant(t) | Scope::Own { tenant: t, .. } => Some(t.as_deref()),
        }
    }

    /// Whether an item of `owner` is visible. `schedule_creator` is the
    /// creator of the schedule that fired a job, if any.
    pub fn sees(&self, owner: &Owner, schedule_creator: Option<&str>) -> bool {
        if let Some(t) = self.tenant()
            && t != owner.tenant.as_deref()
        {
            return false;
        }
        match self {
            Scope::Own { actor, .. } => owner.created_by == *actor || schedule_creator == Some(actor.as_str()),
            _ => true,
        }
    }

    /// The SQL condition (`1 = 1` for everything) over the columns `tenant`,
    /// `created_by` (and `schedule_id` with `jobs`) of `alias`, with
    /// placeholders numbered after `params`.
    pub fn sql(&self, alias: &str, jobs: bool, params: &mut Vec<Value>) -> String {
        let a = if alias.is_empty() {
            String::new()
        } else {
            format!("{alias}.")
        };
        let mut sql = match self.tenant() {
            None => return "1 = 1".to_owned(),
            Some(None) => format!("{a}tenant IS NULL"),
            Some(Some(t)) => {
                params.push(t.into());
                format!("{a}tenant = ?{}", params.len())
            }
        };
        if let Scope::Own { actor, .. } = self {
            params.push(actor.as_str().into());
            let n = params.len();
            if jobs {
                sql.push_str(&format!(
                    " AND ({a}created_by = ?{n} OR {a}schedule_id IN \
                     (SELECT s.id FROM _forgebase_job_schedules s WHERE s.created_by = ?{n}))"
                ));
            } else {
                sql.push_str(&format!(" AND {a}created_by = ?{n}"));
            }
        }
        sql
    }
}

/// The identity a job was enqueued with, as stored in its `context` column.
/// The system is not recorded (`NULL`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedContext {
    /// `user`, `admin`, `service` or `anonymous`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Principal id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Auth collection of a user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// A service account with the admin role.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
    /// Realm.
    pub realm: String,
    /// Organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// API-key or PAT scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// Impersonating administrator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonator: Option<String>,
}

impl RecordedContext {
    /// The record of `ctx`; `None` for the system.
    pub fn of(ctx: &AccessContext) -> Option<Self> {
        let (kind, id, collection, admin) = match &ctx.principal {
            Principal::System => return None,
            Principal::Admin { id } => ("admin", Some(id.clone()), None, false),
            Principal::User { collection, id } => ("user", Some(id.clone()), Some(collection.clone()), false),
            Principal::Anonymous => ("anonymous", None, None, false),
            Principal::Service { id, admin } => ("service", Some(id.clone()), None, *admin),
            // A principal added later: recorded, but restored as none.
            _ => ("unknown", None, None, false),
        };
        Some(Self {
            kind: kind.to_owned(),
            id,
            collection,
            admin,
            realm: ctx.realm_id.clone(),
            org: ctx.org_id.clone(),
            tenant: ctx.tenant.clone(),
            scopes: ctx.scopes.clone(),
            impersonator: ctx.impersonator.clone(),
        })
    }

    /// The stored text of `ctx` (`None` for the system).
    pub(crate) fn encode(ctx: &AccessContext) -> Option<String> {
        Self::of(ctx).and_then(|r| serde_json::to_string(&r).ok())
    }

    /// Decodes a stored context; `None` for `NULL` (the system).
    pub(crate) fn decode(text: Option<&str>) -> Result<Option<Self>, JobError> {
        text.map(|t| {
            serde_json::from_str(t).map_err(|e| JobError::Internal(format!("stored job context is not valid: {e}")))
        })
        .transpose()
    }

    /// The access context (without roles and attributes, which the policy
    /// engine and the data core resolve again). `None` for an unknown type.
    pub fn context(&self) -> Option<AccessContext> {
        let id = || self.id.clone().unwrap_or_default();
        let mut ctx = match self.kind.as_str() {
            "admin" => AccessContext::admin(id()),
            "user" => AccessContext::user(self.collection.clone().unwrap_or_default(), id()),
            "service" => AccessContext::service(id(), self.admin),
            "anonymous" => AccessContext::anonymous(),
            _ => return None,
        }
        .with_realm(self.realm.clone());
        ctx.org_id.clone_from(&self.org);
        ctx.tenant.clone_from(&self.tenant);
        ctx.scopes.clone_from(&self.scopes);
        ctx.impersonator.clone_from(&self.impersonator);
        Some(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_context_round_trips() {
        let ctx = AccessContext::user("users", "u1")
            .with_realm("r1")
            .with_org("o1")
            .with_tenant("t1")
            .with_scopes(vec!["jobs:write".into()]);
        let text = RecordedContext::encode(&ctx).unwrap();
        let back = RecordedContext::decode(Some(&text))
            .unwrap()
            .unwrap()
            .context()
            .unwrap();
        assert_eq!(back, ctx);
        assert!(RecordedContext::encode(&AccessContext::system()).is_none());
        let sa = AccessContext::service("sa", true);
        let back = RecordedContext::decode(RecordedContext::encode(&sa).as_deref())
            .unwrap()
            .unwrap()
            .context()
            .unwrap();
        assert!(back.is_privileged());
    }

    #[test]
    fn scopes_filter_by_tenant_and_creator() {
        let own = Scope::Own {
            tenant: Some("t1".into()),
            actor: "user:users/a".into(),
        };
        let mine = Owner {
            tenant: Some("t1".into()),
            created_by: "user:users/a".into(),
        };
        let other = Owner {
            tenant: Some("t1".into()),
            created_by: "user:users/b".into(),
        };
        let foreign = Owner {
            tenant: Some("t2".into()),
            created_by: "user:users/a".into(),
        };
        assert!(own.sees(&mine, None));
        assert!(!own.sees(&other, None));
        assert!(own.sees(&other, Some("user:users/a")));
        assert!(!own.sees(&foreign, None));
        let all = Scope::Tenant(Some("t1".into()));
        assert!(all.sees(&other, None) && !all.sees(&foreign, None));
        assert!(Scope::All.sees(&foreign, None));
        let none = Scope::Tenant(None);
        assert!(!none.sees(&mine, None));
        let mut p = Vec::new();
        assert_eq!(none.sql("", true, &mut p), "tenant IS NULL");
        assert_eq!(own.sql("j", false, &mut p), "j.tenant = ?1 AND j.created_by = ?2");
    }

    #[test]
    fn module_kinds_are_reserved_by_default() {
        let a = JobAccess::new();
        let user = AccessContext::user("users", "u");
        assert!(a.admit(&user, "billing.maintenance", &Json::Null).is_err());
        assert!(a.admit(&user, "email", &Json::Null).is_ok());
        a.set_kinds([("email".to_owned(), KindAccess::Reserved)]);
        assert!(a.admit(&user, "email", &Json::Null).is_err());
        assert!(a.admit(&AccessContext::admin("x"), "email", &Json::Null).is_ok());
        a.set_kinds([("storage.public".to_owned(), KindAccess::Public)]);
        assert!(a.admit(&user, "storage.public", &Json::Null).is_ok());
        assert_eq!(a.public_with_prefix(), vec!["storage.public".to_owned()]);
    }
}
