//! Shared harness: a real SQLite database in a temp dir, migrated, with the
//! policy engine as the authorizer, the quota engine, realtime (for the
//! outbox that feeds record sources) and the queue service on a manual clock.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forgebase_auth::crypto::Keyring;
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService};
use forgebase_db::{Backend, Database, DbOptions};
use forgebase_entitlements::{
    EntitlementService, FeatureInput, FeatureKind, PlanFeature, PlanInput, SubscriptionStatus, SubscriptionSync,
};
use forgebase_jobs::ManualClock;
use forgebase_policy::PolicyService;
use forgebase_policy::snapshot::Effect;
use forgebase_queue::{
    NewMessage, NewSubscription, NewTopic, PullRequest, QueueService, QueueSettings, ReceivedMessage, Subscription,
};
use forgebase_realtime::{RealtimeConfig, RealtimeService};
use serde_json::Value as Json;

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn user(id: &str) -> AccessContext {
    AccessContext::user("users", id)
}

pub fn real_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

pub fn settings() -> QueueSettings {
    QueueSettings {
        poll_interval: Duration::from_millis(20),
        allow_http_push: true,
        ..QueueSettings::default()
    }
}

pub struct H {
    pub db: Database,
    pub data: DataService,
    pub policy: PolicyService,
    pub ent: EntitlementService,
    pub rt: RealtimeService,
    pub svc: QueueService,
    pub clock: ManualClock,
    pub keyring: Keyring,
    pub dir: tempfile::TempDir,
}

pub async fn open_db(dir: &Path) -> Database {
    let layout = DataLayout::new(dir.join("data"));
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(dir);
    let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
        .await
        .unwrap();
    db.migrate(Some(&layout.backups_dir())).await.unwrap();
    db
}

pub async fn harness() -> H {
    harness_with(settings()).await
}

pub async fn harness_with(settings: QueueSettings) -> H {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(real_now());
    open_in(dir, settings, clock).await
}

/// Opens (or reopens) everything over the database in `dir`.
pub async fn open_in(dir: tempfile::TempDir, settings: QueueSettings, clock: ManualClock) -> H {
    let db = open_db(dir.path()).await;
    let backend: Arc<dyn Backend> = Arc::new(db.clone());
    let policy = PolicyService::open(backend.clone()).await.unwrap();
    let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
    policy.attach_data(data.clone());
    let ent = EntitlementService::open(
        backend.clone(),
        policy.clone(),
        Arc::new(forgebase_entitlements::ManualClock::at(real_now())),
    )
    .await
    .unwrap();
    policy.set_hooks(ent.hooks());
    let rt = RealtimeService::start(data.clone(), None, RealtimeConfig::default(), None)
        .await
        .unwrap();
    let keyring = Keyring::from_master(b"queue-tests-master-key-0123456789");
    let svc = QueueService::with_clock(data.clone(), keyring.clone(), settings, Arc::new(clock.clone()));
    svc.attach_entitlements(ent.clone());
    H {
        db,
        data,
        policy,
        ent,
        rt,
        svc,
        clock,
        keyring,
        dir,
    }
}

impl H {
    /// Closes the database and opens a fresh stack over the same files, as
    /// after a process restart. The clock carries on.
    pub async fn restart(self) -> H {
        let H {
            db, rt, svc, clock, dir, ..
        } = self;
        let settings = svc.settings().clone();
        svc.shutdown(Duration::from_secs(1)).await;
        rt.shutdown();
        drop(svc);
        db.close().await;
        open_in(dir, settings, clock).await
    }

    pub async fn topic(&self, name: &str) {
        self.svc
            .create_topic(
                &admin(),
                NewTopic {
                    name: name.into(),
                    ..NewTopic::default()
                },
            )
            .await
            .unwrap();
    }

    pub async fn sub(&self, name: &str, topic: &str) -> Subscription {
        self.sub_with(NewSubscription {
            name: name.into(),
            topic: topic.into(),
            ..NewSubscription::default()
        })
        .await
    }

    pub async fn sub_with(&self, new: NewSubscription) -> Subscription {
        self.svc.create_subscription(&admin(), new).await.unwrap()
    }

    pub async fn publish(&self, topic: &str, data: Json) -> String {
        self.svc
            .publish(&admin(), topic, vec![NewMessage::new(data)])
            .await
            .unwrap()
            .remove(0)
            .message_id
    }

    pub async fn pull(&self, sub: &str, max: i64) -> Vec<ReceivedMessage> {
        self.svc
            .pull(
                &admin(),
                sub,
                PullRequest {
                    max_messages: Some(max),
                    ..PullRequest::default()
                },
            )
            .await
            .unwrap()
    }

    pub async fn ack(&self, sub: &str, msgs: &[ReceivedMessage]) {
        let ids: Vec<String> = msgs.iter().map(|m| m.ack_id.clone()).collect();
        let r = self.svc.ack(&admin(), sub, &ids).await.unwrap();
        assert_eq!(r.count, i64::try_from(ids.len()).unwrap(), "unknown: {:?}", r.unknown);
    }

    /// Grants `permission` (`engine:queue_publish`, ...) to `principal`.
    pub async fn grant(&self, principal: &str, permission: &str) {
        self.policy
            .add_override(&admin(), None, principal, permission, Effect::Allow)
            .await
            .unwrap();
    }

    /// A metered feature `feature` limited to `limit` units for `subject`.
    pub async fn quota(&self, subject: &str, limits: &[(&str, i64)]) {
        let mut features = Vec::new();
        for (feature, limit) in limits {
            self.ent
                .put_feature(
                    &admin(),
                    feature,
                    FeatureInput {
                        name: None,
                        description: None,
                        kind: FeatureKind::Metered,
                        unit: None,
                        permissions: vec![],
                    },
                )
                .await
                .unwrap();
            features.push(PlanFeature {
                feature: (*feature).into(),
                enabled: true,
                limit: Some(*limit),
                unit: String::new(),
                period: None,
                window_secs: None,
                trial_limit: None,
            });
        }
        self.ent
            .put_plan(
                &admin(),
                "queues",
                PlanInput {
                    name: None,
                    description: None,
                    is_default: false,
                    active: true,
                    trial_days: 0,
                    metadata: None,
                    features,
                },
            )
            .await
            .unwrap();
        self.ent
            .apply_subscription(
                &admin(),
                SubscriptionSync {
                    subject: subject.into(),
                    plan: "queues".into(),
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
}

/// Polls `check` until it returns `Some` or `timeout` passes.
pub async fn wait_for<T, F, Fut>(timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out after {timeout:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn ids(msgs: &[ReceivedMessage]) -> Vec<String> {
    msgs.iter().map(|m| m.message.id.clone()).collect()
}

pub fn datas(msgs: &[ReceivedMessage]) -> Vec<Json> {
    msgs.iter().map(|m| m.message.data.clone()).collect()
}
