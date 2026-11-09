//! Live auth tests against the real server binary (see `tests/common`).

mod common;

use common::{Server, now_secs, totp};
use forgebase_sdk::{FileStore, ForgeBase, SignInOptions, SignInResult};

const USER: (&str, &str) = ("reader@example.com", "another long passphrase 7");

fn opts() -> SignInOptions {
    SignInOptions::default()
}

#[tokio::test(flavor = "multi_thread")]
async fn password_flow_refresh_and_sign_out() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");

    let user = fb.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");
    assert_eq!(user["email"], USER.0);

    let first = fb
        .auth()
        .sign_in(USER.0, USER.1, &opts())
        .await
        .expect("sign in")
        .into_session()
        .expect("session");
    assert!(!first.access_token.is_empty());
    assert!(first.refresh_token.is_some());
    let me = fb.auth().me().await.expect("me");
    assert!(me.get("subject").is_some(), "{me}");

    let watch = fb.auth().watch();
    let next = fb.auth().refresh().await.expect("refresh");
    assert_ne!(next.access_token, first.access_token);
    assert_ne!(next.refresh_token, first.refresh_token, "refresh tokens rotate");
    assert!(watch.has_changed().expect("watch"), "refresh notifies watchers");

    fb.auth().sign_out().await.expect("sign out");
    assert!(fb.auth().session().is_none());
    let err = fb.auth().me().await.expect_err("signed out");
    assert!(err.is_unauthenticated(), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_credentials_are_typed_errors() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    let err = fb
        .auth()
        .sign_in("nobody@example.com", "nope nope nope", &opts())
        .await
        .expect_err("bad login");
    assert!(err.is_unauthenticated(), "{err}");
    assert_eq!(err.status(), Some(401));
    assert!(err.request_id().is_some(), "errors carry the request id: {err}");
    let weak = fb
        .auth()
        .sign_up("weak@example.com", "x", None, None)
        .await
        .expect_err("weak password");
    assert!(weak.is_validation(), "{weak}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_access_token_is_refreshed_transparently() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    fb.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");
    fb.auth().sign_in(USER.0, USER.1, &opts()).await.expect("sign in");
    let mut broken = fb.auth().session().expect("session");
    let good = broken.access_token.clone();
    broken.access_token = "garbage.token.value".into();
    broken.expires_at_ms = u64::MAX; // not expired locally: the server's 401 triggers the refresh
    fb.auth().set_session(Some(broken));
    let me = fb.auth().me().await.expect("refreshes after the 401 and retries");
    assert!(me.get("subject").is_some());
    let now = fb.auth().session().expect("session");
    assert_ne!(now.access_token, "garbage.token.value");
    assert_ne!(now.access_token, good);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_requests_share_one_refresh() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    fb.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");
    fb.auth().sign_in(USER.0, USER.1, &opts()).await.expect("sign in");
    let mut sess = fb.auth().session().expect("session");
    sess.access_token = "garbage.token.value".into();
    sess.expires_at_ms = u64::MAX;
    fb.auth().set_session(Some(sess));
    // Refresh tokens are single use: without single-flight at most one of these could succeed.
    let auth = fb.auth();
    let calls: Vec<_> = (0..6).map(|_| auth.me()).collect();
    for r in futures_util::future::join_all(calls).await {
        r.expect("every concurrent call succeeds");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn magic_link_and_otp() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    fb.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");

    let seen = s.mails().len();
    fb.auth().request_magic_link(USER.0, None).await.expect("request link");
    let mail = s.mail(seen).await;
    let token = mail
        .split("token=")
        .nth(1)
        .and_then(|t| t.split(['"', '&', '\\', ' ', '\n']).next())
        .expect("token in mail");
    let token = url_decode(token);
    let sess = fb
        .auth()
        .confirm_magic_link(&token, &opts())
        .await
        .expect("confirm")
        .into_session()
        .expect("session");
    assert!(!sess.access_token.is_empty());
    let replay = fb
        .auth()
        .confirm_magic_link(&token, &opts())
        .await
        .expect_err("single use");
    assert!(replay.is_unauthenticated(), "{replay}");

    let seen = s.mails().len();
    fb.auth().request_otp(USER.0, None).await.expect("request otp");
    let mail = s.mail(seen).await;
    let code: String = mail
        .split(|c: char| !c.is_ascii_digit())
        .find(|d| (6..=8).contains(&d.len()))
        .expect("code in mail")
        .to_owned();
    let sess = fb
        .auth()
        .verify_otp(USER.0, &code, &opts())
        .await
        .expect("verify")
        .into_session()
        .expect("session");
    assert!(!sess.access_token.is_empty());
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn totp_mfa_challenge_and_recovery_codes() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    fb.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");
    fb.auth().sign_in(USER.0, USER.1, &opts()).await.expect("sign in");

    let setup = fb.auth().mfa_totp_setup().await.expect("setup");
    assert!(setup.otpauth_url.starts_with("otpauth://"));
    let enabled = fb
        .auth()
        .mfa_totp_confirm(&totp(&setup.secret, now_secs()))
        .await
        .expect("confirm");
    assert!(enabled.enabled);
    assert!(!enabled.recovery_codes.is_empty());
    fb.auth().sign_out().await.expect("sign out");

    // Password alone is no longer enough.
    let challenge = match fb.auth().sign_in(USER.0, USER.1, &opts()).await.expect("sign in") {
        SignInResult::MfaRequired(c) => c,
        other => panic!("expected an MFA challenge, got {other:?}"),
    };
    assert!(fb.auth().session().is_none(), "no session before the second factor");
    let wrong = fb
        .auth()
        .verify_mfa(&challenge.mfa_token, "000000")
        .await
        .expect_err("wrong code");
    assert!(wrong.is_unauthenticated() || wrong.is_validation(), "{wrong}");

    // A recovery code works once, as a second factor.
    let recovery = enabled.recovery_codes[0].clone();
    let sess = fb
        .auth()
        .verify_mfa(&challenge.mfa_token, &recovery)
        .await
        .expect("recovery code");
    assert!(!sess.access_token.is_empty());
    let me = fb.auth().me().await.expect("me");
    assert!(me.get("subject").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn personal_access_tokens_work_as_api_keys_until_revoked() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let created = admin
        .auth()
        .create_token("sdk-live", &["collections:read"], None)
        .await
        .expect("create token");
    assert!(created.key.starts_with("fb"), "{}", created.key);

    let keyed = ForgeBase::builder(&s.base)
        .api_key(created.key.clone())
        .build()
        .expect("client");
    assert!(keyed.collections().list().await.is_ok());
    // No write scope: a typed 403.
    let def = serde_json::json!({"name": "nope", "type": "base", "fields": []});
    let err = keyed.collections().create(&def).await.expect_err("scope");
    assert!(err.is_forbidden(), "{err}");

    let listed = admin.auth().list_tokens().await.expect("list tokens");
    assert!(listed.iter().any(|t| t["id"] == created.id), "{listed:?}");
    admin.auth().revoke_token(&created.id).await.expect("revoke");
    let err = keyed.collections().list().await.expect_err("revoked");
    assert!(err.is_unauthenticated(), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn service_account_keys() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let acct = admin
        .auth()
        .create_service_account("worker", Some("ci"), None)
        .await
        .expect("account");
    let id = acct["id"].as_str().expect("id").to_owned();
    let key = admin
        .auth()
        .create_service_account_key(&id, "k1", &["auth:read", "collections:read"], None)
        .await
        .expect("key");
    let svc = ForgeBase::builder(&s.base).api_key(key.key).build().expect("client");
    let me = svc.auth().me().await.expect("me");
    assert!(me.to_string().contains("service"), "{me}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_persist_in_a_file_store() {
    let Some(s) = Server::start() else { return };
    let path = s.dir.path().join("session.json");
    let a = ForgeBase::builder(&s.base)
        .session_store(FileStore::new(&path))
        .build()
        .expect("client");
    a.auth().sign_up(USER.0, USER.1, None, None).await.expect("sign up");
    a.auth().sign_in(USER.0, USER.1, &opts()).await.expect("sign in");
    assert!(path.exists());
    // A new process (client) restores the session without signing in again.
    let b = ForgeBase::builder(&s.base)
        .session_store(FileStore::new(&path))
        .build()
        .expect("client");
    assert!(b.auth().me().await.is_ok());
    b.auth().sign_out().await.expect("sign out");
    assert!(!path.exists(), "sign-out clears the store");
}

#[tokio::test(flavor = "multi_thread")]
async fn oauth_providers_and_start_without_config() {
    let Some(s) = Server::start() else { return };
    let fb = ForgeBase::new(&s.base).expect("client");
    assert!(fb.auth().oauth_providers().await.expect("providers").is_empty());
    let err = fb
        .auth()
        .oauth_start("github", &opts())
        .await
        .expect_err("unconfigured provider");
    assert!(err.is_not_found(), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn health() {
    let Some(s) = Server::start() else { return };
    assert!(ForgeBase::new(&s.base).expect("client").health().await);
    let down = ForgeBase::builder("http://127.0.0.1:1")
        .retry(forgebase_sdk::RetryPolicy::none())
        .build()
        .expect("client");
    assert!(!down.health().await);
}
