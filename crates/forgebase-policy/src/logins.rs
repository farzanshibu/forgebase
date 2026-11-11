//! Named direct logins and the SQL editor's "run as" (FB-153,
//! rls-coverage §3.5, G11).
//!
//! A direct login is a PostgreSQL role `fbu_<name>` bound to one ForgeBase
//! principal (a user or a non-administrator service account) and at most one
//! tenant (a raw connection cannot send `X-ForgeBase-Tenant`):
//!
//! * the role is created, rotated and dropped by the database administrator
//!   ([`forgebase_db::Backend::admin_batch`]: `forgebase_super` on the
//!   bundled engine). Its password is generated, shown once, and only its
//!   SCRAM verifier (computed here) reaches PostgreSQL. Each role has a
//!   `CONNECTION LIMIT` (10), `statement_timeout` (30 s),
//!   `idle_in_transaction_session_timeout` (60 s), optional `VALID UNTIL`,
//!   and `search_path = api`;
//! * it is a member of `forgebase_login` (`SELECT` on the `api` views) or,
//!   unless `readonly`, `forgebase_login_write` (also writes through them).
//!   An `admin` login is a member of `forgebase_sql_admin` instead and its
//!   sessions start as that role (the SQL editor's rights);
//! * its claims (what `forgebase.claims()` returns for its sessions) are
//!   stored in `_forgebase_db_logins` and rewritten after every policy
//!   snapshot reload ([`PolicyService::reload`]) and inside every collection
//!   change (the native-policy hook), from the same snapshot and steps as the
//!   API path. A login whose principal or tenant is gone gets `NULL` claims:
//!   every policy fails.
//!
//! Login claims cover every collection and the actions `list`, `create`,
//! `update`, `delete` (a direct login always lists, never views). One
//! difference with the API path: the tenant gate counts memberships only,
//! not environment ACL entries that share one permission key with
//! non-members (a login's statements are not tied to one key), so a login
//! may see less than the API, never more.
//!
//! "Run as" (SQL editor): [`PolicyService::run_as_claims`] computes the same
//! claims for a chosen user and tenant; the data core runs the statements as
//! the login groups through the `api` views (`forgebase_data::SqlRunAs`).

use std::fmt::Write as _;

use base64::Engine as _;
use forgebase_data::{AccessContext, Action, GetOptions, Principal, SqlRunAs};
use forgebase_db::{ColumnType, Statement, Value, WriteTx};
use forgebase_schema::Collection;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::engine::{home_realm, tenant_scope};
use crate::native::{LOGIN_GROUP, NativeMode, claims, has_policies};
use crate::snapshot::Snapshot;
use crate::{PolicyError, PolicyService};

/// Prefix of every named login's role.
pub const ROLE_PREFIX: &str = "fbu_";
/// Logins that may also write through the `api` views.
pub const LOGIN_WRITE_GROUP: &str = "forgebase_login_write";
/// The SQL editor's role, and the group of `admin` logins.
pub const SQL_ADMIN_ROLE: &str = "forgebase_sql_admin";
/// Default `CONNECTION LIMIT` of a login.
pub const DEFAULT_CONNECTION_LIMIT: u32 = 10;
/// Default `statement_timeout` of a login.
pub const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 30_000;
/// `idle_in_transaction_session_timeout` of every login.
pub const IDLE_TIMEOUT_MS: u64 = 60_000;
/// SCRAM iterations of generated verifiers (PostgreSQL's default).
const SCRAM_ITERATIONS: u32 = 4096;
/// Audit events (`_forgebase_auth_audit`).
pub const AUDIT_CREATE: &str = "db.login.create";
/// See [`AUDIT_CREATE`].
pub const AUDIT_ROTATE: &str = "db.login.rotate";
/// See [`AUDIT_CREATE`].
pub const AUDIT_DELETE: &str = "db.login.delete";

/// A login to create (`forgebase db login create`, `POST
/// /api/v1/system/db-logins`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginSpec {
    /// `[a-z][a-z0-9_]{0,39}`; the role is `fbu_<name>`.
    pub name: String,
    /// Principal key the login acts as: `user:<collection>/<id>` or
    /// `service:<id>`. Optional for `admin` logins (default: the creator).
    #[serde(default, alias = "as")]
    pub principal: Option<String>,
    /// The one tenant the login's statements are scoped to.
    #[serde(default)]
    pub tenant: Option<String>,
    /// Reads only (member of `forgebase_login`, not `forgebase_login_write`).
    #[serde(default)]
    pub readonly: bool,
    /// The SQL editor's rights (`forgebase_sql_admin`, bypasses row-level
    /// security); platform administrators only.
    #[serde(default)]
    pub admin: bool,
    /// `CONNECTION LIMIT` (1 to 100, default 10).
    #[serde(default)]
    pub connection_limit: Option<u32>,
    /// `statement_timeout` in milliseconds (1 to 3 600 000, default 30 000).
    #[serde(default)]
    pub statement_timeout_ms: Option<u64>,
    /// Expiry (`VALID UNTIL`), RFC 3339 such as `2027-01-31T00:00:00Z`.
    #[serde(default)]
    pub valid_until: Option<String>,
}

/// A named login, as listed.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct DbLogin {
    /// Name.
    pub name: String,
    /// PostgreSQL role (`fbu_<name>`).
    pub role: String,
    /// Principal key it acts as.
    pub principal: String,
    /// Tenant.
    pub tenant: Option<String>,
    /// Reads only.
    pub readonly: bool,
    /// SQL editor rights.
    pub admin: bool,
    /// `CONNECTION LIMIT`.
    pub connection_limit: i64,
    /// `statement_timeout` (ms).
    pub statement_timeout_ms: i64,
    /// `idle_in_transaction_session_timeout` (ms).
    pub idle_timeout_ms: i64,
    /// Expiry.
    pub valid_until: Option<String>,
    /// The claims are present (`false`: the principal or tenant is gone, or
    /// an admin login, whose sessions bypass row-level security).
    pub has_claims: bool,
    /// Last claims refresh.
    pub claims_at: Option<String>,
    /// Who created it.
    pub created_by: String,
    /// When.
    pub created_at: String,
    /// Last password rotation.
    pub rotated_at: Option<String>,
}

/// A login with its password, returned once by create and rotate.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LoginSecret {
    /// The login.
    pub login: DbLogin,
    /// The generated password: shown once, never stored by ForgeBase.
    pub password: String,
    /// `psql` connection string over the Unix socket, without the password.
    pub hint: String,
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// An RFC 3339 instant PostgreSQL parses (`VALID UNTIL`): digits and `T Z :
/// + - .` only, so it can go into SQL as a literal.
fn valid_until(v: &str) -> bool {
    let b = v.as_bytes();
    (10..=35).contains(&b.len())
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b.iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b'T' | b'Z' | b':' | b'+' | b'-' | b'.'))
}

/// The SCRAM-SHA-256 verifier PostgreSQL stores for `password`
/// (RFC 5802/7677): the password itself never reaches the server or its log.
pub fn scram_verifier(password: &str, salt: &[u8], iterations: u32) -> String {
    let hmac = |key: &[u8], parts: &[&[u8]]| {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts keys of any length");
        for p in parts {
            mac.update(p);
        }
        mac.finalize().into_bytes()
    };
    // PBKDF2-HMAC-SHA-256, one block.
    let mut u = hmac(password.as_bytes(), &[salt, &1u32.to_be_bytes()]);
    let mut salted = u;
    for _ in 1..iterations {
        u = hmac(password.as_bytes(), &[&u]);
        for (a, b) in salted.iter_mut().zip(u.iter()) {
            *a ^= b;
        }
    }
    let client_key = hmac(&salted, &[b"Client Key"]);
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac(&salted, &[b"Server Key"]);
    let b64 = base64::engine::general_purpose::STANDARD;
    format!(
        "SCRAM-SHA-256${iterations}:{}${}:{}",
        b64.encode(salt),
        b64.encode(stored_key),
        b64.encode(server_key)
    )
}

fn random_password() -> Result<(String, Vec<u8>), PolicyError> {
    let pw = forgebase_auth::crypto::random_token(24).map_err(|e| PolicyError::Internal(e.to_string()))?;
    let salt = forgebase_auth::crypto::random_bytes(16).map_err(|e| PolicyError::Internal(e.to_string()))?;
    Ok((pw, salt))
}

/// Parses a principal key a login may act as.
fn parse_principal(key: &str) -> Result<Principal, PolicyError> {
    let bad = || {
        PolicyError::invalid(
            "invalid_principal",
            format!("{key:?} is not user:<collection>/<id> or service:<id>"),
        )
    };
    if let Some(rest) = key.strip_prefix("user:") {
        let (collection, id) = rest.split_once('/').ok_or_else(bad)?;
        if collection.is_empty() || id.is_empty() {
            return Err(bad());
        }
        return Ok(Principal::User {
            collection: collection.to_owned(),
            id: id.to_owned(),
        });
    }
    if let Some(id) = key.strip_prefix("service:")
        && !id.is_empty()
    {
        return Ok(Principal::Service {
            id: id.to_owned(),
            admin: false,
        });
    }
    Err(bad())
}

/// The claims of a direct login (or "run as") acting as `ctx`: every
/// collection with policies, for `list`, `create`, `update` and `delete`; no
/// `action` (`forgebase.read_action()` is then `list`); the tenant gate
/// counts memberships only (module docs).
pub fn login_claims(snap: &Snapshot, ctx: &AccessContext, catalog: &[Collection]) -> Json {
    let dummy = Collection::base("_");
    let colls: Vec<&Collection> = catalog.iter().filter(|c| has_policies(c)).collect();
    let mut out = claims(snap, ctx, Action::List, colls.first().copied().unwrap_or(&dummy));
    let mut all = serde_json::Map::new();
    for c in &colls {
        let mut per = serde_json::Map::new();
        for a in [Action::List, Action::Create, Action::Update, Action::Delete] {
            if let Some(Json::Object(m)) = claims(snap, ctx, a, c).get("colls").and_then(|x| x.get(&c.name)) {
                per.extend(m.clone());
            }
        }
        all.insert(c.name.clone(), Json::Object(per));
    }
    let principal = ctx.principal_key();
    let realm_ok = ctx.realm_id == home_realm(snap, ctx) && snap.realms.contains_key(&ctx.realm_id);
    let member = realm_ok
        && match tenant_scope(snap, ctx) {
            Err(_) => false,
            Ok((org, owner, _)) => match (org, owner) {
                (Some(o), _) => snap.membership(&o, &principal).is_some_and(|m| m.status == "active"),
                (None, Some(u)) => u == principal,
                _ => true,
            },
        };
    if let Some(m) = out.as_object_mut() {
        m.remove("action");
        m.insert("colls".into(), Json::Object(all));
        m.insert("login".into(), Json::Bool(true));
    }
    out["gate"]["tenant"] = Json::Bool(member);
    out
}

fn row_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

fn row_int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or(0)
}

const LOGIN_COLUMNS: &str = "name, role_name, principal, tenant, readonly, admin, connection_limit, \
     statement_timeout_ms, idle_timeout_ms, valid_until, CAST(claims IS NOT NULL AS integer), claims_at, created_by, \
     created_at, rotated_at";

fn login_from(r: &[Value]) -> DbLogin {
    DbLogin {
        name: row_text(r, 0).unwrap_or_default(),
        role: row_text(r, 1).unwrap_or_default(),
        principal: row_text(r, 2).unwrap_or_default(),
        tenant: row_text(r, 3),
        readonly: row_int(r, 4) != 0,
        admin: row_int(r, 5) != 0,
        connection_limit: row_int(r, 6),
        statement_timeout_ms: row_int(r, 7),
        idle_timeout_ms: row_int(r, 8),
        valid_until: row_text(r, 9),
        has_claims: row_int(r, 10) != 0,
        claims_at: row_text(r, 11),
        created_by: row_text(r, 12).unwrap_or_default(),
        created_at: row_text(r, 13).unwrap_or_default(),
        rotated_at: row_text(r, 14),
    }
}

impl PolicyService {
    fn require_logins(&self) -> Result<(), PolicyError> {
        if self.backend().dialect().name() != "postgres" {
            return Err(PolicyError::invalid(
                "db_logins_unavailable",
                "direct database logins need the PostgreSQL backend",
            ));
        }
        Ok(())
    }

    fn require_admin(&self, ctx: &AccessContext, what: &str) -> Result<(), PolicyError> {
        if ctx.is_privileged() {
            return Ok(());
        }
        let d = crate::Denial::at(crate::Step::Permission, format!("{what} needs an administrator"));
        self.audit()
            .record(crate::pipeline::audit_record(ctx, &format!("db:{what}"), None, &d));
        Err(PolicyError::Denied(d))
    }

    /// The context a login bound to `principal` and `tenant` acts in:
    /// its home realm, the tenancy layer's checks for `tenant`, roles and
    /// auth-record attributes, as for an API request.
    ///
    /// # Errors
    /// An unknown principal, a disabled or administrator service account,
    /// or a tenant the principal cannot reach.
    pub async fn login_context(&self, principal: &str, tenant: Option<&str>) -> Result<AccessContext, PolicyError> {
        let p = parse_principal(principal)?;
        let data = self.data()?;
        match &p {
            Principal::User { collection, id } => {
                data.get_record(&AccessContext::system(), collection, id, &GetOptions::default())
                    .await
                    .map_err(|_| PolicyError::invalid("invalid_principal", format!("no user {principal}")))?;
            }
            Principal::Service { id, .. } => {
                let rows = self
                    .backend()
                    .fetch_all(&Statement::new(
                        "SELECT role, disabled_at FROM _forgebase_service_accounts WHERE id = ?1",
                        vec![id.clone().into()],
                    ))
                    .await?;
                let Some(r) = rows.rows.first() else {
                    return Err(PolicyError::invalid(
                        "invalid_principal",
                        format!("no service account {principal}"),
                    ));
                };
                if row_text(r, 0).as_deref() == Some("admin") {
                    return Err(PolicyError::invalid(
                        "invalid_principal",
                        "an administrator service account bypasses rules; create an admin login instead",
                    ));
                }
                if !matches!(r.get(1), None | Some(Value::Null)) {
                    return Err(PolicyError::invalid(
                        "invalid_principal",
                        format!("service account {principal} is disabled"),
                    ));
                }
            }
            _ => return Err(PolicyError::invalid("invalid_principal", "not a user or service account")),
        }
        let mut ctx = match p {
            Principal::User { collection, id } => AccessContext::user(collection, id),
            Principal::Service { id, .. } => AccessContext::service(id, false),
            _ => return Err(PolicyError::invalid("invalid_principal", "not a user or service account")),
        };
        ctx.realm_id = home_realm(&self.snapshot(), &ctx);
        crate::http::resolve_parts(self, &mut ctx, None, None, tenant.map(str::to_owned))
            .await
            .map_err(|e| match e {
                PolicyError::Denied(d) => PolicyError::invalid(
                    "invalid_tenant",
                    format!("{principal} cannot act in this tenant: {}", d.reason),
                ),
                other => other,
            })?;
        Ok(ctx)
    }

    /// The claims "run as" uses in the SQL editor (FB-153): those a direct
    /// login bound to `principal` and `tenant` would get.
    ///
    /// # Errors
    /// Not an administrator; SQLite; native row-level security not on;
    /// [`PolicyService::login_context`] errors.
    pub async fn run_as_claims(
        &self,
        ctx: &AccessContext,
        principal: &str,
        tenant: Option<&str>,
    ) -> Result<SqlRunAs, PolicyError> {
        self.require_admin(ctx, "sql_run_as")?;
        self.require_logins()?;
        if !matches!(
            self.native_rls_mode(),
            Some(NativeMode::Logins | NativeMode::Enforce)
        ) {
            return Err(PolicyError::invalid(
                "run_as_unavailable",
                "run as needs native row-level security (native_rls = logins or enforce)",
            ));
        }
        let target = self.login_context(principal, tenant).await?;
        let catalog = self.data()?.list_collections(&AccessContext::system()).await?;
        Ok(SqlRunAs {
            principal: target.principal_key(),
            tenant: target.tenant.clone(),
            claims: login_claims(&self.snapshot(), &target, &catalog).to_string(),
        })
    }

    async fn login_claims_of(
        &self,
        principal: &str,
        tenant: Option<&str>,
        admin: bool,
        catalog: &[Collection],
    ) -> Option<Json> {
        if admin {
            return None;
        }
        match self.login_context(principal, tenant).await {
            Ok(ctx) => Some(login_claims(&self.snapshot(), &ctx, catalog)),
            Err(e) => {
                tracing::warn!(principal, error = %e, "direct login without claims: it sees nothing");
                None
            }
        }
    }

    /// Records an administrator's database action in `_forgebase_auth_audit`
    /// (`db.login.*`, `db.extension.*`; `GET /api/auth/audit`).
    ///
    /// # Errors
    /// Storage errors.
    pub async fn audit_event(&self, ctx: &AccessContext, event: &str, detail: Json) -> Result<(), PolicyError> {
        let d = self.backend().dialect();
        let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
        let r = tx
            .execute(&Statement::new(
                format!(
                    "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, impersonator, realm)
                     VALUES ({}, ?1, 'success', ?2, ?3, CAST(?4 AS {}), ?5, ?6)",
                    d.now_epoch(),
                    d.column_type(ColumnType::Json)
                ),
                vec![
                    event.to_owned().into(),
                    ctx.actor().into(),
                    ctx.request_id.clone().into(),
                    detail.to_string().into(),
                    ctx.impersonator.clone().into(),
                    ctx.realm_id.clone().into(),
                ],
            ))
            .await;
        match r {
            Ok(_) => tx.commit().await?,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Creates a named direct login (administrators; `admin` logins:
    /// platform administrators). Audited (`db.login.create`).
    ///
    /// # Errors
    /// Invalid input, a name taken, SQLite, native row-level security off
    /// for a non-admin login, or an entitlement refusal (`db.direct_access`).
    pub async fn create_db_login(&self, ctx: &AccessContext, spec: LoginSpec) -> Result<LoginSecret, PolicyError> {
        self.require_admin(ctx, "logins")?;
        self.require_logins()?;
        if !valid_name(&spec.name) {
            return Err(PolicyError::invalid(
                "invalid_name",
                "a login name is 1 to 40 of a-z, 0-9 and _, starting with a letter",
            ));
        }
        if spec.admin && !matches!(ctx.principal, Principal::Admin { .. } | Principal::System) {
            let d = crate::Denial::at(crate::Step::Permission, "admin logins need a platform administrator");
            return Err(PolicyError::Denied(d));
        }
        let limit = spec.connection_limit.unwrap_or(DEFAULT_CONNECTION_LIMIT);
        let timeout = spec.statement_timeout_ms.unwrap_or(DEFAULT_STATEMENT_TIMEOUT_MS);
        if !(1..=100).contains(&limit) || !(1..=3_600_000).contains(&timeout) {
            return Err(PolicyError::invalid(
                "invalid_limits",
                "connection_limit is 1 to 100 and statement_timeout_ms 1 to 3600000",
            ));
        }
        if let Some(v) = &spec.valid_until
            && !valid_until(v)
        {
            return Err(PolicyError::invalid(
                "invalid_valid_until",
                "valid_until is an RFC 3339 time such as 2027-01-31T00:00:00Z",
            ));
        }
        let principal = match (&spec.principal, spec.admin) {
            (Some(p), _) => p.trim().to_owned(),
            (None, true) => ctx.principal_key(),
            (None, false) => {
                return Err(PolicyError::invalid(
                    "invalid_principal",
                    "name the principal the login acts as (user:<collection>/<id> or service:<id>)",
                ));
            }
        };
        let catalog = self.data()?.list_collections(&AccessContext::system()).await?;
        let bound = if spec.admin {
            None
        } else {
            if !matches!(
                self.native_rls_mode(),
                Some(NativeMode::Logins | NativeMode::Enforce)
            ) {
                return Err(PolicyError::invalid(
                    "db_logins_unavailable",
                    "direct logins see data through native row-level security: set [database.postgres] native_rls = \"logins\" (the default) or \"enforce\"",
                ));
            }
            let b = self.login_context(&principal, spec.tenant.as_deref()).await?;
            let req = crate::HookRequest {
                ctx: &b,
                subject: b.subject(),
                permission: "db:direct_access",
                resource: None,
            };
            if let crate::HookOutcome::Deny(why) = self.hooks().entitlement.check(&req) {
                return Err(PolicyError::Denied(crate::Denial::at(crate::Step::Entitlement, why)));
            }
            Some(b)
        };
        let exists = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM _forgebase_db_logins WHERE name = ?1",
                vec![spec.name.clone().into()],
            ))
            .await?;
        if !exists.rows.is_empty() {
            return Err(PolicyError::Conflict(format!("login {}", spec.name)));
        }
        let role = format!("{ROLE_PREFIX}{}", spec.name);
        let (password, salt) = random_password()?;
        let verifier = scram_verifier(&password, &salt, SCRAM_ITERATIONS);
        let r = ident(&role);
        let mut create = format!(
            "CREATE ROLE {r} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS INHERIT \
             CONNECTION LIMIT {limit} PASSWORD {}",
            literal(&verifier)
        );
        if let Some(v) = &spec.valid_until {
            let _ = write!(create, " VALID UNTIL {}", literal(v));
        }
        let groups = if spec.admin {
            format!("{LOGIN_GROUP}, {SQL_ADMIN_ROLE}")
        } else if spec.readonly {
            LOGIN_GROUP.to_owned()
        } else {
            LOGIN_WRITE_GROUP.to_owned()
        };
        let mut stmts = vec![
            create,
            format!("GRANT {groups} TO {r}"),
            format!("ALTER ROLE {r} SET statement_timeout = '{timeout}ms'"),
            format!("ALTER ROLE {r} SET idle_in_transaction_session_timeout = '{IDLE_TIMEOUT_MS}ms'"),
        ];
        if spec.admin {
            stmts.push(format!("ALTER ROLE {r} SET role = '{SQL_ADMIN_ROLE}'"));
            stmts.push(format!("ALTER ROLE {r} SET search_path = public, forgebase"));
        } else {
            stmts.push(format!("ALTER ROLE {r} SET search_path = api"));
        }
        let taken = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = ?1",
                vec![role.clone().into()],
            ))
            .await?;
        if !taken.rows.is_empty() {
            return Err(PolicyError::Conflict(format!("PostgreSQL role {role}")));
        }
        self.backend().admin_batch(&stmts).await?;
        let claims = match &bound {
            Some(b) => Some(login_claims(&self.snapshot(), b, &catalog)),
            None => None,
        };
        let now = forgebase_core::now_rfc3339();
        let principal_key = bound.as_ref().map_or(principal.clone(), AccessContext::principal_key);
        let inserted = async {
            let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
            let r = tx
                .execute(&Statement::new(
                    "INSERT INTO _forgebase_db_logins (name, role_name, principal, tenant, readonly, admin, \
                     connection_limit, statement_timeout_ms, idle_timeout_ms, valid_until, claims, claims_at, \
                     created_by, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, CAST(?11 AS jsonb), ?12, ?13, ?14)",
                    vec![
                        spec.name.clone().into(),
                        role.clone().into(),
                        principal_key.clone().into(),
                        bound.as_ref().and_then(|b| b.tenant.clone()).into(),
                        i64::from(spec.readonly).into(),
                        i64::from(spec.admin).into(),
                        i64::from(limit).into(),
                        i64::try_from(timeout).unwrap_or(i64::MAX).into(),
                        i64::try_from(IDLE_TIMEOUT_MS).unwrap_or(i64::MAX).into(),
                        spec.valid_until.clone().into(),
                        claims.as_ref().map(Json::to_string).into(),
                        claims.as_ref().map(|_| now.clone()).into(),
                        ctx.actor().into(),
                        now.clone().into(),
                    ],
                ))
                .await;
            match r {
                Ok(_) => tx.commit().await,
                Err(e) => {
                    let _ = tx.rollback().await;
                    Err(e)
                }
            }
        }
        .await;
        if let Err(e) = inserted {
            let _ = self.backend().admin_batch(&[format!("DROP ROLE IF EXISTS {r}")]).await;
            return Err(e.into());
        }
        self.audit_event(
            ctx,
            AUDIT_CREATE,
            json!({"name": spec.name, "principal": principal_key, "tenant": spec.tenant,
                   "readonly": spec.readonly, "admin": spec.admin}),
        )
        .await?;
        let login = self.get_db_login(ctx, &spec.name).await?;
        Ok(LoginSecret {
            hint: format!("forgebase db psql --as {}", spec.name),
            login,
            password,
        })
    }

    /// Every named login (administrators).
    ///
    /// # Errors
    /// Not an administrator; SQLite.
    pub async fn list_db_logins(&self, ctx: &AccessContext) -> Result<Vec<DbLogin>, PolicyError> {
        self.require_admin(ctx, "logins")?;
        self.require_logins()?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {LOGIN_COLUMNS} FROM _forgebase_db_logins ORDER BY name"),
                vec![],
            ))
            .await?;
        Ok(rows.rows.iter().map(|r| login_from(r)).collect())
    }

    /// One named login (administrators).
    ///
    /// # Errors
    /// Not an administrator; SQLite; not found.
    pub async fn get_db_login(&self, ctx: &AccessContext, name: &str) -> Result<DbLogin, PolicyError> {
        self.require_admin(ctx, "logins")?;
        self.require_logins()?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {LOGIN_COLUMNS} FROM _forgebase_db_logins WHERE name = ?1"),
                vec![name.to_owned().into()],
            ))
            .await?;
        rows.rows
            .first()
            .map(|r| login_from(r))
            .ok_or_else(|| PolicyError::NotFound(format!("login {name}")))
    }

    /// A new password for a login (and optionally a new expiry); open
    /// sessions stay. Audited (`db.login.rotate`).
    ///
    /// # Errors
    /// Not an administrator; SQLite; not found; a bad `valid_until`.
    pub async fn rotate_db_login(
        &self,
        ctx: &AccessContext,
        name: &str,
        valid_until_at: Option<String>,
    ) -> Result<LoginSecret, PolicyError> {
        let login = self.get_db_login(ctx, name).await?;
        if login.admin && !matches!(ctx.principal, Principal::Admin { .. } | Principal::System) {
            let d = crate::Denial::at(crate::Step::Permission, "admin logins need a platform administrator");
            return Err(PolicyError::Denied(d));
        }
        if let Some(v) = &valid_until_at
            && !valid_until(v)
        {
            return Err(PolicyError::invalid(
                "invalid_valid_until",
                "valid_until is an RFC 3339 time such as 2027-01-31T00:00:00Z",
            ));
        }
        let (password, salt) = random_password()?;
        let mut alter = format!(
            "ALTER ROLE {} PASSWORD {}",
            ident(&login.role),
            literal(&scram_verifier(&password, &salt, SCRAM_ITERATIONS))
        );
        if let Some(v) = &valid_until_at {
            let _ = write!(alter, " VALID UNTIL {}", literal(v));
        }
        self.backend().admin_batch(&[alter]).await?;
        let now = forgebase_core::now_rfc3339();
        let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
        let r = tx
            .execute(&Statement::new(
                "UPDATE _forgebase_db_logins SET rotated_at = ?1, valid_until = coalesce(?2, valid_until) WHERE name = ?3",
                vec![now.into(), valid_until_at.clone().into(), name.to_owned().into()],
            ))
            .await;
        match r {
            Ok(_) => tx.commit().await?,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        self.audit_event(ctx, AUDIT_ROTATE, json!({"name": name})).await?;
        Ok(LoginSecret {
            login: self.get_db_login(ctx, name).await?,
            password,
            hint: format!("forgebase db psql --as {name}"),
        })
    }

    /// Drops a login: its open sessions end, its role is dropped. Audited
    /// (`db.login.delete`).
    ///
    /// # Errors
    /// Not an administrator; SQLite; not found.
    pub async fn delete_db_login(&self, ctx: &AccessContext, name: &str) -> Result<(), PolicyError> {
        let login = self.get_db_login(ctx, name).await?;
        if login.admin && !matches!(ctx.principal, Principal::Admin { .. } | Principal::System) {
            let d = crate::Denial::at(crate::Step::Permission, "admin logins need a platform administrator");
            return Err(PolicyError::Denied(d));
        }
        let r = ident(&login.role);
        // Sessions see nothing from this moment: the row goes first.
        let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
        let del = tx
            .execute(&Statement::new(
                "DELETE FROM _forgebase_db_logins WHERE name = ?1",
                vec![name.to_owned().into()],
            ))
            .await;
        match del {
            Ok(_) => tx.commit().await?,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        self.backend()
            .admin_batch(&[
                format!("ALTER ROLE {r} NOLOGIN"),
                format!(
                    "SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity WHERE usename = {}",
                    literal(&login.role)
                ),
                format!("REASSIGN OWNED BY {r} TO {SQL_ADMIN_ROLE}"),
                format!("DROP OWNED BY {r}"),
                format!("DROP ROLE {r}"),
            ])
            .await?;
        self.audit_event(ctx, AUDIT_DELETE, json!({"name": name})).await?;
        Ok(())
    }

    /// Rewrites every login's claims from the current snapshot (after a
    /// snapshot reload; at startup). Returns how many logins it refreshed.
    /// Nothing on SQLite.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn refresh_db_logins(&self) -> Result<usize, PolicyError> {
        if self.require_logins().is_err() || self.data().is_err() {
            return Ok(0);
        }
        let _one = self.inner.logins.lock().await;
        let catalog = self.data()?.list_collections(&AccessContext::system()).await?;
        let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
        match self.refresh_db_logins_in(&mut *tx, &catalog).await {
            Ok(n) => {
                tx.commit().await?;
                Ok(n)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// [`PolicyService::refresh_db_logins`] inside `tx` with `catalog` (the
    /// schema transaction of a collection change).
    pub(crate) async fn refresh_db_logins_in(
        &self,
        tx: &mut dyn WriteTx,
        catalog: &[Collection],
    ) -> Result<usize, PolicyError> {
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT name, principal, tenant, admin FROM _forgebase_db_logins ORDER BY name",
                vec![],
            ))
            .await?;
        let now = forgebase_core::now_rfc3339();
        for r in &rows.rows {
            let (name, principal, tenant) = (
                row_text(r, 0).unwrap_or_default(),
                row_text(r, 1).unwrap_or_default(),
                row_text(r, 2),
            );
            let claims = self
                .login_claims_of(&principal, tenant.as_deref(), row_int(r, 3) != 0, catalog)
                .await;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_db_logins SET claims = CAST(?1 AS jsonb), claims_at = ?2 WHERE name = ?3",
                vec![claims.map(|c| c.to_string()).into(), now.clone().into(), name.into()],
            ))
            .await?;
        }
        Ok(rows.rows.len())
    }

    /// Prepares the SQL editor's role (FB-153, PostgreSQL): re-grants
    /// `forgebase_sql_admin` the non-secret columns of every `forgebase`
    /// table (`forgebase.fb_editor_grants()`, which covers tables of later
    /// migrations) and, when both pools may switch to it, makes the data
    /// core's SQL editor run as it. Returns whether the editor now runs as
    /// that role.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn enable_sql_editor_role(&self) -> Result<bool, PolicyError> {
        if self.require_logins().is_err() {
            return Ok(false);
        }
        let data = self.data()?;
        let check = Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_roles r WHERE r.rolname = ?1 AND pg_catalog.pg_has_role(current_user, r.oid, 'SET')",
            vec![SQL_ADMIN_ROLE.into()],
        );
        let mut tx = self.backend().begin(forgebase_db::TxMode::Normal).await?;
        let writer = async {
            let n = tx.fetch_all(&check).await?.scalar().and_then(Value::as_i64).unwrap_or(0);
            if n > 0 {
                tx.execute(&Statement::raw("SELECT forgebase.fb_editor_grants()"))
                    .await?;
            }
            Ok::<bool, forgebase_db::DbError>(n > 0)
        }
        .await;
        let writer = match writer {
            Ok(w) => {
                tx.commit().await?;
                w
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        };
        let reader = self.backend().fetch_all(&check).await?.scalar().and_then(Value::as_i64) == Some(1);
        let on = writer && reader;
        data.set_sql_editor_role(on.then(|| SQL_ADMIN_ROLE.to_owned()));
        if !on {
            tracing::warn!(
                role = SQL_ADMIN_ROLE,
                "the SQL editor runs as the server's own roles: {SQL_ADMIN_ROLE} is missing or not granted to both pools"
            );
        }
        Ok(on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scram_verifier_matches_postgres() {
        // RFC 7677 §3: password "pencil", salt W22ZaJ0SNY7soEsUEjb6gQ==,
        // 4096 iterations. These keys reproduce the RFC's client proof
        // (dHzbZapW…) and server signature (6rriTRBi…) for its nonces.
        let salt = base64::engine::general_purpose::STANDARD
            .decode("W22ZaJ0SNY7soEsUEjb6gQ==")
            .expect("salt");
        let v = scram_verifier("pencil", &salt, 4096);
        assert_eq!(
            v,
            "SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU="
        );
    }

    #[test]
    fn names_and_times_are_checked() {
        for ok in ["a", "alice", "bi_reports2", "x234567890123456789012345678901234567890"[..40].as_ref()] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "Alice", "1a", "a-b", "a b", "ä", &"a".repeat(41)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_until("2027-01-31T00:00:00Z"));
        assert!(valid_until("2027-01-31T00:00:00.5+02:00"));
        for bad in ["tomorrow", "2027-01-31'; DROP ROLE x; --", "infinity", "2027"] {
            assert!(!valid_until(bad), "{bad}");
        }
    }

    #[test]
    fn principals_parse() {
        assert_eq!(
            parse_principal("user:users/u1").expect("user"),
            Principal::User {
                collection: "users".into(),
                id: "u1".into()
            }
        );
        assert!(matches!(
            parse_principal("service:sa1").expect("svc"),
            Principal::Service { admin: false, .. }
        ));
        for bad in ["admin:a1", "user:users", "user:/u1", "service:", "system", "anonymous"] {
            assert!(parse_principal(bad).is_err(), "{bad}");
        }
    }
}
