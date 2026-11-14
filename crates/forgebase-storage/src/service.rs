//! The storage service: state, authorization, buckets and ACLs. Objects,
//! uploads, signed URLs and the reconciler extend it in their own modules.
//!
//! **Database sets** (ADR-0029 §3.2, §3.5, FB-172). With a router attached
//! ([`StorageService::attach_router`]) every call resolves the set of the
//! bucket it is about: a tenant bucket (stored name `<tenant>/<name>`) and
//! its objects, ACL entries, upload sessions and parts live on the tenant's
//! set; an instance-wide bucket (no tenant) and its rows on control. The
//! settings table and the sealed URL signing key are control-class. Blob
//! reference counts (`_forgebase_storage_blobs`) are kept per set, next to
//! the objects that hold them, while the blob files in the driver are shared
//! by every set: the reconciler ([`crate::reconcile`]) deletes a file only
//! when no set references it. Without a router there is one database and
//! nothing changes.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::{Arc, OnceLock, RwLock};

use forgebase_auth::crypto::Keyring;
use forgebase_core::SignedUrlsMode;
use forgebase_data::{AccessContext, Action, DataService, Decision, Principal, ResourceKind, ResourceOp, ResourceRef};
use forgebase_db::{Backend, DbRouter, Rows, SetHandle, Statement, TenantRef, TxMode, Value, WriteTx};
use forgebase_entitlements::{Clock, EntitlementError, EntitlementService, GaugeRequest, subject_key};
use forgebase_jobs::JobService;

use crate::driver::StorageDriver;
use crate::error::{Result, StorageError};
use crate::model::{Acl, AclEntry, Bucket, BucketInput, BucketPatch, Permission, Visibility};
use crate::rules::{Access, WriteCheck};
use crate::util::{new_id, rfc3339, validate_bucket};

/// Quota feature (a gauge, counting logical bytes) the service meters.
pub const STORAGE_FEATURE: &str = "storage.bytes";

/// Name of the reconciler job.
pub const RECONCILE_JOB: &str = "storage.reconcile";

/// Name of the reconciler schedule.
pub const RECONCILE_SCHEDULE: &str = "storage-reconcile";

/// Tunables of the service.
#[derive(Debug, Clone)]
pub struct StorageSettings {
    /// Largest object, in bytes, unless a bucket sets a lower limit.
    pub max_object_bytes: u64,
    /// How long an upload session lives without being completed.
    pub upload_ttl_secs: u64,
    /// Longest lifetime of a signed URL.
    pub max_signed_ttl_secs: u64,
    /// Lifetime of a signed URL when the request names none.
    pub default_signed_ttl_secs: u64,
    /// How old an unreferenced blob or an abandoned staged file must be
    /// before the reconciler removes it.
    pub reconcile_grace_secs: u64,
    /// Quota feature key; `None` turns metering off.
    pub quota_feature: Option<String>,
    /// How signed downloads are served: ForgeBase's HMAC URLs (`Proxy`,
    /// the default), the provider's signed URL (`Native`), or native when
    /// the driver can sign (`Auto`). Uploads always use the proxy
    /// (ADR-0040 addendum).
    pub signed_urls: SignedUrlsMode,
    /// Per-bucket `signed_urls`: a bucket name (that name in every tenant)
    /// or `<tenant>/<name>` (one tenant's bucket, which wins).
    pub signed_urls_buckets: BTreeMap<String, SignedUrlsMode>,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            max_object_bytes: 5 * 1024 * 1024 * 1024,
            upload_ttl_secs: 24 * 3600,
            max_signed_ttl_secs: 7 * 24 * 3600,
            default_signed_ttl_secs: 900,
            reconcile_grace_secs: 3600,
            quota_feature: Some(STORAGE_FEATURE.to_owned()),
            signed_urls: SignedUrlsMode::Proxy,
            signed_urls_buckets: BTreeMap::new(),
        }
    }
}

/// The object storage service. Cheap to clone.
#[derive(Clone)]
pub struct StorageService {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for StorageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageService")
            .field("driver", &self.inner.driver.name())
            .finish_non_exhaustive()
    }
}

pub(crate) struct Inner {
    pub db: Arc<dyn Backend>,
    pub data: DataService,
    pub driver: Arc<dyn StorageDriver>,
    pub clock: Arc<dyn Clock>,
    pub settings: StorageSettings,
    pub ent: OnceLock<EntitlementService>,
    pub jobs: OnceLock<JobService>,
    /// Uploads hold the read side while they place a blob and commit its
    /// metadata; the reconciler holds the write side while it deletes blobs.
    pub gate: tokio::sync::RwLock<()>,
    /// The URL signing key (32 bytes), sealed in the database with the
    /// auth keyring (G11); replaced by [`StorageService::rotate_signing_key`].
    pub signing_key: RwLock<Arc<Vec<u8>>>,
    pub keyring: Keyring,
    /// The database sets (FB-172); unset: every row is in `db`.
    pub router: OnceLock<DbRouter>,
}

/// The database one call's rows live on: the service's own database (no
/// router), or one set of the router. Derefs to its [`Backend`]; a set's
/// handle routes the replica reads of FB-169 over that set's replicas.
#[derive(Clone, Debug)]
pub(crate) struct Db {
    backend: Arc<dyn Backend>,
    /// The router's set id (`None`: the service's database, no router).
    set: Option<String>,
    /// Whether this is control (always, without a router).
    control: bool,
}

impl Deref for Db {
    type Target = dyn Backend;

    fn deref(&self) -> &Self::Target {
        &*self.backend
    }
}

impl Db {
    fn plain(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            set: None,
            control: true,
        }
    }

    pub(crate) fn of(set: SetHandle) -> Self {
        Self {
            set: Some(set.id().to_owned()),
            control: set.is_control(),
            backend: Arc::new(set),
        }
    }

    /// Whether this is the control set (or the only database).
    pub(crate) fn is_control(&self) -> bool {
        self.control
    }

    /// The router's id of the set (`None` without a router).
    pub(crate) fn set_id(&self) -> Option<&str> {
        self.set.as_deref()
    }

    pub(crate) async fn read(&self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.backend.fetch_all(&st(sql, params)).await?)
    }

    /// A metadata read that may be served by a replica (ADR-0029 §2.5,
    /// FB-169): bucket listings, usage and statistics, object listings,
    /// versions and heads. Authorisation state (ACLs, bucket rows, grants),
    /// object content reads, writes and signed-URL issue stay on
    /// [`Self::read`].
    pub(crate) async fn read_replica(&self, what: &'static str, sql: &str, params: Vec<Value>) -> Result<Rows> {
        self.fetch_replica(what, &st(sql, params)).await
    }

    pub(crate) async fn fetch_replica(&self, what: &'static str, stmt: &Statement) -> Result<Rows> {
        Ok(self
            .backend
            .fetch_routed(stmt, &forgebase_db::ReadIntent::replica(what))
            .await?)
    }

    pub(crate) async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.backend.begin(TxMode::Normal).await?)
    }
}

/// A bucket as stored.
#[derive(Debug, Clone)]
pub(crate) struct BucketRow {
    /// Stored name (the key of objects, ACL entries and uploads):
    /// `<name>` for an instance-wide bucket, `<tenant>/<name>` for a
    /// tenant bucket (FB-140).
    pub id: String,
    /// Name the API shows and resource permissions and rules name.
    pub name: String,
    /// Owning tenant; `None` for an instance-wide bucket.
    pub tenant: Option<String>,
    /// The set holding the bucket and its objects, ACL and uploads.
    pub db: Db,
    pub visibility: Visibility,
    pub versioning: bool,
    pub max_object_bytes: Option<u64>,
    pub allowed: Option<Vec<String>>,
    pub owner: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BucketRow {
    fn wire(&self) -> Bucket {
        Bucket {
            name: self.name.clone(),
            tenant: self.tenant.clone(),
            visibility: self.visibility,
            versioning: self.versioning,
            max_object_bytes: self.max_object_bytes,
            allowed_content_types: self.allowed.clone(),
            owner: self.owner.clone(),
            created_at: rfc3339(self.created_at),
            updated_at: rfc3339(self.updated_at),
        }
    }
}

/// What an operation needs on a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Read,
    Write,
    Admin,
}

impl Need {
    fn action(self) -> Action {
        match self {
            Need::Read => Action::StorageRead,
            Need::Write => Action::StorageWrite,
            Need::Admin => Action::StorageAdmin,
        }
    }

    fn permission(self) -> Permission {
        match self {
            Need::Read => Permission::Read,
            Need::Write => Permission::Write,
            Need::Admin => Permission::Admin,
        }
    }
}

pub(crate) fn st(sql: &str, params: Vec<Value>) -> Statement {
    Statement::new(sql, params)
}

pub(crate) fn text(r: &[Value], i: usize) -> String {
    r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn opt_text(r: &[Value], i: usize) -> Option<String> {
    r.get(i).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn int(r: &[Value], i: usize) -> i64 {
    r.get(i).and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) fn opt_int(r: &[Value], i: usize) -> Option<i64> {
    r.get(i).and_then(Value::as_i64)
}

pub(crate) fn uint(r: &[Value], i: usize) -> u64 {
    u64::try_from(int(r, i)).unwrap_or(0)
}

pub(crate) fn iv(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

pub(crate) fn blob_key(sha: &str) -> String {
    format!("blobs/{}/{sha}", &sha[..2])
}

impl StorageService {
    /// Opens the service over a migrated database. Creates the URL-signing
    /// key on first use, sealed with `keyring` (the auth keyring of the data
    /// directory); a key stored in plain text before FB-140 is sealed in
    /// place, unchanged, so signed URLs already issued keep working.
    /// Registers the object rule fields ([`crate::rules::object_schema`])
    /// with the authorizer.
    pub async fn open(
        db: Arc<dyn Backend>,
        data: DataService,
        driver: Arc<dyn StorageDriver>,
        clock: Arc<dyn Clock>,
        settings: StorageSettings,
        keyring: Keyring,
    ) -> Result<Self> {
        let signing_key = load_signing_key(db.as_ref(), &keyring).await?;
        data.authorizer()
            .register_resource_schema(crate::rules::object_schema());
        let wants_native = settings.signed_urls == SignedUrlsMode::Native
            || settings
                .signed_urls_buckets
                .values()
                .any(|m| *m == SignedUrlsMode::Native);
        if wants_native && let Err(why) = driver.presign_support() {
            tracing::warn!(
                driver = driver.name(),
                reason = %why,
                "signed_urls is native, but the storage driver cannot sign: native signed download URLs will be refused (409 native_signing_unavailable)"
            );
        }
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                data,
                driver,
                clock,
                settings,
                ent: OnceLock::new(),
                jobs: OnceLock::new(),
                gate: tokio::sync::RwLock::new(()),
                signing_key: RwLock::new(Arc::new(signing_key)),
                keyring,
                router: OnceLock::new(),
            }),
        })
    }

    /// Routes calls over the database sets of `router` (ADR-0029 §3.5,
    /// FB-172): a tenant bucket and its objects, ACL entries, uploads and
    /// parts on the tenant's set (`ctx.tenant`, or the tenant a signed URL
    /// or stored bucket names), instance-wide buckets, the settings and the
    /// URL signing key on control; the reconciler runs over every set. A
    /// frozen tenant's calls wait for the move's switch, at most the
    /// router's freeze timeout, then fail with `409 tenant_moving`. Without
    /// a router everything is in the database the service was opened on (a
    /// [`DbRouter::single`] over that database behaves the same). Attach it
    /// once, before serving; a second call is ignored.
    pub fn attach_router(&self, router: DbRouter) {
        let _ = self.inner.router.set(router);
    }

    /// The router, when attached.
    pub(crate) fn router(&self) -> Option<&DbRouter> {
        self.inner.router.get()
    }

    /// Control: instance-wide buckets, settings, the signing key (the
    /// service's database without a router).
    pub(crate) fn control(&self) -> Db {
        match self.router() {
            Some(r) => Db::of(r.control()),
            None => Db::plain(self.inner.db.clone()),
        }
    }

    /// The set of `tenant`'s rows (`None` or `""`: control). Waits while
    /// the tenant is frozen by a move, at most the freeze timeout.
    pub(crate) async fn tenant_set(&self, tenant: Option<&str>) -> Result<Db> {
        match self.router() {
            Some(r) => Ok(Db::of(r.for_tenant(TenantRef::new(tenant)).await?)),
            None => Ok(Db::plain(self.inner.db.clone())),
        }
    }

    /// Every set (control first) with the tenants its maintenance leaves
    /// alone ([`forgebase_jobs::excluded_tenants`]: frozen everywhere,
    /// moving or in cleanup on every set but the one the placement names).
    /// Without a router, the one database and no exclusions.
    pub(crate) fn all_sets(&self) -> Vec<(Db, Vec<String>)> {
        let Some(router) = self.router() else {
            return vec![(Db::plain(self.inner.db.clone()), Vec::new())];
        };
        router
            .sets()
            .into_iter()
            .map(|set| {
                let excluded = forgebase_jobs::excluded_tenants(router, set.id());
                (Db::of(set), excluded)
            })
            .collect()
    }

    /// Whether `db` owns the rows of `tenant` (rule 5 of the brief: a move's
    /// source keeps stale copies until cleanup): the tenant's placement
    /// names the set, or the tenant has none (or is instance-wide) and the
    /// set is control.
    pub(crate) fn owns(&self, db: &Db, tenant: Option<&str>) -> bool {
        let (Some(router), Some(set)) = (self.router(), db.set_id()) else {
            return true;
        };
        match tenant.filter(|t| !t.is_empty()).and_then(|t| router.placement(t)) {
            Some(p) => p.set_id == set,
            None => db.is_control(),
        }
    }

    /// Attaches the quota engine; uploads then count against the
    /// `storage.bytes` gauge of the acting subject.
    pub fn attach_entitlements(&self, ent: EntitlementService) {
        let _ = self.inner.ent.set(ent);
    }

    /// Attaches the job queue (the reconciler schedule).
    pub fn attach_jobs(&self, jobs: JobService) {
        let _ = self.inner.jobs.set(jobs);
    }

    /// The driver.
    pub fn driver(&self) -> &Arc<dyn StorageDriver> {
        &self.inner.driver
    }

    /// Settings in force.
    pub fn settings(&self) -> &StorageSettings {
        &self.inner.settings
    }

    pub(crate) fn now(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    // ---- authorization -------------------------------------------------

    /// Decides whether `ctx` may do `need` with `op` on `bucket` (and `key`,
    /// `None` for an operation on the bucket itself).
    ///
    /// Order: administrators and the system; the bucket's owner; the
    /// bucket's visibility (reads); ACL entries (an entry for a prefix
    /// covers the keys under it, and only an entry with the empty prefix
    /// covers the bucket itself: G1); then the authorizer
    /// ([`Authorizer::authorize_resource`](forgebase_data::Authorizer::authorize_resource),
    /// FB-134): the permission `engine:storage_<need>:<bucket>` or
    /// `engine:storage_<need>`, else the bucket's resource rule for `op`,
    /// which may come back as a filter over object fields
    /// ([`Access::Filter`], G14). Admin needs are granted by permissions
    /// only.
    pub(crate) async fn authorize(
        &self,
        ctx: &AccessContext,
        bucket: &BucketRow,
        key: Option<&str>,
        need: Need,
        op: ResourceOp,
    ) -> Result<Access> {
        if self.direct_grant(ctx, bucket, key, need).await? {
            return Ok(Access::All);
        }
        match self.decide(ctx, bucket, need, op) {
            Decision::Allow => Ok(Access::All),
            Decision::Filter(p) if need != Need::Admin => Ok(Access::Filter(p)),
            _ => Err(Self::refusal(ctx, bucket, need)),
        }
    }

    /// The rule decisions of a write that adds a version of `key` (put,
    /// upload, copy target, restore): `WriteCheck::open()` when the caller
    /// is let in without rules, else the `create` and `update` decisions,
    /// applied when the version is installed. Fails when neither can allow.
    pub(crate) async fn authorize_write(
        &self,
        ctx: &AccessContext,
        bucket: &BucketRow,
        key: &str,
    ) -> Result<WriteCheck> {
        if self.direct_grant(ctx, bucket, Some(key), Need::Write).await? {
            return Ok(WriteCheck::open());
        }
        let create = self.decide(ctx, bucket, Need::Write, ResourceOp::Create);
        let update = self.decide(ctx, bucket, Need::Write, ResourceOp::Update);
        match (&create, &update) {
            (Decision::Allow, Decision::Allow) => Ok(WriteCheck::open()),
            (Decision::Deny, Decision::Deny) => Err(Self::refusal(ctx, bucket, Need::Write)),
            _ => Ok(WriteCheck {
                rules: Some((create, update)),
            }),
        }
    }

    /// Grants that need no rule: privileged callers, the bucket owner,
    /// visibility (reads) and ACL entries.
    async fn direct_grant(
        &self,
        ctx: &AccessContext,
        bucket: &BucketRow,
        key: Option<&str>,
        need: Need,
    ) -> Result<bool> {
        if ctx.is_privileged() {
            return Ok(true);
        }
        let anonymous = ctx.principal == Principal::Anonymous;
        if !anonymous && ctx.principal_key() == bucket.owner {
            return Ok(true);
        }
        if need == Need::Read {
            match bucket.visibility {
                Visibility::Public => return Ok(true),
                Visibility::Authenticated if !anonymous => return Ok(true),
                _ => {}
            }
        }
        let acl = self.load_acl(bucket).await?;
        let me = ctx.principal_key();
        Ok(acl.iter().any(|e| {
            e.permission >= need.permission()
                && acl_covers(&e.prefix, key)
                && match e.grantee.as_str() {
                    "public" => true,
                    "authenticated" => !anonymous,
                    g => {
                        !anonymous
                            && (g == me
                                || g.strip_prefix("role:")
                                    .is_some_and(|r| ctx.roles.iter().any(|have| have == r)))
                    }
                }
        }))
    }

    /// The authorizer's answer for `need` and `op` on `bucket` (FB-134).
    pub(crate) fn decide(&self, ctx: &AccessContext, bucket: &BucketRow, need: Need, op: ResourceOp) -> Decision {
        let res = ResourceRef::new(ResourceKind::Bucket, bucket.name.clone(), op);
        self.inner
            .data
            .authorizer()
            .authorize_resource(ctx, need.action(), &res)
    }

    pub(crate) fn refusal(ctx: &AccessContext, bucket: &BucketRow, need: Need) -> StorageError {
        if ctx.principal == Principal::Anonymous {
            return StorageError::Unauthenticated;
        }
        StorageError::Forbidden(format!(
            "you may not {} bucket {}",
            match need {
                Need::Read => "read",
                Need::Write => "write to",
                Need::Admin => "administer",
            },
            bucket.name
        ))
    }

    /// Platform-level permission without a bucket (run the reconciler, read
    /// statistics).
    pub(crate) fn authorize_platform(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        match self.inner.data.authorize_action(ctx, action) {
            Ok(()) => Ok(()),
            Err(forgebase_data::DataError::Unauthenticated) => Err(StorageError::Unauthenticated),
            Err(_) => Err(StorageError::Forbidden("this action needs a storage permission".into())),
        }
    }

    async fn load_acl(&self, bucket: &BucketRow) -> Result<Vec<AclEntry>> {
        let rows = bucket
            .db
            .read(
                "SELECT prefix, grantee, permission FROM _forgebase_storage_acl WHERE bucket = ? ORDER BY prefix, grantee",
                vec![bucket.id.as_str().into()],
            )
            .await?;
        rows.rows
            .iter()
            .map(|r| {
                Ok(AclEntry {
                    prefix: text(r, 0),
                    grantee: text(r, 1),
                    permission: Permission::parse(&text(r, 2))?,
                })
            })
            .collect()
    }

    // ---- quota ---------------------------------------------------------

    /// Changes the `storage.bytes` gauge of `subject` by `delta`. Without an
    /// attached quota engine, or when the catalog has no such feature,
    /// nothing is metered. A refusal is [`StorageError::Entitlement`]
    /// (`quota_exceeded`, HTTP 429).
    pub(crate) async fn meter(&self, subject: &str, delta: i64, idempotency_key: &str) -> Result<()> {
        let (Some(ent), Some(feature)) = (self.inner.ent.get(), self.inner.settings.quota_feature.as_deref()) else {
            return Ok(());
        };
        // Administrators and the system have no billing subject: not metered.
        if delta == 0 || forgebase_entitlements::parse_subject(subject).is_err() {
            return Ok(());
        }
        let req = GaugeRequest {
            feature: feature.to_owned(),
            delta: Some(delta),
            subject: Some(subject.to_owned()),
            idempotency_key: Some(idempotency_key.to_owned()),
            ..GaugeRequest::default()
        };
        match ent.adjust_gauge(&AccessContext::system(), req).await {
            Ok(_) => Ok(()),
            // Not in the catalog: storage is not metered on this instance.
            Err(EntitlementError::NotFound(_)) => Ok(()),
            // A release that would take the gauge below zero (counters were
            // reset): nothing left to give back.
            Err(EntitlementError::Invalid { .. }) if delta < 0 => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The billing subject of a caller.
    pub(crate) fn subject_of(ctx: &AccessContext) -> String {
        subject_key(&ctx.subject())
    }

    // ---- buckets -------------------------------------------------------

    /// The stored bucket `id` (a stored name) on `db`, if any.
    pub(crate) async fn stored_bucket(&self, db: &Db, id: &str) -> Result<Option<BucketRow>> {
        let rows = db
            .read(
                &format!("SELECT {BUCKET_COLS} FROM _forgebase_storage_buckets WHERE name = ?"),
                vec![id.into()],
            )
            .await?;
        rows.rows.first().map(|r| decode_bucket(r, db)).transpose()
    }

    /// The bucket `name` as seen from `tenant` (FB-140, G14): the tenant's
    /// own bucket of that name (on the tenant's set), else the instance-wide
    /// one (on control). A name is never a stored `<tenant>/<name>`, so
    /// another tenant's bucket is unreachable.
    pub(crate) async fn bucket_in(&self, tenant: Option<&str>, name: &str) -> Result<BucketRow> {
        let missing = || StorageError::NotFound(format!("bucket {name}"));
        validate_bucket(name).map_err(|_| missing())?;
        if let Some(t) = tenant {
            let set = self.tenant_set(Some(t)).await?;
            if let Some(b) = self.stored_bucket(&set, &tenant_bucket_id(t, name)).await? {
                return Ok(b);
            }
        }
        match self.stored_bucket(&self.control(), name).await? {
            Some(b) if b.tenant.is_none() => Ok(b),
            _ => Err(missing()),
        }
    }

    /// The bucket `name` as the caller sees it (in the caller's tenant).
    pub(crate) async fn bucket_row(&self, ctx: &AccessContext, name: &str) -> Result<BucketRow> {
        self.bucket_in(ctx.tenant.as_deref(), name).await
    }

    /// Creates a bucket. Needs the `storage_admin` permission (for this
    /// bucket name or every bucket), or an administrator; the caller becomes
    /// the owner. A caller with a tenant creates a bucket of that tenant
    /// (names are unique per tenant, and a tenant bucket may not take the
    /// name of an instance-wide bucket); a caller without one creates an
    /// instance-wide bucket.
    pub async fn create_bucket(&self, ctx: &AccessContext, input: BucketInput) -> Result<Bucket> {
        validate_bucket(&input.name)?;
        if !ctx.is_privileged() {
            let res = ResourceRef::new(ResourceKind::Bucket, input.name.clone(), ResourceOp::Create);
            if self
                .inner
                .data
                .authorizer()
                .authorize_resource(ctx, Action::StorageAdmin, &res)
                != Decision::Allow
            {
                return Err(if ctx.principal == Principal::Anonymous {
                    StorageError::Unauthenticated
                } else {
                    StorageError::Forbidden("this action needs a storage permission".into())
                });
            }
        }
        if input.max_object_bytes == Some(0) {
            return Err(StorageError::invalid(
                "invalid_limit",
                "max_object_bytes must be positive",
            ));
        }
        let allowed = normalise_types(input.allowed_content_types)?;
        let tenant = ctx.tenant.clone();
        let id = tenant
            .as_deref()
            .map_or_else(|| input.name.clone(), |t| tenant_bucket_id(t, &input.name));
        let now = self.now();
        let db = self.tenant_set(tenant.as_deref()).await?;
        if !db.is_control() {
            // Instance-wide names are on control; a tenant bucket may not
            // take one.
            let wide = self
                .control()
                .read(
                    "SELECT 1 FROM _forgebase_storage_buckets WHERE name = ?1 AND tenant IS NULL",
                    vec![input.name.as_str().into()],
                )
                .await?;
            if !wide.is_empty() {
                return Err(StorageError::conflict(
                    "bucket_exists",
                    format!("bucket {} exists", input.name),
                ));
            }
        }
        let mut tx = db.begin().await?;
        let exists = tx
            .fetch_all(&st(
                "SELECT 1 FROM _forgebase_storage_buckets WHERE name = ?1 OR (name = ?2 AND tenant IS NULL)",
                vec![id.as_str().into(), input.name.as_str().into()],
            ))
            .await?;
        if !exists.is_empty() {
            let _ = tx.rollback().await;
            return Err(StorageError::conflict(
                "bucket_exists",
                format!("bucket {} exists", input.name),
            ));
        }
        tx.execute(&st(
            "INSERT INTO _forgebase_storage_buckets \
             (name, tenant, visibility, versioning, max_object_bytes, allowed_content_types, owner, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                id.as_str().into(),
                tenant.map_or(Value::Null, Value::Text),
                input.visibility.as_str().into(),
                input.versioning.into(),
                input.max_object_bytes.map_or(Value::Null, iv),
                allowed.as_ref().map_or(Value::Null, |a| {
                    Value::Text(serde_json::to_string(a).unwrap_or_else(|_| "[]".into()))
                }),
                ctx.principal_key().into(),
                now.into(),
                now.into(),
            ],
        ))
        .await?;
        tx.commit().await?;
        self.stored_bucket(&db, &id)
            .await?
            .map(|b| b.wire())
            .ok_or_else(|| StorageError::Internal("bucket vanished".into()))
    }

    /// Lists the buckets the caller may read: those of the caller's tenant
    /// and the instance-wide ones (one of which a tenant bucket of the same
    /// name hides).
    pub async fn list_buckets(&self, ctx: &AccessContext) -> Result<Vec<Bucket>> {
        let decode_all = |rows: &Rows, db: &Db| {
            rows.rows
                .iter()
                .map(|r| decode_bucket(r, db))
                .collect::<Result<Vec<_>>>()
        };
        let wide_sql =
            format!("SELECT {BUCKET_COLS} FROM _forgebase_storage_buckets WHERE tenant IS NULL ORDER BY name");
        let all = match &ctx.tenant {
            Some(t) => {
                let set = self.tenant_set(Some(t)).await?;
                if set.is_control() {
                    let rows = set
                        .read_replica(
                            "storage.list_buckets",
                            &format!(
                                "SELECT {BUCKET_COLS} FROM _forgebase_storage_buckets WHERE tenant = ?1 OR tenant IS NULL ORDER BY name"
                            ),
                            vec![t.as_str().into()],
                        )
                        .await?;
                    decode_all(&rows, &set)?
                } else {
                    // The tenant's buckets on its set, the instance-wide ones on control.
                    let own = set
                        .read_replica(
                            "storage.list_buckets",
                            &format!(
                                "SELECT {BUCKET_COLS} FROM _forgebase_storage_buckets WHERE tenant = ?1 ORDER BY name"
                            ),
                            vec![t.as_str().into()],
                        )
                        .await?;
                    let control = self.control();
                    let wide = control.read_replica("storage.list_buckets", &wide_sql, vec![]).await?;
                    let mut all = decode_all(&own, &set)?;
                    all.extend(decode_all(&wide, &control)?);
                    all
                }
            }
            None => {
                let control = self.control();
                let rows = control.read_replica("storage.list_buckets", &wide_sql, vec![]).await?;
                decode_all(&rows, &control)?
            }
        };
        let mine: std::collections::BTreeSet<String> = all
            .iter()
            .filter(|b| b.tenant.is_some())
            .map(|b| b.name.clone())
            .collect();
        let mut out = Vec::new();
        for b in all {
            if b.tenant.is_none() && mine.contains(&b.name) {
                continue;
            }
            if self
                .authorize(ctx, &b, None, Need::Read, ResourceOp::List)
                .await
                .is_ok()
            {
                out.push(b.wire());
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// One bucket (needs read access).
    pub async fn get_bucket(&self, ctx: &AccessContext, name: &str) -> Result<Bucket> {
        let b = self.bucket_row(ctx, name).await?;
        self.authorize(ctx, &b, None, Need::Read, ResourceOp::View).await?;
        Ok(b.wire())
    }

    /// Changes a bucket's policy (needs admin access on it).
    pub async fn update_bucket(&self, ctx: &AccessContext, name: &str, patch: BucketPatch) -> Result<Bucket> {
        let mut b = self.bucket_row(ctx, name).await?;
        self.authorize(ctx, &b, None, Need::Admin, ResourceOp::Update).await?;
        if let Some(v) = patch.visibility {
            b.visibility = v;
        }
        if let Some(v) = patch.versioning {
            b.versioning = v;
        }
        if let Some(m) = patch.max_object_bytes {
            b.max_object_bytes = (m > 0).then_some(m);
        }
        if let Some(t) = patch.allowed_content_types {
            b.allowed = normalise_types(Some(t))?;
        }
        let mut tx = b.db.begin().await?;
        tx.execute(&st(
            "UPDATE _forgebase_storage_buckets SET visibility = ?, versioning = ?, max_object_bytes = ?, \
             allowed_content_types = ?, updated_at = ? WHERE name = ?",
            vec![
                b.visibility.as_str().into(),
                b.versioning.into(),
                b.max_object_bytes.map_or(Value::Null, iv),
                b.allowed.as_ref().map_or(Value::Null, |a| {
                    Value::Text(serde_json::to_string(a).unwrap_or_else(|_| "[]".into()))
                }),
                self.now().into(),
                b.id.as_str().into(),
            ],
        ))
        .await?;
        tx.commit().await?;
        self.stored_bucket(&b.db, &b.id)
            .await?
            .map(|b| b.wire())
            .ok_or_else(|| StorageError::NotFound(format!("bucket {name}")))
    }

    /// The ACL of a bucket (needs admin access on it).
    pub async fn get_acl(&self, ctx: &AccessContext, name: &str) -> Result<Acl> {
        let b = self.bucket_row(ctx, name).await?;
        self.authorize(ctx, &b, None, Need::Admin, ResourceOp::View).await?;
        Ok(Acl {
            entries: self.load_acl(&b).await?,
        })
    }

    /// Replaces the ACL of a bucket (needs admin access on it).
    pub async fn set_acl(&self, ctx: &AccessContext, name: &str, acl: Acl) -> Result<Acl> {
        let b = self.bucket_row(ctx, name).await?;
        self.authorize(ctx, &b, None, Need::Admin, ResourceOp::Update).await?;
        for e in &acl.entries {
            validate_grantee(&e.grantee)?;
            if e.prefix.len() > 1024 || e.prefix.chars().any(char::is_control) {
                return Err(StorageError::invalid("invalid_acl", "invalid ACL prefix"));
            }
        }
        let now = self.now();
        let mut tx = b.db.begin().await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_acl WHERE bucket = ?",
            vec![b.id.as_str().into()],
        ))
        .await?;
        for e in &acl.entries {
            tx.execute(&st(
                "INSERT INTO _forgebase_storage_acl (bucket, prefix, grantee, permission, created_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (bucket, prefix, grantee) DO UPDATE SET permission = excluded.permission",
                vec![
                    b.id.as_str().into(),
                    e.prefix.as_str().into(),
                    e.grantee.as_str().into(),
                    e.permission.as_str().into(),
                    now.into(),
                ],
            ))
            .await?;
        }
        tx.commit().await?;
        Ok(Acl {
            entries: self.load_acl(&b).await?,
        })
    }

    /// Deletes a bucket. A bucket with objects, versions or open uploads is
    /// refused unless `force` is set, which removes them all.
    pub async fn delete_bucket(&self, ctx: &AccessContext, name: &str, force: bool) -> Result<()> {
        let b = self.bucket_row(ctx, name).await?;
        self.authorize(ctx, &b, None, Need::Admin, ResourceOp::Delete).await?;
        let name = b.id.as_str();
        let busy =
            b.db.read(
                "SELECT (SELECT COUNT(*) FROM _forgebase_storage_objects WHERE bucket = ?1) \
                      + (SELECT COUNT(*) FROM _forgebase_storage_uploads WHERE bucket = ?1)",
                vec![name.into()],
            )
            .await?;
        let busy = busy.scalar().and_then(Value::as_i64).unwrap_or(0);
        if busy > 0 && !force {
            return Err(StorageError::conflict(
                "bucket_not_empty",
                format!("bucket {} holds objects or uploads; delete them or pass force", b.name),
            ));
        }
        let now = self.now();
        let mut released: Vec<(String, String, i64)> = Vec::new();
        let mut staged: Vec<String> = Vec::new();
        let gate = self.inner.gate.read().await;
        let mut tx = b.db.begin().await?;
        let versions = tx
            .fetch_all(&st(
                "SELECT id, blob_sha, size, quota_subject FROM _forgebase_storage_objects WHERE bucket = ?",
                vec![name.into()],
            ))
            .await?;
        for r in &versions.rows {
            if let Some(sha) = opt_text(r, 1) {
                tx.execute(&st(
                    "UPDATE _forgebase_storage_blobs SET refcount = refcount - 1, \
                     zero_since = CASE WHEN refcount - 1 = 0 THEN ? ELSE zero_since END WHERE sha256 = ?",
                    vec![now.into(), sha.into()],
                ))
                .await?;
                released.push((text(r, 0), text(r, 3), int(r, 2)));
            }
        }
        let parts = tx
            .fetch_all(&st(
                "SELECT p.staging_key FROM _forgebase_storage_parts p \
                 JOIN _forgebase_storage_uploads u ON u.id = p.upload_id WHERE u.bucket = ?",
                vec![name.into()],
            ))
            .await?;
        staged.extend(parts.rows.iter().map(|r| text(r, 0)));
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_uploads WHERE bucket = ?",
            vec![name.into()],
        ))
        .await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_objects WHERE bucket = ?",
            vec![name.into()],
        ))
        .await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_buckets WHERE name = ?",
            vec![name.into()],
        ))
        .await?;
        tx.commit().await?;
        drop(gate);
        for key in staged {
            let _ = self.inner.driver.delete(&key).await;
        }
        for (id, subject, size) in released {
            let _ = self.meter(&subject, -size, &format!("storage:{id}:rm")).await;
        }
        Ok(())
    }

    /// Creates the reconciler schedule (hourly) unless it exists.
    pub async fn ensure_schedule(&self) -> Result<()> {
        let Some(jobs) = self.inner.jobs.get() else {
            return Ok(());
        };
        let new = forgebase_jobs::NewSchedule {
            name: RECONCILE_SCHEDULE.to_owned(),
            cron: "0 17 * * * *".to_owned(),
            kind: RECONCILE_JOB.to_owned(),
            queue: None,
            payload: serde_json::json!({}),
            priority: Some(1),
            max_attempts: Some(3),
            backoff_base_ms: None,
            backoff_max_ms: None,
            visibility_timeout_ms: None,
            enabled: Some(true),
        };
        match jobs.create_schedule(&AccessContext::system(), new).await {
            Ok(_) | Err(forgebase_jobs::JobError::ScheduleExists(_)) => Ok(()),
            Err(e) => Err(StorageError::Internal(format!("reconcile schedule: {e}"))),
        }
    }
}

/// Columns [`decode_bucket`] reads.
pub(crate) const BUCKET_COLS: &str =
    "name, visibility, versioning, max_object_bytes, allowed_content_types, owner, created_at, updated_at, tenant";

fn decode_bucket(r: &[Value], db: &Db) -> Result<BucketRow> {
    let id = text(r, 0);
    let tenant = opt_text(r, 8);
    let name = match &tenant {
        Some(_) => id.rsplit('/').next().unwrap_or_default().to_owned(),
        None => id.clone(),
    };
    Ok(BucketRow {
        id,
        name,
        tenant,
        db: db.clone(),
        visibility: Visibility::parse(&text(r, 1))?,
        versioning: int(r, 2) == 1,
        max_object_bytes: opt_int(r, 3).and_then(|n| u64::try_from(n).ok()),
        allowed: opt_text(r, 4).and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()),
        owner: text(r, 5),
        created_at: int(r, 6),
        updated_at: int(r, 7),
    })
}

/// Stored name of tenant `tenant`'s bucket `name`.
pub(crate) fn tenant_bucket_id(tenant: &str, name: &str) -> String {
    format!("{tenant}/{name}")
}

/// The name the API shows for a stored bucket name.
pub(crate) fn public_bucket(id: &str) -> String {
    id.rsplit('/').next().unwrap_or_default().to_owned()
}

/// Whether an ACL entry for `prefix` covers `key`; `None` is the bucket
/// itself, which only an entry with the empty prefix covers (G1: an entry
/// for `users/alice/` grants nothing on the bucket).
fn acl_covers(prefix: &str, key: Option<&str>) -> bool {
    match key {
        None => prefix.is_empty(),
        Some(k) => k.starts_with(prefix),
    }
}

fn normalise_types(t: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    let Some(list) = t else { return Ok(None) };
    if list.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for item in list {
        let item = item.trim().to_ascii_lowercase();
        let ok = item.len() <= 100
            && item
                .split_once('/')
                .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty());
        if !ok {
            return Err(StorageError::invalid(
                "invalid_content_type",
                "allowed content types look like image/png or image/*",
            ));
        }
        out.push(item);
    }
    Ok(Some(out))
}

fn validate_grantee(g: &str) -> Result<()> {
    let ok = matches!(g, "public" | "authenticated")
        || g.strip_prefix("user:")
            .is_some_and(|r| r.contains('/') && r.len() <= 200)
        || g.strip_prefix("service:")
            .is_some_and(|r| !r.is_empty() && r.len() <= 200)
        || g.strip_prefix("role:").is_some_and(|r| !r.is_empty() && r.len() <= 200);
    if ok {
        Ok(())
    } else {
        Err(StorageError::invalid(
            "invalid_grantee",
            "a grantee is user:<collection>/<id>, service:<id>, role:<name>, authenticated or public",
        ))
    }
}

/// Whether `content_type` matches an allow list entry (`image/*` or exact).
pub(crate) fn type_allowed(allowed: &Option<Vec<String>>, content_type: &str) -> bool {
    let Some(list) = allowed else { return true };
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    list.iter().any(|a| match a.strip_suffix("/*") {
        Some(major) => ct.split_once('/').is_some_and(|(m, _)| m == major),
        None => *a == ct,
    })
}

/// Settings row of the sealed URL signing key (FB-140, G11).
const SEALED_KEY_ROW: &str = "signing_key_sealed";
/// Settings row of the plain-text key written before FB-140.
const PLAIN_KEY_ROW: &str = "signing_key";
/// Additional data binding the sealed key to its purpose.
const SEALED_KEY_AAD: &[u8] = b"forgebase/storage/url-signing-key/v1";

fn seal_key(keyring: &Keyring, key: &[u8]) -> Result<String> {
    keyring
        .seal(key, SEALED_KEY_AAD)
        .map(|b| crate::util::hex(&b))
        .map_err(|e| StorageError::Internal(format!("cannot seal the URL signing key: {e}")))
}

fn open_key(keyring: &Keyring, sealed: &str) -> Result<Vec<u8>> {
    let bytes =
        crate::util::unhex(sealed).ok_or_else(|| StorageError::Internal("sealed signing key is not hex".into()))?;
    let key = keyring.open(&bytes, SEALED_KEY_AAD).map_err(|e| {
        StorageError::Internal(format!(
            "cannot open the URL signing key ({e}); restore the master key, or delete the \
             `{SEALED_KEY_ROW}` row of _forgebase_storage_settings to start a new key (signed URLs issued so far stop working)"
        ))
    })?;
    if key.len() == 32 {
        Ok(key)
    } else {
        Err(StorageError::Internal("sealed signing key has the wrong length".into()))
    }
}

fn random_key() -> Result<Vec<u8>> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| StorageError::Internal(format!("no randomness: {e}")))?;
    Ok(key.to_vec())
}

async fn setting(db: &dyn Backend, name: &str) -> Result<Option<String>> {
    let rows = db
        .fetch_all(&st(
            "SELECT value FROM _forgebase_storage_settings WHERE name = ?",
            vec![name.into()],
        ))
        .await?;
    Ok(rows.scalar().and_then(Value::as_str).map(str::to_owned))
}

/// Loads the URL signing key, sealed with the auth keyring (G11).
///
/// * A sealed key is opened (a master key that does not open it is an error:
///   nothing is overwritten).
/// * A plain-text key from before FB-140 is sealed **unchanged** and its
///   plain row deleted, in one transaction, so every signed URL issued with
///   it stays valid until it expires.
/// * Otherwise a new random key is sealed. Two openers racing: the first
///   insert wins and both read it back.
async fn load_signing_key(db: &dyn Backend, keyring: &Keyring) -> Result<Vec<u8>> {
    if let Some(sealed) = setting(db, SEALED_KEY_ROW).await? {
        let key = open_key(keyring, &sealed)?;
        // A plain copy left by an interrupted migration: drop it.
        if setting(db, PLAIN_KEY_ROW).await?.is_some() {
            let mut tx = db.begin(TxMode::Normal).await?;
            tx.execute(&st(
                "DELETE FROM _forgebase_storage_settings WHERE name = ?",
                vec![PLAIN_KEY_ROW.into()],
            ))
            .await?;
            tx.commit().await?;
        }
        return Ok(key);
    }
    let plain = setting(db, PLAIN_KEY_ROW)
        .await?
        .and_then(|v| crate::util::unhex(&v))
        .filter(|v| v.len() == 32);
    let key = match plain {
        Some(k) => k,
        None => random_key()?,
    };
    let sealed = seal_key(keyring, &key)?;
    let mut tx = db.begin(TxMode::Normal).await?;
    let insert = db
        .dialect()
        .insert_ignore("_forgebase_storage_settings", "name, value", "VALUES (?1, ?2)");
    tx.execute(&st(&insert, vec![SEALED_KEY_ROW.into(), sealed.into()]))
        .await?;
    tx.execute(&st(
        "DELETE FROM _forgebase_storage_settings WHERE name = ?",
        vec![PLAIN_KEY_ROW.into()],
    ))
    .await?;
    tx.commit().await?;
    let stored = setting(db, SEALED_KEY_ROW)
        .await?
        .ok_or_else(|| StorageError::Internal("signing key unreadable".into()))?;
    open_key(keyring, &stored)
}

impl StorageService {
    /// The URL signing key in force.
    pub(crate) fn signing_key(&self) -> Arc<Vec<u8>> {
        self.inner
            .signing_key
            .read()
            .map_or_else(|p| p.into_inner().clone(), |k| k.clone())
    }

    /// Replaces the URL signing key with a new random one (administrators
    /// only; G15). Every signed URL issued so far stops working at once
    /// (`403 signature_invalid`): this is how signed URLs are revoked.
    pub async fn rotate_signing_key(&self, ctx: &AccessContext) -> Result<()> {
        if !ctx.is_privileged() {
            return Err(if ctx.principal == Principal::Anonymous {
                StorageError::Unauthenticated
            } else {
                StorageError::Forbidden("only administrators rotate the URL signing key".into())
            });
        }
        let key = random_key()?;
        let sealed = seal_key(&self.inner.keyring, &key)?;
        let mut tx = self.control().begin().await?;
        tx.execute(&st(
            "UPDATE _forgebase_storage_settings SET value = ? WHERE name = ?",
            vec![sealed.into(), SEALED_KEY_ROW.into()],
        ))
        .await?;
        tx.commit().await?;
        if let Ok(mut k) = self.inner.signing_key.write() {
            *k = Arc::new(key);
        }
        tracing::info!(actor = %ctx.actor(), "storage URL signing key rotated; earlier signed URLs no longer work");
        Ok(())
    }
}

pub(crate) fn id() -> String {
    new_id()
}
