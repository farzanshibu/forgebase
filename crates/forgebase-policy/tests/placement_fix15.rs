//! FIX-15: creating an environment places it (ADR-0029 §3.4). Projects get
//! three default environments that stay together (project affinity); the
//! next project goes to the least-loaded set; `POST environments` follows
//! its project. With one set nothing is written. Real, separate PostgreSQL
//! clusters; skipped without an engine.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::AccessContext;
use forgebase_db::shard_testing::Sharded;
use forgebase_db::shards::{self, ShardSetup};
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, DbRouter};
use forgebase_policy::PolicyService;
use serde_json::Value as Json;

fn env_ids(project: &Json) -> Vec<String> {
    project["environments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect()
}

fn set_of(router: &DbRouter, env: &str) -> String {
    let p = router
        .placement(env)
        .unwrap_or_else(|| panic!("environment {env} has no placement row"));
    assert_eq!(p.state, PlacementState::Active);
    p.set_id
}

#[tokio::test]
async fn new_environments_are_placed_per_the_policy() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let backend: Arc<dyn Backend> = Arc::new(s.router.control());
    let policy = PolicyService::open(backend).await.unwrap();
    policy.attach_router(s.router.clone());
    let ctx = AccessContext::admin("root");
    let org = policy.create_org(&ctx, "acme", "Acme").await.unwrap();
    let org = org["id"].as_str().unwrap().to_owned();

    // One set: behaves as before, no placement rows.
    assert!(!s.router.is_sharded());
    let solo = policy.create_project(&ctx, Some(&org), "solo", "Solo").await.unwrap();
    assert_eq!(env_ids(&solo).len(), 3);
    for e in env_ids(&solo) {
        assert!(s.router.placement(&e).is_none(), "one set writes no placement row");
    }

    // Two shards.
    let mut shard_ids = Vec::new();
    for name in ["s1", "s2"] {
        let (set, shard) = s.provision(name).await;
        shards::activate_shard(
            &s.router,
            &set,
            &shard,
            ShardSetup {
                backups_dir: None,
                replay: None,
                control_conninfo: None,
            },
        )
        .await
        .unwrap();
        shard_ids.push(set.id);
    }
    assert!(s.router.is_sharded());

    // Least loaded, ties to the lower number: control (no tenants counted
    // yet), then s1, then s2; the three environments of a project stay on
    // one set.
    let mut sets = Vec::new();
    let mut projects = Vec::new();
    for slug in ["alpha", "beta", "gamma"] {
        let p = policy.create_project(&ctx, Some(&org), slug, slug).await.unwrap();
        let envs = env_ids(&p);
        let on: Vec<String> = envs.iter().map(|e| set_of(&s.router, e)).collect();
        assert!(on.iter().all(|x| *x == on[0]), "{slug}: {on:?}");
        sets.push(on[0].clone());
        projects.push(p);
    }
    assert_eq!(sets, vec!["control".to_owned(), shard_ids[0].clone(), shard_ids[1].clone()]);

    // `POST environments` follows its project, also on a shard.
    for (p, expect) in projects.iter().zip(&sets) {
        let e = policy
            .create_environment(&ctx, p["id"].as_str().unwrap(), "preview")
            .await
            .unwrap();
        assert_eq!(&set_of(&s.router, e["id"].as_str().unwrap()), expect);
    }

    // The placements are durable: a router reading control sees them.
    s.router.reload().await.unwrap();
    for (p, expect) in projects.iter().zip(&sets) {
        for e in env_ids(p) {
            assert_eq!(&set_of(&s.router, &e), expect);
        }
    }
}
