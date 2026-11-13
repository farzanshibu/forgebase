//! Mail provider selection (ADR-0039): sealed per-realm and instance
//! configuration, the test-send path, delivery through the chosen transport
//! with the provider's message id in the log, and reload after a restart.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::sync::Arc;

use common::mock_http::{self, MockHttp};
use common::{Env, admin, in_realm};
use forgebase_auth::crypto::Keyring;
use forgebase_auth::social::PlainHttpClient;
use forgebase_customize::{CustomizeConfig, CustomizeError, CustomizeService, DeliveryStatus, SendInput};
use forgebase_db::Backend as _;
use serde_json::{Value, json};

const MASK: &str = "********";

async fn mock(id_key: &'static str) -> MockHttp {
    mock_http::start(move |r| {
        if r.json()["subject"] == "FAIL" {
            return (422, json!({"message": "bad"}).to_string());
        }
        (200, json!({id_key: "provider-msg-1"}).to_string())
    })
    .await
}

fn keyring() -> Keyring {
    Keyring::from_master(b"a-test-master-key-of-32-bytes!!!")
}

async fn runtime(env: &Env) {
    env.svc
        .attach_mail_runtime(Arc::new(PlainHttpClient::default()), keyring())
        .await
        .unwrap();
}

fn resend(url: &str, key: &str) -> Value {
    json!({"provider": "resend", "api_key": key, "base_url": url, "from_address": "no-reply@acme.test"})
}

fn send_input(to: &str) -> SendInput {
    SendInput {
        template: "magic_link".into(),
        to: to.into(),
        variables: json!({"link": "https://acme.test/m?t=1"}),
    }
}

fn code(e: &CustomizeError) -> String {
    use forgebase_core::ErrorCode;
    e.code().to_owned()
}

#[tokio::test]
async fn only_platform_administrators_manage_providers() {
    let env = Env::new().await;
    runtime(&env).await;
    let op = env.operator("olga", "default").await;
    let m = mock("id").await;
    for r in [
        env.svc.mail_provider_config(&op, None).await.map(|_| ()),
        env.svc
            .put_mail_provider(&op, None, resend(&m.url, "k"))
            .await
            .map(|_| ()),
        env.svc.delete_mail_provider(&op, None).await,
        env.svc.send_test_mail(&op, None, "ann@example.com").await.map(|_| ()),
    ] {
        assert!(matches!(r, Err(CustomizeError::Forbidden(_))), "{r:?}");
    }
    assert!(m.requests().is_empty());
}

#[tokio::test]
async fn secrets_are_sealed_masked_and_kept() {
    let env = Env::new().await;
    runtime(&env).await;
    let m = mock("id").await;
    let v = env
        .svc
        .put_mail_provider(&admin(), Some("*"), resend(&m.url, "re_super_secret"))
        .await
        .unwrap();
    assert_eq!(v["configured"], true);
    assert_eq!(v["config"]["api_key"], MASK);
    assert!(!v.to_string().contains("re_super_secret"));
    // At rest: sealed, not plain.
    let stored = env
        .db
        .fetch_all(&forgebase_db::Statement::raw(
            "SELECT config FROM _forgebase_email_providers WHERE realm = '*'",
        ))
        .await
        .unwrap();
    let cfg = stored.rows[0][0].as_str().unwrap().to_owned();
    assert!(
        cfg.contains("\"api_key\":\"sealed:") && !cfg.contains("re_super_secret"),
        "{cfg}"
    );
    assert_eq!(
        env.svc.mail_provider_config(&admin(), Some("*")).await.unwrap()["config"]["api_key"],
        MASK
    );

    // The mask (or absence) keeps the stored key: the test send still uses it.
    let mut again = resend(&m.url, MASK);
    again["from_address"] = json!("other@acme.test");
    env.svc.put_mail_provider(&admin(), Some("*"), again).await.unwrap();
    let mut keep = resend(&m.url, MASK);
    keep.as_object_mut().unwrap().remove("api_key");
    env.svc.put_mail_provider(&admin(), Some("*"), keep).await.unwrap();
    let t = env
        .svc
        .send_test_mail(&admin(), Some("*"), "ann@example.com")
        .await
        .unwrap();
    assert_eq!(
        (t.provider.as_str(), t.message_id.as_deref()),
        ("resend", Some("provider-msg-1"))
    );
    let r = &m.requests()[0];
    assert_eq!(r.header("authorization"), Some("Bearer re_super_secret"));
    assert_eq!(r.json()["from"], "no-reply@acme.test");
    assert_eq!(r.json()["to"], json!(["ann@example.com"]));

    // A different provider type does not inherit the key.
    let e = env
        .svc
        .put_mail_provider(
            &admin(),
            Some("*"),
            json!({"provider": "sendgrid", "api_key": MASK, "base_url": m.url}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_mail_config");
}

#[tokio::test]
async fn configuration_is_validated() {
    let env = Env::new().await;
    runtime(&env).await;
    let bad = |v: Value| {
        let svc = env.svc.clone();
        async move { code(&svc.put_mail_provider(&admin(), Some("*"), v).await.unwrap_err()) }
    };
    for v in [
        json!({"provider": "carrier-pigeon"}),
        json!({"provider": "resend"}),
        json!({"provider": "resend", "api_key": ""}),
        json!({"provider": "resend", "api_key": "k", "base_url": "ftp://x"}),
        json!({"provider": "resend", "api_key": "k", "unknown": 1}),
        json!({"provider": "resend", "api_key": "k", "from_address": "not an address"}),
        json!({"provider": "resend", "api_key": 5}),
        json!({"provider": "sendgrid", "api_key": "k", "region": "mars"}),
        json!({"provider": "mailgun", "api_key": "k", "domain": "no-dot"}),
        json!({"provider": "mailgun", "api_key": "k", "domain": "a.b/../c"}),
        json!({"provider": "mailgun", "api_key": "k", "domain": "mg.x.com", "region": "asia"}),
        json!({"provider": "ses", "access_key_id": "AK", "secret_access_key": "s", "region": "Not A Region"}),
        json!({"provider": "postmark", "server_token": "t", "message_stream": "a b"}),
        json!({"provider": "smtp", "host": "h", "envelope_from": "bad"}),
        json!({"provider": "smtp", "host": "h", "envelope_from": "a@b.com", "username": "u"}),
        json!({"provider": "smtp", "host": "h", "envelope_from": "a@b.com", "tls": "ssl3"}),
    ] {
        assert_eq!(bad(v.clone()).await, "invalid_mail_config", "{v}");
    }
    // Realms must exist; instance and existing realms are fine.
    let e = env
        .svc
        .put_mail_provider(&admin(), Some("nope"), json!({"provider": "log"}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "not_found");
    env.svc
        .put_mail_provider(&admin(), Some("globex"), json!({"provider": "log"}))
        .await
        .unwrap();
    // A plain SMTP provider builds without connecting.
    env.svc
        .put_mail_provider(
            &admin(),
            Some("*"),
            json!({"provider": "smtp", "host": "smtp.acme.test", "tls": "implicit", "username": "u", "password": "p",
                   "envelope_from": "bounces@acme.test"}),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn deliveries_use_the_realms_provider_then_the_instances_and_log_the_message_id() {
    let env = Env::new().await;
    runtime(&env).await;
    let resend_mock = mock("id").await;
    let postmark_mock = mock("MessageID").await;
    env.svc
        .put_mail_provider(&admin(), Some("*"), resend(&resend_mock.url, "re_key"))
        .await
        .unwrap();
    env.svc
        .put_mail_provider(
            &admin(),
            Some("globex"),
            json!({"provider": "postmark", "server_token": "pm_token", "base_url": postmark_mock.url,
                   "from_address": "hello@globex.test"}),
        )
        .await
        .unwrap();

    // default realm -> the instance's Resend.
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    env.svc.deliver(&d.id, 1, false).await.unwrap();
    let d = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert_eq!(d.provider.as_deref(), Some("resend"));
    assert_eq!(d.provider_message_id.as_deref(), Some("provider-msg-1"));
    assert_eq!(resend_mock.requests().len(), 1);
    assert_eq!(resend_mock.requests()[0].header("authorization"), Some("Bearer re_key"));
    assert_eq!(resend_mock.requests()[0].json()["from"], "no-reply@acme.test");

    // globex -> its own Postmark, with its own From.
    let gop = in_realm(admin(), "globex");
    let g = env.svc.send(&gop, None, send_input("bob@example.com")).await.unwrap();
    env.svc.deliver(&g.id, 1, false).await.unwrap();
    let g = env.svc.delivery(&gop, &g.id).await.unwrap();
    assert_eq!(
        (g.provider.as_deref(), g.provider_message_id.as_deref()),
        (Some("postmark"), Some("provider-msg-1"))
    );
    assert_eq!(
        postmark_mock.requests()[0].header("x-postmark-server-token"),
        Some("pm_token")
    );
    assert_eq!(postmark_mock.requests()[0].json()["From"], "hello@globex.test");

    // The id shows in the REST view of the log (a cleared body, no secret).
    assert!(serde_json::to_string(&d).unwrap().contains("provider-msg-1"));

    // Removing the realm's provider falls back to the instance's.
    env.svc.delete_mail_provider(&admin(), Some("globex")).await.unwrap();
    let g2 = env.svc.send(&gop, None, send_input("bob@example.com")).await.unwrap();
    env.svc.deliver(&g2.id, 1, false).await.unwrap();
    assert_eq!(
        env.svc.delivery(&gop, &g2.id).await.unwrap().provider.as_deref(),
        Some("resend")
    );
    // Removing the instance's falls back to the installed (capturing log) provider.
    env.svc.delete_mail_provider(&admin(), Some("*")).await.unwrap();
    let d3 = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    env.svc.deliver(&d3.id, 1, false).await.unwrap();
    assert_eq!(
        env.svc.delivery(&op, &d3.id).await.unwrap().provider.as_deref(),
        Some("log")
    );
    assert_eq!(env.mail.sent().len(), 1);
    assert!(matches!(
        env.svc.delete_mail_provider(&admin(), Some("*")).await,
        Err(CustomizeError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_failing_provider_is_a_transient_or_permanent_delivery_failure() {
    use forgebase_customize::Attempt;
    let env = Env::new().await;
    runtime(&env).await;
    let m = mock_http::start(|_| (503, "{}".into())).await;
    env.svc
        .put_mail_provider(&admin(), Some("*"), resend(&m.url, "k"))
        .await
        .unwrap();
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    assert!(matches!(
        env.svc.deliver(&d.id, 1, false).await.unwrap(),
        Attempt::Retry(_)
    ));
    let d = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Retrying);
    assert!(d.last_error.unwrap().contains("503"));
    assert!(d.provider_message_id.is_none());
    // The test send reports the refusal instead of queueing it.
    let e = env
        .svc
        .send_test_mail(&admin(), Some("*"), "ann@example.com")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "mail_test_failed");
    let e = env
        .svc
        .send_test_mail(&admin(), Some("*"), "not-an-address")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_recipient");
}

#[tokio::test]
async fn stored_providers_reload_after_a_restart_and_a_wrong_master_key_is_skipped() {
    let env = Env::new().await;
    runtime(&env).await;
    let m = mock("id").await;
    env.svc
        .put_mail_provider(&admin(), Some("*"), resend(&m.url, "re_key"))
        .await
        .unwrap();
    env.svc
        .put_mail_provider(&admin(), Some("globex"), resend(&m.url, "re_globex_key"))
        .await
        .unwrap();

    let reopen = || async {
        let backend: Arc<dyn forgebase_db::Backend> = Arc::new(env.db.clone());
        CustomizeService::open(
            backend,
            env.policy.clone(),
            Arc::new(forgebase_auth::ManualClock::from_system()),
            CustomizeConfig::default(),
        )
        .await
        .unwrap()
    };
    // Same key: the stored configuration is live again.
    let svc = reopen().await;
    svc.attach_mail_runtime(Arc::new(PlainHttpClient::default()), keyring())
        .await
        .unwrap();
    let t = svc
        .send_test_mail(&admin(), Some("globex"), "ann@example.com")
        .await
        .unwrap();
    assert_eq!(t.provider, "resend");
    assert_eq!(
        m.requests().last().unwrap().header("authorization"),
        Some("Bearer re_globex_key")
    );

    // Another master key cannot open the secrets: skipped, not applied.
    let svc = reopen().await;
    svc.attach_mail_runtime(
        Arc::new(PlainHttpClient::default()),
        Keyring::from_master(b"some-other-master-key-0123456789"),
    )
    .await
    .unwrap();
    let t = svc
        .send_test_mail(&admin(), Some("globex"), "ann@example.com")
        .await
        .unwrap();
    assert_eq!(t.provider, "log", "fell back to the default provider");
    // The view never fails because of it: it just cannot be changed with the mask.
    assert!(svc.mail_provider_config(&admin(), Some("*")).await.is_ok());

    // A realm with its own provider is isolated from another realm's.
    let _ = in_realm(admin(), "globex");
}

#[tokio::test]
async fn providers_need_the_runtime() {
    let env = Env::new().await;
    // No attach_mail_runtime: no HTTP client, no key.
    let e = env
        .svc
        .put_mail_provider(&admin(), Some("*"), json!({"provider": "log"}))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "mail_runtime_unavailable");
}
