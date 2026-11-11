//! Test harness: migrated SQLite, policy, jobs, the customize service with a
//! static DNS resolver, a capturing mail provider and a manual clock.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use forgebase_auth::ManualClock;
use forgebase_core::DataLayout;
use forgebase_customize::{
    CustomizeConfig, CustomizeService, LogProvider, MailProvider, OutMail, SendError, StaticResolver,
};
use forgebase_data::{AccessContext, DataService};
use forgebase_db::backend::BoxFuture;
use forgebase_db::{Backend, Database, Statement};
use forgebase_jobs::JobService;
use forgebase_policy::PolicyService;
use serde_json::json;

pub struct Env {
    pub db: Database,
    pub policy: PolicyService,
    pub data: DataService,
    pub svc: CustomizeService,
    pub jobs: JobService,
    pub clock: ManualClock,
    pub dns: StaticResolver,
    pub mail: LogProvider,
    pub tmp: forgebase_db::testing::TestDb,
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

pub fn in_realm(mut c: AccessContext, realm: &str) -> AccessContext {
    c.realm_id = realm.to_owned();
    c
}

impl Env {
    pub async fn new() -> Self {
        Self::with_config(CustomizeConfig {
            email_backoff_base_ms: 20,
            ..CustomizeConfig::default()
        })
        .await
    }

    pub async fn with_config(cfg: CustomizeConfig) -> Self {
        let tmp = forgebase_db::testing::test_backend().await;
        let layout = DataLayout::new(tmp.data_dir().join("data"));
        layout.ensure().unwrap();
        let db = tmp.database().clone();
        let backend: Arc<dyn Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let clock = ManualClock::from_system();
        let svc = CustomizeService::open(backend, policy.clone(), Arc::new(clock.clone()), cfg)
            .await
            .unwrap();
        let jobs = JobService::new(data.clone());
        svc.attach_jobs(jobs.clone());
        let dns = StaticResolver::new();
        svc.set_dns(Arc::new(dns.clone()));
        let mail = LogProvider::capturing();
        svc.set_mail_provider(Arc::new(mail.clone()));
        let env = Self {
            db,
            policy,
            data,
            svc,
            jobs,
            clock,
            dns,
            mail,
            tmp,
        };
        for realm in ["globex"] {
            env.policy.create_realm(&admin(), realm, realm).await.unwrap();
        }
        env
    }

    /// A realm operator: holds `customization:*` at realm level.
    pub async fn operator(&self, who: &str, realm: &str) -> AccessContext {
        let mut a = admin();
        a.realm_id = realm.to_owned();
        self.policy
            .put_role(
                &a,
                None,
                &serde_json::from_value(json!({"key": "brand_admin", "allow": ["customization:*"]})).unwrap(),
            )
            .await
            .unwrap();
        self.policy
            .put_realm_binding(&a, &pk(who), &["brand_admin".to_owned()], &[])
            .await
            .unwrap();
        in_realm(user(who), realm)
    }

    pub async fn count(&self, sql: &str) -> i64 {
        self.db
            .fetch_all(&Statement::raw(sql))
            .await
            .unwrap()
            .scalar()
            .and_then(|v| v.as_i64())
            .unwrap()
    }

    pub fn advance(&self, d: Duration) {
        self.clock.advance(d);
    }
}

/// Fails the first `fail` sends (transiently, or permanently), then succeeds.
#[derive(Debug)]
pub struct Flaky {
    pub fail: usize,
    pub permanent: bool,
    pub calls: AtomicUsize,
}

impl Flaky {
    pub fn new(fail: usize, permanent: bool) -> Arc<Self> {
        Arc::new(Self {
            fail,
            permanent,
            calls: AtomicUsize::new(0),
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl MailProvider for Flaky {
    fn name(&self) -> &'static str {
        "flaky"
    }

    fn send<'a>(&'a self, _m: &'a OutMail) -> BoxFuture<'a, Result<(), SendError>> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail {
                let m = format!("simulated failure {n}");
                Err(if self.permanent {
                    SendError::Permanent(m)
                } else {
                    SendError::Transient(m)
                })
            } else {
                Ok(())
            }
        })
    }
}
