//! Access-token verification (signature + live session check) and policy checks.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_auth::{ClientInfo, LoginOptions, LoginOutcome};
use forgebase_auth::{AuthConfig, AuthService};
use forgebase_bench::Rig;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_policy::{Check, PolicyService};

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let (auth, token) = rt.block_on(async {
        let cfg = AuthConfig::for_data_dir(rig.tmp.path().join("data"));
        let auth = AuthService::open_with_data(rig.backend(), Arc::new(DefaultAuthorizer), rig.tmp.path(), cfg, None)
            .await
            .unwrap();
        let ci = ClientInfo::default();
        auth.create_admin("root@example.com", "bench password 1", "bench", &ci)
            .await
            .unwrap();
        let out = auth
            .login_admin(&ci, "root@example.com", "bench password 1", &LoginOptions::default())
            .await
            .unwrap();
        let LoginOutcome::Session(s) = out else { panic!("expected a session") };
        (auth, s.access_token.clone().unwrap())
    });
    let policy = rt.block_on(async {
        let p = PolicyService::open(rig.backend()).await.unwrap();
        p.attach_data(auth.data().clone());
        p
    });

    let mut g = c.benchmark_group("auth_policy");
    g.throughput(Throughput::Elements(1));
    g.bench_function("access_token_verify", |b| {
        b.to_async(&rt).iter(|| {
            let (auth, token) = (auth.clone(), token.clone());
            async move {
                auth.authenticate_access_token(&token).await.unwrap();
            }
        });
    });
    let user = AccessContext::user("users", "u1").with_roles(vec!["member".into()]);
    let check = Check {
        permission: "projects:read".into(),
        org: None,
        resource: None,
        owner: None,
        allow_anonymous: false,
    };
    g.bench_function("policy_check_user", |b| {
        b.iter(|| {
            let _ = std::hint::black_box(policy.check(&user, &check));
        });
    });
    let admin = AccessContext::admin("a1");
    g.bench_function("policy_check_admin", |b| {
        b.iter(|| {
            let _ = std::hint::black_box(policy.check(&admin, &check));
        });
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
