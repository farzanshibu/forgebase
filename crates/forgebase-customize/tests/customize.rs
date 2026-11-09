//! Service-level acceptance tests: domains, Host routing, templates, email
//! delivery and retries, agreements and the re-accept gate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use common::*;
use forgebase_api::{GateDecision, RequestGate};
use forgebase_core::ErrorCode;
use forgebase_customize::*;
use forgebase_jobs::{JobRegistry, PoolConfig, start_pool};
use serde_json::json;
use tower::ServiceExt;

fn dom(host: &str) -> DomainInput {
    DomainInput {
        host: host.into(),
        realm: None,
    }
}

// ----- domains -----------------------------------------------------------------

#[tokio::test]
async fn domain_verification_good_bad_and_expired_token() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let d = env.svc.add_domain(&op, dom("Login.Acme.test")).await.unwrap();
    assert_eq!(d.status, DomainStatus::Pending);
    assert_eq!(d.host, "login.acme.test");
    let v = d.verification.clone().unwrap();
    assert_eq!(v.record_name, "_forgebase-verify.login.acme.test");
    assert!(v.record_value.starts_with("forgebase-verify="));

    // No record yet: not verified, with a reason; nothing routes.
    let r = env.svc.verify_domain(&op, &d.id).await.unwrap();
    assert!(!r.verified && r.reason.unwrap().contains("no TXT record"));
    assert_eq!(env.svc.realm_for_host("login.acme.test"), None);

    // Wrong token: not verified.
    env.dns.add_txt(&v.record_name, "forgebase-verify=not-the-token");
    let r = env.svc.verify_domain(&op, &d.id).await.unwrap();
    assert!(!r.verified && r.reason.unwrap().contains("expected value"));

    // Resolver outage is reported, not a crash.
    env.dns.set_failing(true);
    let r = env.svc.verify_domain(&op, &d.id).await.unwrap();
    assert!(!r.verified && r.reason.unwrap().contains("DNS lookup failed"));
    env.dns.set_failing(false);

    // Right token: verified, and the host now routes to the realm.
    env.dns.add_txt(&v.record_name, &v.record_value);
    let r = env.svc.verify_domain(&op, &d.id).await.unwrap();
    assert!(r.verified);
    assert_eq!(r.domain.status, DomainStatus::Verified);
    assert!(r.domain.verification.is_none());
    assert_eq!(env.svc.realm_for_host("login.acme.test").as_deref(), Some("default"));

    // Removing it stops routing.
    env.svc.delete_domain(&op, &d.id).await.unwrap();
    assert_eq!(env.svc.realm_for_host("login.acme.test"), None);
}

#[tokio::test]
async fn expired_token_is_refused_until_reissued() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let d = env.svc.add_domain(&op, dom("a.acme.test")).await.unwrap();
    let old = d.verification.unwrap();
    env.dns.add_txt(&old.record_name, &old.record_value);
    env.advance(Duration::from_secs(8 * 24 * 3600));
    let e = env.svc.verify_domain(&op, &d.id).await.unwrap_err();
    assert_eq!(e.code(), "token_expired");
    // A new token must be published; the old record no longer matches.
    let d2 = env.svc.reissue_domain(&op, &d.id).await.unwrap();
    let new = d2.verification.unwrap();
    assert_ne!(new.record_value, old.record_value);
    assert!(!env.svc.verify_domain(&op, &d.id).await.unwrap().verified);
    env.dns.add_txt(&new.record_name, &new.record_value);
    assert!(env.svc.verify_domain(&op, &d.id).await.unwrap().verified);
}

#[tokio::test]
async fn hosts_belong_to_one_realm_and_stale_claims_can_be_taken_over() {
    let env = Env::new().await;
    let a = env.operator("olga", "default").await;
    let g = in_realm(admin(), "globex");
    let d = env.svc.add_domain(&a, dom("shared.test")).await.unwrap();
    // A live claim blocks the other realm; so does a duplicate in the same realm.
    assert_eq!(
        env.svc.add_domain(&g, dom("shared.test")).await.unwrap_err().code(),
        "domain_taken"
    );
    assert_eq!(
        env.svc.add_domain(&a, dom("shared.test")).await.unwrap_err().code(),
        "domain_taken"
    );
    // After the token expired the claim is stale: another realm may take the host.
    env.advance(Duration::from_secs(8 * 24 * 3600));
    let d2 = env.svc.add_domain(&g, dom("shared.test")).await.unwrap();
    assert_eq!(d2.realm, "globex");
    assert!(env.svc.domain(&a, &d.id).await.is_err());
    // Bad hosts and unknown realms are refused.
    assert_eq!(
        env.svc.add_domain(&a, dom("localhost")).await.unwrap_err().code(),
        "invalid_host"
    );
    let bad = DomainInput {
        host: "x.test".into(),
        realm: Some("nope".into()),
    };
    assert_eq!(env.svc.add_domain(&admin(), bad).await.unwrap_err().code(), "not_found");
}

#[tokio::test]
async fn domain_administration_is_authorized() {
    let env = Env::new().await;
    let nobody = in_realm(user("nobody"), "default");
    assert_eq!(
        env.svc
            .add_domain(&nobody, dom("a.acme.test"))
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(
        env.svc
            .add_domain(&AccessContext::anonymous(), dom("a.acme.test"))
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
    // An operator of acme cannot touch globex.
    let op = env.operator("olga", "default").await;
    let other = DomainInput {
        host: "x.globex.test".into(),
        realm: Some("globex".into()),
    };
    assert_eq!(env.svc.add_domain(&op, other).await.unwrap_err().code(), "forbidden");
    let gd = env
        .svc
        .add_domain(&in_realm(admin(), "globex"), dom("y.globex.test"))
        .await
        .unwrap();
    assert_eq!(
        env.svc.verify_domain(&op, &gd.id).await.unwrap_err().code(),
        "not_found"
    );
}

use forgebase_data::AccessContext;

// ----- Host routing --------------------------------------------------------------

fn echo_app(svc: &CustomizeService) -> Router {
    Router::new()
        .route(
            "/x",
            get(|req: Request<Body>| async move {
                let h = req
                    .headers()
                    .get("x-forgebase-realm")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_owned();
                let r = req
                    .extensions()
                    .get::<ResolvedHost>()
                    .map(|r| r.realm.clone())
                    .unwrap_or_default();
                format!("{h}|{r}")
            }),
        )
        .layer(axum::middleware::from_fn_with_state(svc.clone(), host_router))
}

async fn echo(app: &Router, host: &str, extra: &[(&str, &str)]) -> String {
    let mut b = Request::builder().uri("/x").header("host", host);
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let res = app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap().to_vec()).unwrap()
}

async fn verified(env: &Env, ctx: &AccessContext, host: &str) {
    let d = env.svc.add_domain(ctx, dom(host)).await.unwrap();
    let v = d.verification.unwrap();
    env.dns.add_txt(&v.record_name, &v.record_value);
    assert!(env.svc.verify_domain(ctx, &d.id).await.unwrap().verified);
}

#[tokio::test]
async fn host_header_routes_to_the_realm_of_the_verified_domain() {
    let env = Env::new().await;
    let a = in_realm(admin(), "default");
    let g = in_realm(admin(), "globex");
    verified(&env, &a, "login.acme.test").await;
    verified(&env, &g, "id.globex.test").await;
    let pending = env.svc.add_domain(&a, dom("pending.acme.test")).await.unwrap();
    assert_eq!(pending.status, DomainStatus::Pending);
    let app = echo_app(&env.svc);

    assert_eq!(echo(&app, "login.acme.test", &[]).await, "default|default");
    assert_eq!(echo(&app, "ID.Globex.Test:8443", &[]).await, "globex|globex");
    // The client cannot pick another realm on a custom domain.
    assert_eq!(
        echo(&app, "login.acme.test", &[("x-forgebase-realm", "globex")]).await,
        "default|default"
    );
    // X-Forwarded-Host is ignored unless the proxy is trusted.
    assert_eq!(
        echo(&app, "platform.test", &[("x-forwarded-host", "login.acme.test")]).await,
        "-|"
    );
    // Unknown and unverified hosts pass through untouched (platform host: header selects).
    assert_eq!(echo(&app, "platform.test", &[]).await, "-|");
    assert_eq!(
        echo(&app, "platform.test", &[("x-forgebase-realm", "globex")]).await,
        "globex|"
    );
    assert_eq!(echo(&app, "pending.acme.test", &[]).await, "-|");
}

#[tokio::test]
async fn forwarded_host_is_honoured_when_trusted() {
    let env = Env::with_config(CustomizeConfig {
        trust_forwarded_host: true,
        ..CustomizeConfig::default()
    })
    .await;
    verified(&env, &in_realm(admin(), "default"), "login.acme.test").await;
    let app = echo_app(&env.svc);
    assert_eq!(
        echo(&app, "proxy.internal", &[("x-forwarded-host", "login.acme.test")]).await,
        "default|default"
    );
}

// ----- templates -------------------------------------------------------------------

#[tokio::test]
async fn template_rendering_escapes_variables() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    env.svc
        .put_branding(
            &op,
            None,
            BrandingInput {
                display_name: Some("Acme <Cloud>".into()),
                ..BrandingInput::default()
            },
        )
        .await
        .unwrap();
    env.svc
        .put_template(
            &op,
            None,
            "welcome",
            TemplateInput {
                subject: "Hi {{ name }} from {{ app_name }}".into(),
                text: "Hello {{name}}\nOpen {{ link }}".into(),
                html: Some("<p>Hello {{ name }}</p><a href=\"{{ link }}\">open</a><i>{{ missing_var }}</i>".into()),
            },
        )
        .await
        .unwrap();
    let evil = "<img src=x onerror=alert(1)>\r\nBcc: evil@example.com";
    let p = env
        .svc
        .preview(
            &op,
            None,
            PreviewInput {
                template: Some("welcome".into()),
                draft: None,
                variables: json!({"name": evil, "link": "javascript:alert(1)", "app_name": "SPOOFED"}),
            },
        )
        .await
        .unwrap();
    let html = p.html.unwrap();
    assert!(!html.contains("<img"), "{html}");
    assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(html.contains("href=\"#\""), "javascript: must not survive: {html}");
    assert!(!p.subject.contains('\n') && !p.subject.contains('\r'));
    // System variables cannot be overridden by the caller.
    assert!(p.subject.contains("Acme <Cloud>") && !p.subject.contains("SPOOFED"));
    assert_eq!(p.missing, vec!["missing_var"]);
}

#[tokio::test]
async fn templates_validate_override_and_revert() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let all = env.svc.templates(&op, None).await.unwrap();
    assert!(all.iter().any(|t| t.key == "password_reset" && !t.customized));
    let bad = TemplateInput {
        subject: "s".into(),
        text: "{{ unclosed".into(),
        html: None,
    };
    assert_eq!(
        env.svc
            .put_template(&op, None, "password_reset", bad)
            .await
            .unwrap_err()
            .code(),
        "invalid_template"
    );
    env.svc
        .put_template(
            &op,
            None,
            "password_reset",
            TemplateInput {
                subject: "Custom {{ app_name }}".into(),
                text: "Go {{ link }}".into(),
                html: None,
            },
        )
        .await
        .unwrap();
    assert!(env.svc.template(&op, None, "password_reset").await.unwrap().customized);
    // Another realm still sees the built-in one.
    let g = in_realm(admin(), "globex");
    assert!(!env.svc.template(&g, None, "password_reset").await.unwrap().customized);
    env.svc.delete_template(&op, None, "password_reset").await.unwrap();
    assert!(!env.svc.template(&op, None, "password_reset").await.unwrap().customized);
    assert_eq!(
        env.svc.template(&op, None, "nope").await.unwrap_err().code(),
        "not_found"
    );
}

// ----- email delivery ----------------------------------------------------------------

fn send_input(to: &str) -> SendInput {
    SendInput {
        template: "magic_link".into(),
        to: to.into(),
        variables: json!({"link": "https://acme.test/m?t=1"}),
    }
}

async fn run_pool(env: &Env, until: impl Fn() -> bool) {
    let registry = register_jobs(JobRegistry::new(), env.svc.clone());
    let pool = start_pool(
        env.jobs.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_millis(50),
            ..PoolConfig::default()
        },
    );
    for _ in 0..400 {
        if until() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pool.shutdown().await;
}

#[tokio::test]
async fn email_is_sent_through_the_queue_and_logged() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Queued);
    assert!(d.job_id.is_some());
    // The body is stored for the attempt, then cleared.
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_email_deliveries WHERE text_body IS NOT NULL")
            .await,
        1
    );
    let mail = env.mail.clone();
    run_pool(&env, || !mail.sent().is_empty()).await;
    let sent = env.mail.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "ann@example.com");
    assert!(sent[0].text.contains("https://acme.test/m?t=1"));
    let d = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert_eq!((d.attempts, d.provider.as_deref()), (1, Some("log")));
    assert!(d.sent_at.is_some());
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_email_deliveries WHERE text_body IS NOT NULL")
            .await,
        0
    );
    // The log lists it; the other realm does not see it.
    let log = env.svc.deliveries(&op, None, DeliveryQuery::default()).await.unwrap();
    assert_eq!(log.len(), 1);
    let g = in_realm(admin(), "globex");
    assert!(
        env.svc
            .deliveries(&g, None, DeliveryQuery::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn transient_failures_are_retried_then_succeed() {
    let env = Env::new().await;
    let flaky = Flaky::new(2, false);
    env.svc.set_mail_provider(flaky.clone());
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    let svc = env.svc.clone();
    let id = d.id.clone();
    let op2 = op.clone();
    let f = flaky.clone();
    run_pool(&env, move || {
        f.calls() >= 3 && {
            // poll the log synchronously from the pool wait loop
            let _ = (&svc, &id, &op2);
            true
        }
    })
    .await;
    // Give the last update a moment (pool shutdown waits for running jobs).
    let d = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(flaky.calls(), 3);
    assert_eq!(d.status, DeliveryStatus::Sent);
    assert_eq!(d.attempts, 3);
    assert!(d.last_error.is_none());
}

#[tokio::test]
async fn permanent_failure_stops_and_can_be_retried_by_hand() {
    let env = Env::new().await;
    let flaky = Flaky::new(1, true);
    env.svc.set_mail_provider(flaky.clone());
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    let f = flaky.clone();
    run_pool(&env, move || f.calls() >= 1).await;
    let d1 = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(d1.status, DeliveryStatus::Failed);
    assert_eq!(d1.attempts, 1);
    assert!(d1.last_error.unwrap().contains("simulated failure"));
    assert_eq!(flaky.calls(), 1, "a permanent failure is not retried");
    // Manual retry re-queues it and it goes through.
    let q = env.svc.retry_delivery(&op, &d.id).await.unwrap();
    assert_eq!(q.status, DeliveryStatus::Queued);
    let f = flaky.clone();
    run_pool(&env, move || f.calls() >= 2).await;
    assert_eq!(env.svc.delivery(&op, &d.id).await.unwrap().status, DeliveryStatus::Sent);
    // Only failed deliveries can be retried.
    assert_eq!(
        env.svc.retry_delivery(&op, &d.id).await.unwrap_err().code(),
        "not_failed"
    );
}

#[tokio::test]
async fn exhausted_retries_end_failed() {
    let env = Env::with_config(CustomizeConfig {
        email_max_attempts: 2,
        email_backoff_base_ms: 20,
        ..CustomizeConfig::default()
    })
    .await;
    let flaky = Flaky::new(100, false);
    env.svc.set_mail_provider(flaky.clone());
    let op = env.operator("olga", "default").await;
    let d = env.svc.send(&op, None, send_input("ann@example.com")).await.unwrap();
    let f = flaky.clone();
    run_pool(&env, move || f.calls() >= 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let d = env.svc.delivery(&op, &d.id).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Failed);
    assert_eq!(d.attempts, 2);
    assert_eq!(flaky.calls(), 2);
}

#[tokio::test]
async fn send_validates_recipient_and_authorization() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    assert_eq!(
        env.svc
            .send(&op, None, send_input("a@b.com\r\nRCPT TO:<x@y.z>"))
            .await
            .unwrap_err()
            .code(),
        "invalid_recipient"
    );
    let nobody = in_realm(user("nobody"), "default");
    assert_eq!(
        env.svc
            .send(&nobody, None, send_input("a@b.com"))
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(
        env.svc
            .send(
                &op,
                None,
                SendInput {
                    template: "nope".into(),
                    ..send_input("a@b.com")
                }
            )
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

// ----- agreements ----------------------------------------------------------------------

fn terms(title: &str) -> AgreementInput {
    AgreementInput {
        kind: "terms".into(),
        title: title.into(),
        body: "You agree.".into(),
        url: Some("https://acme.test/terms".into()),
    }
}

fn gate_rejects(env: &Env, ctx: &AccessContext, path: &str) -> Option<String> {
    match AgreementGate::new(env.svc.clone()).admit(ctx, path) {
        GateDecision::Pass(_) => None,
        GateDecision::Reject(e, _) => Some(e.code().to_owned()),
    }
}

#[tokio::test]
async fn new_agreement_version_forces_reaccept_through_the_gate() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let ann = in_realm(user("ann"), "default");
    // No agreement published: nothing is required.
    assert_eq!(gate_rejects(&env, &ann, "/api/v1/collections"), None);

    let v1 = env.svc.publish_agreement(&op, None, terms("Terms 2026")).await.unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(
        gate_rejects(&env, &ann, "/api/v1/collections").as_deref(),
        Some("agreement_required")
    );
    // Exempt paths, other principals and other realms are not gated.
    assert_eq!(gate_rejects(&env, &ann, "/api/auth/login"), None);
    assert_eq!(gate_rejects(&env, &ann, "/api/v1/customize/agreements/accept"), None);
    assert_eq!(
        gate_rejects(&env, &AccessContext::anonymous(), "/api/v1/collections"),
        None
    );
    assert_eq!(gate_rejects(&env, &admin(), "/api/v1/collections"), None);
    assert_eq!(
        gate_rejects(&env, &in_realm(user("ann"), "globex"), "/api/v1/collections"),
        None
    );
    let mut imp = ann.clone();
    imp.impersonator = Some("root".into());
    assert_eq!(gate_rejects(&env, &imp, "/api/v1/collections"), None);

    // Accepting records time and source, and opens the gate.
    let acc = env
        .svc
        .accept(
            &ann,
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "terms".into(),
                    version: 1,
                }],
            },
            AcceptSource {
                ip: Some("203.0.113.7".into()),
                user_agent: Some("test-agent/1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(acc.len(), 1);
    assert_eq!(
        (acc[0].version, acc[0].ip.as_deref(), acc[0].user_agent.as_deref()),
        (1, Some("203.0.113.7"), Some("test-agent/1"))
    );
    assert!(acc[0].accepted_at > 0 && acc[0].principal == pk("ann"));
    assert_eq!(gate_rejects(&env, &ann, "/api/v1/collections"), None);
    assert!(env.svc.pending(&ann).unwrap().is_empty());

    // Version 2 makes everybody accept again; version 1 is no longer acceptable.
    let v2 = env.svc.publish_agreement(&op, None, terms("Terms 2027")).await.unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(
        gate_rejects(&env, &ann, "/api/v1/collections").as_deref(),
        Some("agreement_required")
    );
    assert_eq!(
        env.svc.pending(&ann).unwrap(),
        vec![AgreementRef {
            kind: "terms".into(),
            version: 2,
            title: "Terms 2027".into()
        }]
    );
    let stale = AcceptInput {
        items: vec![AcceptItem {
            kind: "terms".into(),
            version: 1,
        }],
    };
    assert_eq!(
        env.svc
            .accept(&ann, stale, AcceptSource::default())
            .await
            .unwrap_err()
            .code(),
        "agreement_outdated"
    );
    env.svc
        .accept(
            &ann,
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "terms".into(),
                    version: 2,
                }],
            },
            AcceptSource::default(),
        )
        .await
        .unwrap();
    assert_eq!(gate_rejects(&env, &ann, "/api/v1/collections"), None);
    // Accepting twice is a no-op; the history keeps both versions.
    env.svc
        .accept(
            &ann,
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "terms".into(),
                    version: 2,
                }],
            },
            AcceptSource::default(),
        )
        .await
        .unwrap();
    assert_eq!(env.svc.my_acceptances(&ann).await.unwrap().len(), 2);
    let all = env
        .svc
        .acceptances(&op, None, AcceptanceQuery::default())
        .await
        .unwrap();
    assert_eq!(all.len(), 2);

    // The state survives a restart (it is rebuilt from the database).
    env.svc.reload_agreements().await.unwrap();
    assert_eq!(gate_rejects(&env, &ann, "/api/v1/collections"), None);
    // A second kind gates again.
    env.svc
        .publish_agreement(
            &op,
            None,
            AgreementInput {
                kind: "privacy".into(),
                title: "Privacy".into(),
                body: "p".into(),
                url: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        gate_rejects(&env, &ann, "/api/v1/collections").as_deref(),
        Some("agreement_required")
    );
}

#[tokio::test]
async fn acceptance_records_are_immutable_and_versions_too() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let ann = in_realm(user("ann"), "default");
    env.svc.publish_agreement(&op, None, terms("T")).await.unwrap();
    env.svc
        .accept(
            &ann,
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "terms".into(),
                    version: 1,
                }],
            },
            AcceptSource::default(),
        )
        .await
        .unwrap();
    use forgebase_db::{Backend, Statement, TxMode};
    for sql in [
        "UPDATE _forgebase_agreement_acceptances SET version = 9",
        "DELETE FROM _forgebase_agreement_acceptances",
        "UPDATE _forgebase_agreements SET body = 'changed'",
        "DELETE FROM _forgebase_agreements",
    ] {
        let mut tx = env.db.begin(TxMode::Normal).await.unwrap();
        assert!(tx.execute(&Statement::raw(sql)).await.is_err(), "{sql}");
        let _ = tx.rollback().await;
    }
    // Only users accept; unknown kinds are not found.
    let e = env
        .svc
        .accept(
            &op,
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "nope".into(),
                    version: 1,
                }],
            },
            AcceptSource::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "not_found");
    let e = env
        .svc
        .accept(
            &admin(),
            AcceptInput {
                items: vec![AcceptItem {
                    kind: "terms".into(),
                    version: 1,
                }],
            },
            AcceptSource::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "forbidden");
    // Publishing needs the permission.
    let nobody = in_realm(user("nobody"), "default");
    assert_eq!(
        env.svc
            .publish_agreement(&nobody, None, terms("T"))
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    // Bad input.
    assert_eq!(
        env.svc
            .publish_agreement(
                &op,
                None,
                AgreementInput {
                    url: Some("javascript:x".into()),
                    ..terms("T")
                }
            )
            .await
            .unwrap_err()
            .code(),
        "invalid_agreement"
    );
}

// ----- branding ------------------------------------------------------------------------

#[tokio::test]
async fn branding_validation_and_assets() {
    let env = Env::new().await;
    let op = env.operator("olga", "default").await;
    let bad = BrandingInput {
        primary_color: Some("red;}".into()),
        ..BrandingInput::default()
    };
    assert_eq!(
        env.svc.put_branding(&op, None, bad).await.unwrap_err().code(),
        "invalid_branding"
    );
    let b = env
        .svc
        .put_branding(
            &op,
            None,
            BrandingInput {
                display_name: Some("Acme".into()),
                primary_color: Some("#FF0000".into()),
                signin_title: Some("Welcome <b>back</b>".into()),
                support_email: Some("help@acme.test".into()),
                ..BrandingInput::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(b.primary_color.as_deref(), Some("#ff0000"));
    // PUT replaces: fields left out are cleared.
    let b = env.svc.put_branding(&op, None, BrandingInput::default()).await.unwrap();
    assert!(b.display_name.is_none() && b.primary_color.is_none());
    // Mismatched content is refused (HTML as PNG), real PNG accepted.
    let e = env
        .svc
        .put_asset(&op, None, AssetKind::Logo, "image/png", b"<html>")
        .await
        .unwrap_err();
    assert_eq!(e.code(), "invalid_asset");
    let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
    assert!(
        env.svc
            .put_asset(&op, None, AssetKind::Logo, "image/png", &png)
            .await
            .unwrap()
            .has_logo
    );
    let p = env.svc.public_branding("default").await.unwrap();
    assert_eq!(p.display_name, "default");
    assert_eq!(p.logo_url.as_deref(), Some("/api/v1/customize/public/branding/logo"));
    assert!(env.svc.public_branding("nope").await.is_err());
}

#[test]
fn smtp_types_are_constructible() {
    let c = SmtpConfig::new("smtp.example.com", "no-reply@example.com");
    assert_eq!(c.port, 587);
    assert_eq!(c.tls, SmtpTls::StartTls);
    let _ = SmtpProvider::new(c);
    let _ = StatusCode::OK;
}
