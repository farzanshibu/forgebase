//! Test harness: migrated database, policy + entitlements, storage on a
//! local driver, a manual clock (starting at the real time, so file mtimes
//! and the clock agree), and the real HTTP router (no sockets).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService};
use forgebase_db::testing::TestDb;
use forgebase_db::{Backend, Database};
use forgebase_entitlements::{
    EntitlementService, FeatureInput, FeatureKind, ManualClock, PlanFeature, PlanInput, SubscriptionStatus,
    SubscriptionSync,
};
use forgebase_policy::PolicyService;
use forgebase_storage::{BucketInput, LocalDriver, StorageDriver, StorageService, StorageSettings};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

pub struct Env {
    pub db: Database,
    pub data: DataService,
    pub policy: PolicyService,
    pub ent: EntitlementService,
    pub storage: StorageService,
    pub driver: Arc<dyn StorageDriver>,
    pub clock: ManualClock,
    pub app: Router,
    pub tmp: tempfile::TempDir,
    pub test: TestDb,
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn user(id: &str) -> AccessContext {
    AccessContext::user("people", id)
}

pub fn pk(id: &str) -> String {
    format!("user:people/{id}")
}

fn real_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap())
        .unwrap()
}

impl Env {
    pub async fn new() -> Self {
        Self::with(|_| {}, None).await
    }

    pub async fn with(tune: impl FnOnce(&mut StorageSettings), driver: Option<Arc<dyn StorageDriver>>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let test = forgebase_db::testing::test_backend().await;
        let db = test.database().clone();
        let backend: Arc<dyn Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let clock = ManualClock::at(real_now());
        let ent = EntitlementService::open(backend.clone(), policy.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        policy.set_hooks(ent.hooks());
        let driver: Arc<dyn StorageDriver> = driver.unwrap_or_else(|| Arc::new(LocalDriver::new(layout.storage_dir())));
        let mut settings = StorageSettings {
            reconcile_grace_secs: 0,
            ..StorageSettings::default()
        };
        tune(&mut settings);
        let keyring = forgebase_auth::crypto::Keyring::from_master(&[9u8; 32]);
        let storage = StorageService::open(
            backend,
            data.clone(),
            driver.clone(),
            Arc::new(clock.clone()),
            settings,
            keyring,
        )
        .await
        .unwrap();
        storage.attach_entitlements(ent.clone());
        data.create_collection(
            &AccessContext::system(),
            serde_json::from_value(json!({"name": "people", "fields": [{"name": "email", "type": "text"}]})).unwrap(),
        )
        .await
        .unwrap();
        let state = AppState::from_data(db.clone(), layout, data.clone());
        let app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![forgebase_storage::extension(storage.clone())],
                ..RouterOptions::default()
            },
        );
        Self {
            db,
            data,
            policy,
            ent,
            storage,
            driver,
            clock,
            app,
            tmp,
            test,
        }
    }

    pub async fn bucket(&self, name: &str, versioning: bool) {
        self.storage
            .create_bucket(
                &admin(),
                BucketInput {
                    name: name.into(),
                    versioning,
                    ..BucketInput::default()
                },
            )
            .await
            .unwrap();
    }

    /// A gauge feature `storage.bytes` with `limit` bytes for `subject`.
    pub async fn quota(&self, subject: &str, limit: i64) {
        self.ent
            .put_feature(
                &admin(),
                "storage.bytes",
                FeatureInput {
                    name: None,
                    description: None,
                    kind: FeatureKind::Gauge,
                    unit: None,
                    permissions: vec![],
                },
            )
            .await
            .unwrap();
        self.ent
            .put_plan(
                &admin(),
                "small",
                PlanInput {
                    name: None,
                    description: None,
                    is_default: false,
                    active: true,
                    trial_days: 0,
                    metadata: None,
                    features: vec![PlanFeature {
                        feature: "storage.bytes".into(),
                        enabled: true,
                        limit: Some(limit),
                        unit: String::new(),
                        period: None,
                        window_secs: None,
                        trial_limit: None,
                    }],
                },
            )
            .await
            .unwrap();
        self.ent
            .apply_subscription(
                &admin(),
                SubscriptionSync {
                    subject: subject.into(),
                    plan: "small".into(),
                    status: SubscriptionStatus::Active,
                    trial_ends_at: None,
                    current_period_start: None,
                    current_period_end: None,
                    cycle_anchor: None,
                    cancel_at: None,
                    external_ref: None,
                    addons: None,
                    event_id: None,
                    event_at: None,
                },
            )
            .await
            .unwrap();
    }

    pub async fn req(
        &self,
        ctx: Option<AccessContext>,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Body,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut req = req.body(body).unwrap();
        if let Some(ctx) = ctx {
            req.extensions_mut().insert(ctx);
        }
        let res = self.app.clone().oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 28).await.unwrap();
        (status, headers, bytes.to_vec())
    }

    pub async fn json(&self, ctx: Option<AccessContext>, method: Method, uri: &str, body: Json) -> (StatusCode, Json) {
        let (s, _, b) = self
            .req(
                ctx,
                method,
                uri,
                &[("content-type", "application/json")],
                Body::from(body.to_string()),
            )
            .await;
        (s, serde_json::from_slice(&b).unwrap_or(Json::Null))
    }
}

pub fn bytes_of(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from((i * 31 + i / 7) % 251).unwrap()).collect()
}

pub fn sha(data: &[u8]) -> String {
    forgebase_storage::util::sha256_hex(data)
}

pub fn put(bucket: &str, key: &str, data: &[u8]) -> forgebase_storage::PutRequest {
    forgebase_storage::PutRequest {
        bucket: bucket.into(),
        key: key.into(),
        content_type: None,
        metadata: Default::default(),
        expected_sha256: None,
        content_length: None,
        body: forgebase_storage::body_of(data.to_vec()),
    }
}

/// A body that delivers `data` in `chunk`-byte pieces.
pub fn chunked(data: Vec<u8>, chunk: usize) -> forgebase_storage::ByteStream {
    let pieces: Vec<std::io::Result<axum::body::Bytes>> = data
        .chunks(chunk)
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    Box::pin(futures_util::stream::iter(pieces))
}

/// A body that delivers `data` and then fails, like a dropped connection.
pub fn interrupted(data: Vec<u8>) -> forgebase_storage::ByteStream {
    let items: Vec<std::io::Result<axum::body::Bytes>> = vec![
        Ok(axum::body::Bytes::from(data)),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset",
        )),
    ];
    Box::pin(futures_util::stream::iter(items))
}

pub async fn read_all(r: forgebase_storage::ObjectRead) -> Vec<u8> {
    forgebase_storage::driver::collect(r.stream).await.unwrap()
}
