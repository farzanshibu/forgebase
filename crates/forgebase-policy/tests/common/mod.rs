//! Test harness: a migrated SQLite database in a temp dir, the policy
//! service and a data service that uses it as its authorizer.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService};
use forgebase_db::{Backend, Database, DbOptions, Statement};
use forgebase_policy::PolicyService;
use forgebase_schema::Collection;
use serde_json::{Value as Json, json};

pub struct Env {
    pub db: Database,
    pub policy: PolicyService,
    pub data: DataService,
    _tmp: tempfile::TempDir,
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

impl Env {
    pub async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let cfg = forgebase_core::Config::defaults(tmp.path());
        let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
            .await
            .unwrap();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        let backend: Arc<dyn forgebase_db::Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend, policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let env = Self {
            db,
            policy,
            data,
            _tmp: tmp,
        };
        // Principals' auth records (base collection standing in for users).
        env.collection(json!({"name": "people", "fields": [
            {"name": "email", "type": "text"}, {"name": "level", "type": "number"}
        ]}))
        .await;
        env
    }

    pub async fn collection(&self, def: Json) -> Collection {
        self.data
            .create_collection(&AccessContext::system(), serde_json::from_value(def).unwrap())
            .await
            .unwrap()
    }

    pub async fn person(&self, id: &str, email: &str) {
        self.data
            .create_record(&AccessContext::system(), "people", json!({"id": id, "email": email}))
            .await
            .unwrap();
    }

    /// Creates an org owned by `owner` and returns its id.
    pub async fn org(&self, owner: &str, slug: &str) -> String {
        let o = self.policy.create_org(&user(owner), slug, slug).await.unwrap();
        o["id"].as_str().unwrap().to_owned()
    }

    /// Creates a project in `org` (as `owner`) and returns (project id,
    /// production environment id).
    pub async fn project(&self, owner: &str, org: &str, slug: &str) -> (String, String) {
        let p = self
            .policy
            .create_project(&user(owner).with_org(org), Some(org), slug, slug)
            .await
            .unwrap();
        let env = p["environments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "production")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (p["id"].as_str().unwrap().to_owned(), env)
    }

    pub async fn member(&self, owner: &str, org: &str, who: &str, roles: &[&str]) {
        let roles: Vec<String> = roles.iter().map(|r| (*r).to_owned()).collect();
        self.policy
            .set_member(&user(owner), org, &pk(who), &roles, &[])
            .await
            .unwrap();
    }

    /// Denials in the audit table (waits for the background writer).
    pub async fn audit_steps(&self, principal: &str, min: usize) -> Vec<String> {
        let mut last = Vec::new();
        for _ in 0..60 {
            let rows = self
                .db
                .fetch_all(&Statement::new(
                    "SELECT step FROM _forgebase_authz_audit WHERE principal = ?1 ORDER BY id",
                    vec![principal.into()],
                ))
                .await
                .unwrap();
            last = rows.rows.iter().map(|r| r[0].as_str().unwrap().to_owned()).collect();
            if last.len() >= min {
                return last;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        last
    }
}
