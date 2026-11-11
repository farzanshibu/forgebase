//! Test harness: a migrated SQLite database, the policy service with the
//! entitlement hooks, the entitlement service (the usage feed), the credit
//! service, the job queue and a manual clock.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_core::DataLayout;
use forgebase_credits::{AuditReport, CreditInput, CreditService, SpendInput};
use forgebase_data::{AccessContext, DataService};
use forgebase_db::{Backend, Database, Statement};
use forgebase_entitlements::{
    EntitlementService, FeatureInput, FeatureKind, ManualClock, PlanFeature, PlanInput, UsageRequest,
};
use forgebase_jobs::JobService;
use forgebase_policy::PolicyService;
use serde_json::json;

/// 2026-11-09 12:00 UTC.
pub const T0: i64 = 1_794_225_600_000;
pub const MIN: i64 = 60_000;
pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 86_400_000;

pub struct Env {
    pub db: Database,
    pub policy: PolicyService,
    pub data: DataService,
    pub ent: EntitlementService,
    pub credits: CreditService,
    pub jobs: JobService,
    pub clock: ManualClock,
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

pub fn pf(feature: &str, limit: Option<i64>) -> PlanFeature {
    PlanFeature {
        feature: feature.into(),
        enabled: true,
        limit,
        unit: String::new(),
        period: Some(serde_json::from_value(json!("month")).unwrap()),
        window_secs: None,
        trial_limit: None,
    }
}

impl Env {
    pub async fn new() -> Self {
        let tmp = forgebase_db::testing::test_backend().await;
        let layout = DataLayout::new(tmp.data_dir().join("data"));
        layout.ensure().unwrap();
        let db = tmp.database().clone();
        let backend: Arc<dyn Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let clock = ManualClock::at(T0);
        let ent = EntitlementService::open(backend.clone(), policy.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        policy.set_hooks(ent.hooks());
        let jobs = JobService::new(data.clone());
        ent.attach_jobs(jobs.clone());
        let credits = CreditService::open(backend, policy.clone(), ent.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        credits.attach_jobs(jobs.clone());
        let env = Self {
            db,
            policy,
            data,
            ent,
            credits,
            jobs,
            clock,
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

    /// Realm-level roles (operators).
    pub async fn realm_roles(&self, who: &str, roles: &[&str]) {
        let roles: Vec<String> = roles.iter().map(|r| (*r).to_owned()).collect();
        self.policy
            .put_realm_binding(&admin(), &pk(who), &roles, &[])
            .await
            .unwrap();
    }

    /// Metered features on a default plan without limits.
    pub async fn metered(&self, features: &[&str]) {
        for f in features {
            self.ent
                .put_feature(
                    &admin(),
                    f,
                    FeatureInput {
                        name: None,
                        description: None,
                        kind: FeatureKind::Metered,
                        unit: None,
                        permissions: Vec::new(),
                    },
                )
                .await
                .unwrap();
        }
        self.ent
            .put_plan(
                &admin(),
                "free",
                PlanInput {
                    name: None,
                    description: None,
                    is_default: true,
                    active: true,
                    trial_days: 0,
                    metadata: None,
                    features: features.iter().map(|f| pf(f, None)).collect(),
                },
            )
            .await
            .unwrap();
    }

    /// Records FB-24 usage.
    pub async fn use_units(&self, subject: &str, feature: &str, units: i64, key: &str) {
        self.ent
            .consume(&sys(), UsageRequest::new(feature, units, key).for_subject(subject))
            .await
            .unwrap();
    }

    pub async fn top_up(&self, subject: &str, amount: i64, key: &str) -> String {
        self.credits
            .top_up(&sys(), CreditInput::new(subject, amount, key))
            .await
            .unwrap()
            .entry
            .id
    }

    pub async fn grant_until(&self, subject: &str, amount: i64, key: &str, expires_at: i64) -> String {
        self.credits
            .grant(&sys(), CreditInput::new(subject, amount, key).expiring(expires_at))
            .await
            .unwrap()
            .entry
            .id
    }

    pub async fn spend(&self, subject: &str, amount: i64, key: &str) -> String {
        self.credits
            .spend(&sys(), SpendInput::new(subject, amount, key))
            .await
            .unwrap()
            .entry
            .id
    }

    pub async fn balance(&self, subject: &str) -> i64 {
        self.credits.balance(&sys(), Some(subject)).await.unwrap().balance
    }

    pub async fn audit(&self, subject: &str) -> AuditReport {
        self.credits.audit(&sys(), subject).await.unwrap()
    }

    pub async fn assert_consistent(&self, subject: &str) {
        let a = self.audit(subject).await;
        assert!(a.consistent, "{a:?}");
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
