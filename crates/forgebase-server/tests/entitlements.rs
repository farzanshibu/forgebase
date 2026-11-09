//! Entitlements through the real server lifecycle: the migration applies,
//! the hooks are installed in the policy pipeline, the API is mounted and
//! documented, and the maintenance schedule exists. Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;

use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_entitlements::{FeatureInput, FeatureKind, PlanFeature, PlanInput, SubscriptionStatus, SubscriptionSync};
use forgebase_policy::Check;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split(' ').nth(1).unwrap().parse().unwrap();
    let body = out
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn the_server_mounts_entitlements_and_installs_the_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults(dir.path());
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    let server = forgebase_server::start(&cfg).await.unwrap();
    let addr = server.local_addr();

    let (s, body) = get(addr, "/api/openapi.json").await;
    assert_eq!(s, 200);
    for p in [
        "/api/v1/usage/consume",
        "/api/v1/plans/{key}",
        "/api/v1/entitlements/has",
    ] {
        assert!(body.contains(p), "{p} not documented");
    }
    let (s, _) = get(addr, "/api/v1/entitlements").await;
    assert_eq!(s, 401, "anonymous");

    let schedules = server.jobs().schedules(&AccessContext::system()).await.unwrap();
    assert!(
        schedules
            .iter()
            .any(|s| s.name == forgebase_entitlements::MAINTENANCE_SCHEDULE)
    );

    // The policy pipeline consults the entitlement hook (step 4).
    let ent = server.entitlements();
    let admin = AccessContext::admin("root");
    ent.put_feature(
        &admin,
        "ai",
        FeatureInput {
            name: None,
            description: None,
            kind: FeatureKind::Boolean,
            unit: None,
            permissions: vec!["ai:*".into()],
        },
    )
    .await
    .unwrap();
    ent.put_plan(
        &admin,
        "free",
        PlanInput {
            name: None,
            description: None,
            is_default: true,
            active: true,
            trial_days: 0,
            metadata: None,
            features: vec![PlanFeature {
                feature: "ai".into(),
                enabled: false,
                limit: None,
                unit: String::new(),
                period: None,
                window_secs: None,
                trial_limit: None,
            }],
        },
    )
    .await
    .unwrap();
    let alice = AccessContext::user("users", "alice");
    let personal = Check {
        permission: "ai:generate".into(),
        owner: Some(alice.principal_key()),
        ..Check::default()
    };
    let policy = ent.policy().clone();
    assert!(policy.check(&alice, &personal).is_err());
    ent.put_plan(
        &admin,
        "pro",
        PlanInput {
            name: None,
            description: None,
            is_default: false,
            active: true,
            trial_days: 0,
            metadata: None,
            features: vec![PlanFeature {
                feature: "ai".into(),
                enabled: true,
                limit: None,
                unit: String::new(),
                period: None,
                window_secs: None,
                trial_limit: None,
            }],
        },
    )
    .await
    .unwrap();
    ent.apply_subscription(
        &admin,
        SubscriptionSync {
            subject: alice.principal_key(),
            plan: "pro".into(),
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
    policy.check(&alice, &personal).unwrap();
    server.shutdown().await.unwrap();
}
