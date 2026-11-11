//! Test harness: a migrated SQLite database, the policy service with the
//! entitlement hooks installed, a data service using the policy authorizer,
//! the job queue, and a manual clock.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService};
use forgebase_db::{Backend, Database, Statement};
use forgebase_entitlements::{
    AlertSink, EntitlementService, FeatureInput, FeatureKind, ManualClock, PlanFeature, PlanInput, QuotaPolicy,
    SubscriptionStatus, SubscriptionSync, UsageAlert, UsageRequest,
};
use forgebase_jobs::JobService;
use forgebase_policy::PolicyService;
use serde_json::json;

/// 2026-11-09 12:00 UTC.
pub const T0: i64 = 1_794_225_600_000;
pub const MIN: i64 = 60_000;
pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 86_400_000;

#[derive(Debug, Default)]
pub struct Recorder(pub Mutex<Vec<UsageAlert>>);

impl AlertSink for Recorder {
    fn publish(&self, alert: &UsageAlert) {
        self.0.lock().unwrap().push(alert.clone());
    }
}

pub struct Env {
    pub db: Database,
    pub policy: PolicyService,
    pub data: DataService,
    pub ent: EntitlementService,
    pub jobs: JobService,
    pub clock: ManualClock,
    pub sink: Arc<Recorder>,
    pub tmp: forgebase_db::testing::TestDb,
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn sys() -> AccessContext {
    AccessContext::system()
}

pub fn user(id: &str) -> AccessContext {
    AccessContext::user("people", id)
}

pub fn pk(id: &str) -> String {
    format!("user:people/{id}")
}

pub fn org_subject(org: &str) -> String {
    format!("org:{org}")
}

pub fn req(feature: &str, units: i64, key: &str) -> UsageRequest {
    UsageRequest::new(feature, units, key)
}

pub fn pf(feature: &str, limit: Option<i64>, period: Option<&str>) -> PlanFeature {
    PlanFeature {
        feature: feature.into(),
        enabled: true,
        limit,
        unit: String::new(),
        period: period.map(|p| serde_json::from_value(json!(p)).unwrap()),
        window_secs: None,
        trial_limit: None,
    }
}

impl Env {
    pub async fn new() -> Self {
        Self::at(T0).await
    }

    pub async fn at(now: i64) -> Self {
        let tmp = forgebase_db::testing::test_backend().await;
        let layout = DataLayout::new(tmp.data_dir().join("data"));
        layout.ensure().unwrap();
        let db = tmp.database().clone();
        let backend: Arc<dyn Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let clock = ManualClock::at(now);
        let ent = EntitlementService::open(backend, policy.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        policy.set_hooks(ent.hooks());
        let jobs = JobService::new(data.clone());
        ent.attach_jobs(jobs.clone());
        let sink = Arc::new(Recorder::default());
        ent.set_alert_sink(sink.clone());
        let env = Self {
            db,
            policy,
            data,
            ent,
            jobs,
            clock,
            sink,
            tmp,
        };
        env.data
            .create_collection(
                &sys(),
                serde_json::from_value(json!({"name": "people", "fields": [{"name": "email", "type": "text"}]}))
                    .unwrap(),
            )
            .await
            .unwrap();
        env
    }

    /// Creates an org owned by `owner`; returns its id.
    pub async fn org(&self, owner: &str, slug: &str) -> String {
        let o = self.policy.create_org(&user(owner), slug, slug).await.unwrap();
        o["id"].as_str().unwrap().to_owned()
    }

    pub async fn member(&self, owner: &str, org: &str, who: &str, roles: &[&str]) {
        let roles: Vec<String> = roles.iter().map(|r| (*r).to_owned()).collect();
        self.policy
            .set_member(&user(owner), org, &pk(who), &roles, &[])
            .await
            .unwrap();
    }

    pub async fn feature(&self, key: &str, kind: FeatureKind, permissions: &[&str]) {
        self.ent
            .put_feature(
                &admin(),
                key,
                FeatureInput {
                    name: None,
                    description: None,
                    kind,
                    unit: None,
                    permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
                },
            )
            .await
            .unwrap();
    }

    pub async fn plan(&self, key: &str, features: Vec<PlanFeature>) {
        self.plan_with(key, false, 0, features).await;
    }

    pub async fn plan_with(&self, key: &str, is_default: bool, trial_days: i64, features: Vec<PlanFeature>) {
        self.ent
            .put_plan(
                &admin(),
                key,
                PlanInput {
                    name: None,
                    description: None,
                    is_default,
                    active: true,
                    trial_days,
                    metadata: None,
                    features,
                },
            )
            .await
            .unwrap();
    }

    pub async fn policy_for(&self, feature: &str, p: QuotaPolicy) {
        self.ent.put_quota_policy(&admin(), feature, p).await.unwrap();
    }

    pub async fn subscribe(&self, subject: &str, plan: &str) {
        self.subscribe_with(subject, plan, |_| {}).await;
    }

    pub async fn subscribe_with(&self, subject: &str, plan: &str, f: impl FnOnce(&mut SubscriptionSync)) {
        let mut s = SubscriptionSync {
            subject: subject.into(),
            plan: plan.into(),
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
        };
        f(&mut s);
        self.ent.apply_subscription(&admin(), s).await.unwrap();
    }

    pub async fn count(&self, sql: &str) -> i64 {
        self.db
            .fetch_all(&Statement::new(sql, Vec::new()))
            .await
            .unwrap()
            .scalar()
            .and_then(forgebase_db::Value::as_i64)
            .unwrap()
    }
}
