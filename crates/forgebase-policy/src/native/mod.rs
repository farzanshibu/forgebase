//! Native PostgreSQL row-level security (FB-152, rls-coverage §3,
//! ADR-0028 decision 12).
//!
//! ForgeBase rules stay the single source of truth; on PostgreSQL they are
//! also compiled to policies on every collection table:
//!
//! * [`claims`]: who the caller is, computed in Rust from the policy
//!   snapshot (`forgebase.claims()` reads it back);
//! * [`generate`]: the policies, grants and `api` view of one collection,
//!   as SQL statements; [`apply`] runs them in the transaction of the node
//!   to write to (FB-173 applies them on every shard);
//! * the data-core hook ([`forgebase_data::NativePolicies`]) regenerates a
//!   collection inside every schema transaction; rule changes regenerate in
//!   their own transaction ([`PolicyService::put_policy`]);
//! * modes ([`NativeMode`]): `off` (RLS forced, no policies), `logins`
//!   (policies for direct logins; the API path runs as the server) and
//!   `enforce` (non-privileged API requests also run as `forgebase_rls`
//!   with their claims, [`forgebase_data::Authorizer::native_session`]).

mod claims;
mod fanout;
mod generate;
mod render;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use forgebase_data::{AccessContext, Action, DataError, NativePolicies};
use forgebase_db::{BoxFuture, RunAs, Statement, TxMode, WriteTx};
use forgebase_schema::Collection;

pub use claims::{CLAIMS_VERSION, claims};
pub use fanout::{LoginRole, copy_login_role, create_login_role, drop_login_role, login_role_verifier};
pub use generate::{Generated, apply, drop_objects, drop_unused_relations, generate, has_policies};

use crate::{PolicyError, PolicyService};

/// The role non-privileged requests run as in `enforce` mode.
pub const RLS_ROLE: &str = "forgebase_rls";
/// The NOLOGIN group of named direct logins (FB-153); `api` views are
/// granted to it.
pub const LOGIN_GROUP: &str = "forgebase_login";
/// `_forgebase_meta` key of the generated statements' fingerprint.
pub const FINGERPRINT_KEY: &str = "native_rls.fingerprint";

/// `[database.postgres] native_rls` (rls-coverage §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NativeMode {
    /// Row-level security forced on every collection table, no policies:
    /// only administrator logins see data directly.
    Off,
    /// Policies installed for direct logins; the API path runs as the server.
    #[default]
    Logins,
    /// Policies installed and non-privileged API requests run under them too.
    Enforce,
}

impl NativeMode {
    /// Parses `off`, `logins` or `enforce`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "logins" => Some(Self::Logins),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }

    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Logins => "logins",
            Self::Enforce => "enforce",
        }
    }
}

/// Native row-level security state of a [`PolicyService`].
#[derive(Debug, Default)]
pub(crate) struct NativeState {
    mode: OnceLock<NativeMode>,
    /// Collection id -> its rules all compiled natively (from the latest
    /// generation). Unknown collections are not enforced natively.
    complete: Mutex<HashMap<String, bool>>,
}

impl NativeState {
    fn record(&self, coll: &Collection, g: &Generated) {
        if !g.problems.is_empty() {
            tracing::warn!(collection = %coll.name, problems = ?g.problems,
                "rules not compiled to native policies: closed for direct logins, checked by ForgeBase only");
        }
        self.complete
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(coll.id.clone(), g.complete);
    }
}

/// The data-core hook.
#[derive(Debug)]
struct Hook {
    svc: Weak<crate::Inner>,
    mode: NativeMode,
}

impl Hook {
    fn service(&self) -> Result<PolicyService, DataError> {
        self.svc
            .upgrade()
            .map(|inner| PolicyService { inner })
            .ok_or_else(|| DataError::Internal("policy service is gone".into()))
    }
}

impl NativePolicies for Hook {
    fn before_change<'a>(
        &'a self,
        tx: &'a mut dyn WriteTx,
        coll: &'a Collection,
    ) -> BoxFuture<'a, Result<(), DataError>> {
        Box::pin(async move { Ok(apply(tx, &drop_objects(coll)).await?) })
    }

    fn after_change<'a>(
        &'a self,
        tx: &'a mut dyn WriteTx,
        coll: &'a Collection,
        catalog: &'a [Collection],
    ) -> BoxFuture<'a, Result<(), DataError>> {
        Box::pin(async move {
            let svc = self.service()?;
            let snap = svc.snapshot();
            // The collection and those whose relation paths may read it.
            let affected = catalog.iter().filter(|c| {
                c.id == coll.id
                    || c.fields
                        .iter()
                        .any(|f| f.kind.relation_target() == Some(coll.id.as_str()))
            });
            for c in affected {
                let g = generate(c, snap.policies.get(&c.id), catalog, self.mode);
                apply(tx, &g.statements).await?;
                svc.inner.native.record(c, &g);
            }
            Ok(())
        })
    }
}

fn fingerprint(statements: &[String]) -> String {
    let parts: Vec<&str> = statements.iter().map(String::as_str).collect();
    render::short_hash(&parts)
}

impl PolicyService {
    /// Turns native row-level security on (PostgreSQL only; once, at
    /// startup, after migrations and [`PolicyService::attach_data`]):
    /// installs the data-core hook and regenerates every collection in one
    /// transaction (skipped when nothing changed since the last start).
    pub async fn enable_native_rls(&self, mode: NativeMode) -> Result<(), PolicyError> {
        let data = self.data()?;
        if self.backend().dialect().name() != "postgres" {
            return Err(PolicyError::invalid(
                "native_rls_unavailable",
                "native row-level security needs the PostgreSQL backend",
            ));
        }
        let role = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = ?1",
                vec![RLS_ROLE.into()],
            ))
            .await?;
        if role.is_empty() {
            return Err(PolicyError::invalid(
                "native_rls_unavailable",
                format!(
                    "role {RLS_ROLE} does not exist (the bundled engine creates it; external servers need it created)"
                ),
            ));
        }
        if self.inner.native.mode.set(mode).is_err() {
            return Err(PolicyError::invalid(
                "native_rls_enabled",
                "native row-level security is already on",
            ));
        }
        if !data.set_native_policies(Arc::new(Hook {
            svc: Arc::downgrade(&self.inner),
            mode,
        })) {
            return Err(PolicyError::Internal(
                "a native policy hook is already installed".into(),
            ));
        }
        self.sync_native_rls().await
    }

    /// The native row-level security mode, once enabled.
    pub fn native_rls_mode(&self) -> Option<NativeMode> {
        self.inner.native.mode.get().copied()
    }

    /// Regenerates every collection's native policies in one transaction.
    pub async fn sync_native_rls(&self) -> Result<(), PolicyError> {
        let Some(mode) = self.native_rls_mode() else {
            return Ok(());
        };
        let data = self.data()?;
        let catalog = data.list_collections(&AccessContext::system()).await?;
        let snap = self.snapshot();
        let generated: Vec<(Collection, Generated)> = catalog
            .iter()
            .filter(|c| has_policies(c))
            .map(|c| (c.clone(), generate(c, snap.policies.get(&c.id), &catalog, mode)))
            .collect();
        let all: Vec<String> = std::iter::once(format!("mode={}", mode.as_str()))
            .chain(generated.iter().flat_map(|(_, g)| g.statements.iter().cloned()))
            .collect();
        let print = fingerprint(&all);
        for (c, g) in &generated {
            self.inner.native.record(c, g);
        }
        // Control, then every shard (FB-173): each set keeps its own
        // fingerprint and is brought up to date on its own.
        self.sync_native_on(self.backend(), &generated, &print).await?;
        for (set, db) in self.shard_databases() {
            if let Err(e) = self.sync_native_on(&db, &generated, &print).await {
                tracing::warn!(set = %set, error = %e, "could not bring native policies up to date on a shard");
                return Err(e);
            }
        }
        Ok(())
    }

    /// Regenerates one collection inside `tx` with `policy` (a rule change
    /// before the snapshot reload); returns the statements (FB-173 runs them
    /// on every shard too).
    pub(crate) async fn regenerate_in(
        &self,
        tx: &mut dyn WriteTx,
        coll: &Collection,
        policy: Option<&crate::snapshot::CollectionPolicy>,
    ) -> Result<Vec<String>, PolicyError> {
        let Some(mode) = self.native_rls_mode() else {
            return Ok(Vec::new());
        };
        let catalog = self.data()?.list_collections(&AccessContext::system()).await?;
        let g = generate(coll, policy, &catalog, mode);
        apply(tx, &g.statements).await?;
        self.inner.native.record(coll, &g);
        Ok(g.statements)
    }

    /// Who `ctx` runs as for `action` on `coll` (`enforce` mode only, and
    /// only for collections whose rules all compiled natively).
    pub fn native_session(&self, ctx: &AccessContext, action: Action, coll: &Collection) -> Option<RunAs> {
        if self.native_rls_mode() != Some(NativeMode::Enforce) || ctx.is_privileged() || !has_policies(coll) {
            return None;
        }
        let complete = self
            .inner
            .native
            .complete
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&coll.id)
            .copied()
            .unwrap_or(false);
        if !complete {
            return None;
        }
        Some(RunAs::new(
            RLS_ROLE,
            claims(&self.snapshot(), ctx, action, coll).to_string(),
        ))
    }
}
