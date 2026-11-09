//! The storage service: state, authorization, buckets and ACLs. Objects,
//! uploads, signed URLs and the reconciler extend it in their own modules.

use std::sync::{Arc, OnceLock};

use forgebase_data::{AccessContext, Action, DataService, Principal};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_entitlements::{Clock, EntitlementError, EntitlementService, GaugeRequest, subject_key};
use forgebase_jobs::JobService;

use crate::driver::StorageDriver;
use crate::error::{Result, StorageError};
use crate::model::{
    Acl, AclEntry, Bucket, BucketInput, BucketPatch, Permission, Visibility,
};
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
    pub signing_key: Vec<u8>,
}

/// A bucket as stored.
#[derive(Debug, Clone)]
pub(crate) struct BucketRow {
    pub name: String,
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
    /// key on first use.
    pub async fn open(
        db: Arc<dyn Backend>,
        data: DataService,
        driver: Arc<dyn StorageDriver>,
        clock: Arc<dyn Clock>,
        settings: StorageSettings,
    ) -> Result<Self> {
        let signing_key = load_signing_key(db.as_ref(), clock.now_ms()).await?;
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
                signing_key,
            }),
        })
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

    pub(crate) async fn read(&self, sql: &str, params: Vec<Value>) -> Result<Rows> {
        Ok(self.inner.db.fetch_all(&st(sql, params)).await?)
    }

    pub(crate) async fn begin(&self) -> Result<Box<dyn WriteTx>> {
        Ok(self.inner.db.begin(TxMode::Normal).await?)
    }

    // ---- authorization -------------------------------------------------

    /// Decides whether `ctx` may do `need` on `bucket` (and `key`).
    ///
    /// Order: administrators and the system; the bucket's owner; the
    /// bucket's visibility (reads); ACL grants on the bucket or a prefix of
    /// the key; finally the platform permission `storage_read` /
    /// `storage_write` / `storage_admin`, which goes through the
    /// [`Authorizer`](forgebase_data::Authorizer) (the policy engine), so
    /// roles, entitlements and quotas of the authorization pipeline apply.
    pub(crate) async fn authorize(
        &self,
        ctx: &AccessContext,
        bucket: &BucketRow,
        key: Option<&str>,
        need: Need,
    ) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        let anonymous = ctx.principal == Principal::Anonymous;
        if !anonymous && ctx.principal_key() == bucket.owner {
            return Ok(());
        }
        if need == Need::Read {
            match bucket.visibility {
                Visibility::Public => return Ok(()),
                Visibility::Authenticated if !anonymous => return Ok(()),
                _ => {}
            }
        }
        let acl = self.load_acl(&bucket.name).await?;
        let me = ctx.principal_key();
        let granted = acl.iter().any(|e| {
            e.permission >= need.permission()
                && key.is_none_or(|k| k.starts_with(&e.prefix))
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
        });
        if granted {
            return Ok(());
        }
        if anonymous {
            return Err(StorageError::Unauthenticated);
        }
        match self.inner.data.authorize_action(ctx, need.action()) {
            Ok(()) => Ok(()),
            Err(forgebase_data::DataError::Unauthenticated) => Err(StorageError::Unauthenticated),
            Err(_) => Err(StorageError::Forbidden(format!(
                "you may not {} bucket {}",
                match need {
                    Need::Read => "read",
                    Need::Write => "write to",
                    Need::Admin => "administer",
                },
                bucket.name
            ))),
        }
    }

    /// Platform-level permission without a bucket (create buckets, run the
    /// reconciler, read statistics).
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

    async fn load_acl(&self, bucket: &str) -> Result<Vec<AclEntry>> {
        let rows = self
            .read(
                "SELECT prefix, grantee, permission FROM _forgebase_storage_acl WHERE bucket = ? ORDER BY prefix, grantee",
                vec![bucket.into()],
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
        if delta == 0 {
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

    pub(crate) async fn bucket_row(&self, name: &str) -> Result<BucketRow> {
        let rows = self
            .read(
                "SELECT name, visibility, versioning, max_object_bytes, allowed_content_types, owner, created_at, updated_at \
                 FROM _forgebase_storage_buckets WHERE name = ?",
                vec![name.into()],
            )
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| StorageError::NotFound(format!("bucket {name}")))?;
        decode_bucket(r)
    }

    /// Creates a bucket. Needs the `storage_admin` platform permission (or an
    /// administrator); the caller becomes the owner.
    pub async fn create_bucket(&self, ctx: &AccessContext, input: BucketInput) -> Result<Bucket> {
        validate_bucket(&input.name)?;
        self.authorize_platform(ctx, Action::StorageAdmin)?;
        if input.max_object_bytes == Some(0) {
            return Err(StorageError::invalid("invalid_limit", "max_object_bytes must be positive"));
        }
        let allowed = normalise_types(input.allowed_content_types)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let exists = tx
            .fetch_all(&st(
                "SELECT 1 FROM _forgebase_storage_buckets WHERE name = ?",
                vec![input.name.as_str().into()],
            ))
            .await?;
        if !exists.is_empty() {
            let _ = tx.rollback().await;
            return Err(StorageError::conflict("bucket_exists", format!("bucket {} exists", input.name)));
        }
        tx.execute(&st(
            "INSERT INTO _forgebase_storage_buckets \
             (name, visibility, versioning, max_object_bytes, allowed_content_types, owner, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                input.name.as_str().into(),
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
        self.bucket_row(&input.name).await.map(|b| b.wire())
    }

    /// Lists the buckets the caller may read.
    pub async fn list_buckets(&self, ctx: &AccessContext) -> Result<Vec<Bucket>> {
        let rows = self
            .read(
                "SELECT name, visibility, versioning, max_object_bytes, allowed_content_types, owner, created_at, updated_at \
                 FROM _forgebase_storage_buckets ORDER BY name",
                vec![],
            )
            .await?;
        let mut out = Vec::new();
        for r in &rows.rows {
            let b = decode_bucket(r)?;
            if self.authorize(ctx, &b, None, Need::Read).await.is_ok() {
                out.push(b.wire());
            }
        }
        Ok(out)
    }

    /// One bucket (needs read access).
    pub async fn get_bucket(&self, ctx: &AccessContext, name: &str) -> Result<Bucket> {
        let b = self.bucket_row(name).await?;
        self.authorize(ctx, &b, None, Need::Read).await?;
        Ok(b.wire())
    }

    /// Changes a bucket's policy (needs admin access on it).
    pub async fn update_bucket(&self, ctx: &AccessContext, name: &str, patch: BucketPatch) -> Result<Bucket> {
        let mut b = self.bucket_row(name).await?;
        self.authorize(ctx, &b, None, Need::Admin).await?;
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
        let mut tx = self.begin().await?;
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
                name.into(),
            ],
        ))
        .await?;
        tx.commit().await?;
        self.bucket_row(name).await.map(|b| b.wire())
    }

    /// The ACL of a bucket (needs admin access on it).
    pub async fn get_acl(&self, ctx: &AccessContext, name: &str) -> Result<Acl> {
        let b = self.bucket_row(name).await?;
        self.authorize(ctx, &b, None, Need::Admin).await?;
        Ok(Acl {
            entries: self.load_acl(name).await?,
        })
    }

    /// Replaces the ACL of a bucket (needs admin access on it).
    pub async fn set_acl(&self, ctx: &AccessContext, name: &str, acl: Acl) -> Result<Acl> {
        let b = self.bucket_row(name).await?;
        self.authorize(ctx, &b, None, Need::Admin).await?;
        for e in &acl.entries {
            validate_grantee(&e.grantee)?;
            if e.prefix.len() > 1024 || e.prefix.chars().any(char::is_control) {
                return Err(StorageError::invalid("invalid_acl", "invalid ACL prefix"));
            }
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_acl WHERE bucket = ?",
            vec![name.into()],
        ))
        .await?;
        for e in &acl.entries {
            tx.execute(&st(
                "INSERT INTO _forgebase_storage_acl (bucket, prefix, grantee, permission, created_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (bucket, prefix, grantee) DO UPDATE SET permission = excluded.permission",
                vec![
                    name.into(),
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
            entries: self.load_acl(name).await?,
        })
    }

    /// Deletes a bucket. A bucket with objects, versions or open uploads is
    /// refused unless `force` is set, which removes them all.
    pub async fn delete_bucket(&self, ctx: &AccessContext, name: &str, force: bool) -> Result<()> {
        let b = self.bucket_row(name).await?;
        self.authorize(ctx, &b, None, Need::Admin).await?;
        let busy = self
            .read(
                "SELECT (SELECT COUNT(*) FROM _forgebase_storage_objects WHERE bucket = ?1) \
                      + (SELECT COUNT(*) FROM _forgebase_storage_uploads WHERE bucket = ?1)",
                vec![name.into()],
            )
            .await?;
        let busy = busy.scalar().and_then(Value::as_i64).unwrap_or(0);
        if busy > 0 && !force {
            return Err(StorageError::conflict(
                "bucket_not_empty",
                format!("bucket {name} holds objects or uploads; delete them or pass force"),
            ));
        }
        let now = self.now();
        let mut released: Vec<(String, String, i64)> = Vec::new();
        let mut staged: Vec<String> = Vec::new();
        let gate = self.inner.gate.read().await;
        let mut tx = self.begin().await?;
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
        tx.execute(&st("DELETE FROM _forgebase_storage_uploads WHERE bucket = ?", vec![name.into()]))
            .await?;
        tx.execute(&st("DELETE FROM _forgebase_storage_objects WHERE bucket = ?", vec![name.into()]))
            .await?;
        tx.execute(&st("DELETE FROM _forgebase_storage_buckets WHERE name = ?", vec![name.into()]))
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

fn decode_bucket(r: &[Value]) -> Result<BucketRow> {
    Ok(BucketRow {
        name: text(r, 0),
        visibility: Visibility::parse(&text(r, 1))?,
        versioning: int(r, 2) == 1,
        max_object_bytes: opt_int(r, 3).and_then(|n| u64::try_from(n).ok()),
        allowed: opt_text(r, 4).and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok()),
        owner: text(r, 5),
        created_at: int(r, 6),
        updated_at: int(r, 7),
    })
}

fn normalise_types(t: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    let Some(list) = t else { return Ok(None) };
    if list.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for item in list {
        let item = item.trim().to_ascii_lowercase();
        let ok = item.len() <= 100 && item.split_once('/').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty());
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
        || g.strip_prefix("user:").is_some_and(|r| r.contains('/') && r.len() <= 200)
        || g.strip_prefix("service:").is_some_and(|r| !r.is_empty() && r.len() <= 200)
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

async fn load_signing_key(db: &dyn Backend, now: i64) -> Result<Vec<u8>> {
    let existing = db
        .fetch_all(&st(
            "SELECT value FROM _forgebase_storage_settings WHERE name = 'signing_key'",
            vec![],
        ))
        .await?;
    if let Some(v) = existing.scalar().and_then(Value::as_str).and_then(crate::util::unhex) {
        if v.len() == 32 {
            return Ok(v);
        }
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| StorageError::Internal(format!("no randomness: {e}")))?;
    let mut tx = db.begin(TxMode::Normal).await?;
    // Two openers racing: the first insert wins, both read it back.
    tx.execute(&st(
        "INSERT OR IGNORE INTO _forgebase_storage_settings (name, value) VALUES ('signing_key', ?)",
        vec![crate::util::hex(&key).into()],
    ))
    .await?;
    let _ = now;
    tx.commit().await?;
    let stored = db
        .fetch_all(&st(
            "SELECT value FROM _forgebase_storage_settings WHERE name = 'signing_key'",
            vec![],
        ))
        .await?;
    stored
        .scalar()
        .and_then(Value::as_str)
        .and_then(crate::util::unhex)
        .filter(|v| v.len() == 32)
        .ok_or_else(|| StorageError::Internal("signing key unreadable".into()))
}

pub(crate) fn id() -> String {
    new_id()
}
