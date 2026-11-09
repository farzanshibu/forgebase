//! The production authenticator against the real auth service (FB-20): API
//! keys and personal access tokens, scopes, revocation of a live connection.
//! These tests bind `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, Authenticated, ClientInfo, CredentialKind, Subject};
use forgebase_data::DefaultAuthorizer;
use forgebase_db::{Database, DbOptions};
use forgebase_kv::{KvConfig, KvService};
use forgebase_resp::{KeyAuthenticator, LocalKv, RespConfig, RespHandle, start};

struct Stack {
    handle: RespHandle,
    auth: AuthService,
    admin: Authenticated,
    _dir: tempfile::TempDir,
}

async fn stack(tweak: impl FnOnce(&mut RespConfig)) -> Stack {
    let dir = tempfile::tempdir().unwrap();
    let cfg = forgebase_core::Config::defaults(dir.path());
    let db = Database::open(DbOptions::from_config(&cfg.database, dir.path().join("forgebase.db")))
        .await
        .unwrap();
    db.migrate(None).await.unwrap();
    let mut acfg = AuthConfig::for_data_dir(dir.path());
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let authorizer = Arc::new(DefaultAuthorizer);
    let auth = AuthService::open_with_data(Arc::new(db.clone()), authorizer.clone(), dir.path(), acfg, None)
        .await
        .unwrap();
    let ci = ClientInfo::default();
    let admin_view = auth
        .create_admin("root@example.com", "a-long-admin-password-1", "test", &ci)
        .await
        .unwrap();
    let admin = Authenticated {
        subject: Subject::Admin { id: admin_view.id },
        credential: CredentialKind::AccessToken,
        credential_id: "t".into(),
        scopes: None,
        mfa: false,
        impersonator: None,
    };
    let kv = KvService::open(
        KvConfig {
            flush_interval: Duration::ZERO,
            ..KvConfig::default()
        },
        authorizer,
        None,
    )
    .await
    .unwrap();
    let mut rc = test_cfg();
    tweak(&mut rc);
    let handle = start(rc, Arc::new(LocalKv(kv)), Arc::new(KeyAuthenticator::new(auth.clone())))
        .await
        .expect("bind 127.0.0.1:0");
    Stack {
        handle,
        auth,
        admin,
        _dir: dir,
    }
}

impl Stack {
    async fn api_key(&self, scopes: &[&str]) -> (String, String) {
        let ci = ClientInfo::default();
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let name = format!("worker-{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let sa = self
            .auth
            .create_service_account(&self.admin, &ci, &name, "", "admin")
            .await
            .unwrap();
        let k = self
            .auth
            .create_api_key(
                &self.admin,
                &ci,
                &sa.id,
                "resp",
                &scopes.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                None,
            )
            .await
            .unwrap();
        (k.key, k.info.id)
    }

    async fn raw(&self) -> Raw {
        Raw::connect(self.handle.local_addr()).await
    }
}

#[tokio::test]
async fn api_keys_and_pats_authenticate_and_everything_else_does_not() {
    let s = stack(|_| {}).await;
    let (key, _) = s.api_key(&["*"]).await;
    assert!(key.starts_with("fbk_"));
    let mut r = s.raw().await;
    assert_eq!(r.cmd(&["AUTH", &key]).await, Val::ok());
    assert_eq!(r.cmd(&["SET", "k", "v"]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::bulk("v"));
    // a personal access token of the administrator works too
    let pat = s
        .auth
        .create_pat(&s.admin, &ClientInfo::default(), "laptop", &["*".to_owned()], None)
        .await
        .unwrap()
        .key;
    assert!(pat.starts_with("fbp_"));
    let mut p = s.raw().await;
    assert_eq!(p.cmd(&["AUTH", &pat]).await, Val::ok());
    assert_eq!(p.cmd(&["GET", "k"]).await, Val::bulk("v"));
    // ACL style: the user is the owner's id (or "default")
    let mut a = s.raw().await;
    let owner = s.admin_id();
    assert_eq!(a.cmd(&["AUTH", &owner, &pat]).await, Val::ok());
    let mut d = s.raw().await;
    assert_eq!(d.cmd(&["AUTH", "default", &key]).await, Val::ok());
    let mut w = s.raw().await;
    assert!(
        w.cmd(&["AUTH", "not-the-owner", &pat])
            .await
            .is_err_starting("WRONGPASS")
    );
    // wrong secret, truncated key, password, session-token lookalike
    let mut bad = s.raw().await;
    let truncated = &key[..key.len() - 3];
    for secret in [
        truncated,
        "fbk_deadbeef_x",
        "a-long-admin-password-1",
        "eyJhbGciOiJIUzI1NiJ9.e30.x",
    ] {
        let v = bad.cmd(&["AUTH", secret]).await;
        assert!(v.is_err_starting("WRONGPASS"), "{secret:?} -> {v:?}");
    }
    assert!(bad.cmd(&["GET", "k"]).await.is_err_starting("NOAUTH"));
    assert!(s.raw().await.cmd(&["AUTH", ""]).await.is_err_starting("WRONGPASS"));
}

impl Stack {
    fn admin_id(&self) -> String {
        match &self.admin.subject {
            Subject::Admin { id } => id.clone(),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn key_scopes_narrow_access_to_the_kv_area() {
    let s = stack(|_| {}).await;
    let (full, _) = s.api_key(&["*"]).await;
    let (ro, _) = s.api_key(&["kv:read"]).await;
    let (other_area, _) = s.api_key(&["collections:read"]).await;
    let mut f = s.raw().await;
    f.cmd(&["AUTH", &full]).await;
    assert_eq!(f.cmd(&["SET", "k", "v"]).await, Val::ok());
    let mut r = s.raw().await;
    assert_eq!(r.cmd(&["AUTH", &ro]).await, Val::ok());
    assert_eq!(r.cmd(&["GET", "k"]).await, Val::bulk("v"));
    assert!(r.cmd(&["SET", "k", "x"]).await.is_err_starting("NOPERM"));
    let mut o = s.raw().await;
    assert_eq!(o.cmd(&["AUTH", &other_area]).await, Val::ok());
    assert!(
        o.cmd(&["GET", "k"]).await.is_err_starting("NOPERM"),
        "no kv scope at all"
    );
    assert_eq!(f.cmd(&["GET", "k"]).await, Val::bulk("v"));
}

#[tokio::test]
async fn a_revoked_key_stops_working_on_an_open_connection() {
    let s = stack(|c| c.revalidate_interval = Duration::from_millis(100)).await;
    let (key, id) = s.api_key(&["*"]).await;
    let mut busy = s.raw().await;
    busy.cmd(&["AUTH", &key]).await;
    assert_eq!(busy.cmd(&["SET", "k", "v"]).await, Val::ok());
    let mut sub = s.raw().await;
    sub.cmd(&["AUTH", &key]).await;
    sub.cmd(&["SUBSCRIBE", "c"]).await;
    s.auth.revoke_key(&s.admin, &ClientInfo::default(), &id).await.unwrap();
    // the next command after the re-check interval is refused and the connection closed
    tokio::time::sleep(Duration::from_millis(250)).await;
    let v = busy.cmd(&["GET", "k"]).await;
    assert!(v.is_err_starting("NOAUTH"), "{v:?}");
    assert!(busy.closed_within(T).await);
    // an idle subscriber is closed by the timer without sending anything
    assert!(sub.closed_within(T).await);
    // and the key cannot start a new session
    let mut again = s.raw().await;
    assert!(again.cmd(&["AUTH", &key]).await.is_err_starting("WRONGPASS"));
}
