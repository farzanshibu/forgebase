//! REST, public views and the docs page through the real router (no sockets).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use common::*;
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::DataLayout;
use forgebase_customize::*;
use forgebase_data::AccessContext;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct Api {
    env: Env,
    app: Router,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
}

impl Res {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.bytes).unwrap_or(Json::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

impl Api {
    async fn new() -> Self {
        let env = Env::new().await;
        let layout = DataLayout::new(env.tmp.data_dir().join("data"));
        let state = AppState::from_data(env.db.clone(), layout, env.data.clone());
        let app = forgebase_api::router(
            state,
            RouterOptions {
                extensions: vec![extension(env.svc.clone())],
                gates: vec![std::sync::Arc::new(AgreementGate::new(env.svc.clone()))],
                ..RouterOptions::default()
            },
        )
        .layer(axum::middleware::from_fn_with_state(env.svc.clone(), host_router));
        Self { env, app }
    }

    async fn call(
        &self,
        ctx: Option<AccessContext>,
        method: Method,
        uri: &str,
        host: Option<&str>,
        ct: Option<&str>,
        body: Vec<u8>,
    ) -> Res {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(h) = host {
            req = req.header("host", h);
        }
        if let Some(c) = ct {
            req = req.header("content-type", c);
        }
        let mut req = req.header("user-agent", "http-test/1").body(Body::from(body)).unwrap();
        if let Some(ctx) = ctx {
            req.extensions_mut().insert(ctx);
        }
        req.extensions_mut().insert(ConnectInfo::<std::net::SocketAddr>(
            "198.51.100.9:5555".parse().unwrap(),
        ));
        let res = self.app.clone().oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        Res {
            status: parts.status,
            headers: parts.headers,
            bytes: axum::body::to_bytes(body, 1 << 20).await.unwrap().to_vec(),
        }
    }

    async fn json(&self, ctx: &AccessContext, m: Method, uri: &str, body: Json) -> Res {
        self.call(
            Some(ctx.clone()),
            m,
            uri,
            None,
            Some("application/json"),
            body.to_string().into_bytes(),
        )
        .await
    }

    async fn get(&self, ctx: Option<&AccessContext>, uri: &str, host: Option<&str>) -> Res {
        self.call(ctx.cloned(), Method::GET, uri, host, None, Vec::new()).await
    }
}

#[tokio::test]
async fn domain_flow_over_http_and_public_branding_by_host() {
    let api = Api::new().await;
    let a = in_realm(admin(), "globex");
    let r = api
        .json(
            &a,
            Method::POST,
            "/api/v1/customize/domains",
            json!({"host": "id.globex.test"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let d = r.json();
    let rec = &d["verification"];
    assert_eq!(rec["record_type"], "TXT");
    api.env.dns.add_txt(
        rec["record_name"].as_str().unwrap(),
        rec["record_value"].as_str().unwrap(),
    );
    let r = api
        .json(
            &a,
            Method::POST,
            &format!("/api/v1/customize/domains/{}/verify", d["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(r.json()["verified"], true);

    api.json(
        &a,
        Method::PUT,
        "/api/v1/customize/branding",
        json!({"display_name": "Globex", "primary_color": "#112233", "signin_title": "<b>Hi</b>"}),
    )
    .await;
    // Anonymous, on the custom host: the realm comes from the Host header.
    let r = api
        .get(None, "/api/v1/customize/public/branding", Some("id.globex.test"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let b = r.json();
    assert_eq!(
        (b["realm"].as_str(), b["display_name"].as_str()),
        (Some("globex"), Some("Globex"))
    );
    assert_eq!(b["domains"], json!(["id.globex.test"]));
    // On the platform host the realm is chosen with ?realm=; default otherwise.
    let r = api
        .get(
            None,
            "/api/v1/customize/public/branding?realm=globex",
            Some("platform.test"),
        )
        .await;
    assert_eq!(r.json()["display_name"], "Globex");
    let r = api
        .get(None, "/api/v1/customize/public/branding", Some("platform.test"))
        .await;
    assert_eq!(r.json()["realm"], "default");
    assert_eq!(
        api.get(None, "/api/v1/customize/public/branding?realm=nope", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // The theme is CSS variables from validated colours.
    let css = api
        .get(None, "/api/v1/customize/public/theme.css", Some("id.globex.test"))
        .await;
    assert_eq!(css.text(), ":root{--fb-primary:#112233;}");
    // Admin endpoints need credentials.
    assert_eq!(
        api.get(None, "/api/v1/customize/domains", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let nobody = in_realm(user("nobody"), "default");
    assert_eq!(
        api.get(Some(&nobody), "/api/v1/customize/domains", None).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn assets_are_served_safely() {
    let api = Api::new().await;
    let a = admin();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#.to_vec();
    let r = api
        .call(
            Some(a.clone()),
            Method::PUT,
            "/api/v1/customize/branding/assets/logo",
            None,
            Some("image/svg+xml"),
            svg.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["has_logo"], true);
    let r = api.get(None, "/api/v1/customize/public/branding/logo", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.bytes, svg);
    assert_eq!(r.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    let csp = r.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("sandbox") && csp.contains("default-src 'none'"));
    // Conditional requests.
    let etag = r.headers[header::ETAG].to_str().unwrap().to_owned();
    let mut req = Request::builder()
        .uri("/api/v1/customize/public/branding/logo")
        .header("if-none-match", etag)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(AccessContext::anonymous());
    assert_eq!(
        api.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_MODIFIED
    );
    // Wrong content, wrong kind, oversize.
    let r = api
        .call(
            Some(a.clone()),
            Method::PUT,
            "/api/v1/customize/branding/assets/logo",
            None,
            Some("image/png"),
            b"<html>".to_vec(),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = api
        .call(
            Some(a.clone()),
            Method::PUT,
            "/api/v1/customize/branding/assets/banner",
            None,
            Some("image/png"),
            vec![1],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let mut big = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    big.resize(300 * 1024, 0);
    let r = api
        .call(
            Some(a),
            Method::PUT,
            "/api/v1/customize/branding/assets/logo",
            None,
            Some("image/png"),
            big,
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn docs_page_is_branded_and_escaped() {
    let api = Api::new().await;
    let a = admin();
    api.json(&a, Method::PUT, "/api/v1/customize/branding", json!({"display_name": "<script>alert(1)</script>Acme", "primary_color": "#abcdef", "support_email": "help@acme.test"}))
        .await;
    let r = api.get(None, "/api/v1/customize/docs", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    let html = r.text();
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;Acme API"),
        "{}",
        &html[..400.min(html.len())]
    );
    assert!(!html.contains("<script>alert(1)"));
    assert!(html.contains("--fb-primary:#abcdef") && html.contains("/api/openapi.json"));
    assert!(html.contains("help@acme.test"));
    let csp = r.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("connect-src 'self'") && csp.contains("frame-ancestors 'none'"));
}

#[tokio::test]
async fn openapi_documents_the_extension() {
    let api = Api::new().await;
    let r = api.get(None, "/api/openapi.json", None).await;
    let doc = r.json();
    for p in [
        "/api/v1/customize/domains",
        "/api/v1/customize/domains/{id}/verify",
        "/api/v1/customize/branding",
        "/api/v1/customize/email/send",
        "/api/v1/customize/email/deliveries",
        "/api/v1/customize/agreements/accept",
        "/api/v1/customize/public/branding",
        "/api/v1/customize/docs",
    ] {
        assert!(doc["paths"][p].is_object(), "{p} missing");
    }
}

#[tokio::test]
async fn agreement_gate_over_http_with_acceptance_record() {
    let api = Api::new().await;
    let a = admin();
    let r = api
        .json(
            &a,
            Method::POST,
            "/api/v1/customize/agreements",
            json!({"kind": "terms", "title": "Terms", "body": "text"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let ann = in_realm(user("ann"), "default");
    // Blocked everywhere except the agreement routes.
    let r = api.get(Some(&ann), "/api/v1/collections", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["code"], "agreement_required");
    assert_eq!(r.json()["error"]["details"]["pending"][0]["version"], 1);
    let r = api.get(Some(&ann), "/api/v1/customize/agreements/pending", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["pending"][0]["kind"], "terms");
    // Public text is readable anonymously.
    let r = api.get(None, "/api/v1/customize/public/agreements", None).await;
    assert_eq!(r.json()["items"][0]["body"], "text");

    let r = api
        .json(
            &ann,
            Method::POST,
            "/api/v1/customize/agreements/accept",
            json!({"items": [{"kind": "terms", "version": 1}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rec = &r.json()["items"][0];
    assert_eq!(rec["ip"], "198.51.100.9");
    assert_eq!(rec["user_agent"], "http-test/1");
    assert!(rec["accepted_at"].as_i64().unwrap() > 0);
    // The gate opens (the request now reaches the handler; any status but the gate's).
    let r = api.get(Some(&ann), "/api/v1/collections", None).await;
    assert_ne!(r.json()["error"]["code"], "agreement_required");

    // A new version closes it again until accepted.
    api.json(
        &a,
        Method::POST,
        "/api/v1/customize/agreements",
        json!({"kind": "terms", "title": "Terms v2", "body": "new"}),
    )
    .await;
    let r = api.get(Some(&ann), "/api/v1/collections", None).await;
    assert_eq!(r.json()["error"]["code"], "agreement_required");
    let r = api
        .json(
            &ann,
            Method::POST,
            "/api/v1/customize/agreements/accept",
            json!({"items": [{"kind": "terms", "version": 1}]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["error"]["code"], "agreement_outdated");
}

#[tokio::test]
async fn email_endpoints() {
    let api = Api::new().await;
    let a = admin();
    let r = api
        .json(
            &a,
            Method::POST,
            "/api/v1/customize/email/preview",
            json!({"template": "email_otp", "variables": {"code": "<123>"}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = r.json();
    assert!(p["html"].as_str().unwrap().contains("&lt;123&gt;"));
    assert!(
        p["subject"].as_str().unwrap().contains("<123>"),
        "subject is plain text"
    );
    let r = api
        .json(
            &a,
            Method::POST,
            "/api/v1/customize/email/send",
            json!({"template": "email_otp", "to": "ann@example.com", "variables": {"code": "1"}}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let r = api
        .get(Some(&a), "/api/v1/customize/email/deliveries?status=queued", None)
        .await;
    assert_eq!(r.json()["items"][0]["id"], id.as_str());
    // The log never exposes bodies.
    assert!(!r.text().contains("sign-in code"));
    assert_eq!(
        api.get(Some(&a), "/api/v1/customize/email/deliveries?status=bogus", None)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn mail_provider_routes_configure_mask_test_and_delete() {
    use forgebase_auth::crypto::Keyring;
    let api = Api::new().await;
    api.env
        .svc
        .attach_mail_runtime(
            std::sync::Arc::new(forgebase_auth::social::PlainHttpClient::default()),
            Keyring::from_master(b"a-test-master-key-of-32-bytes!!!"),
        )
        .await
        .unwrap();
    let mock = mock_http::start(|_| (200, json!({"id": "re-77"}).to_string())).await;
    let a = admin();
    let uri = "/api/v1/customize/email/provider?realm=*";
    let r = api.get(Some(&a), uri, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["configured"], false);

    let r = api
        .json(
            &a,
            Method::PUT,
            uri,
            json!({"provider": "resend", "api_key": "re_http_secret", "base_url": mock.url, "from_address": "no-reply@acme.test"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["config"]["api_key"], "********");
    assert!(!r.text().contains("re_http_secret"));
    assert!(!api.get(Some(&a), uri, None).await.text().contains("re_http_secret"));

    let r = api
        .json(
            &a,
            Method::POST,
            "/api/v1/customize/email/provider/test?realm=*",
            json!({"to": "ann@example.com"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["provider"], "resend");
    assert_eq!(r.json()["message_id"], "re-77");
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer re_http_secret")
    );

    // A realm operator cannot touch it.
    let op = api.env.operator("olga", "default").await;
    let r = api.get(Some(&op), uri, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // Bad config is a 4xx with a stable code.
    let r = api
        .json(&a, Method::PUT, uri, json!({"provider": "resend", "api_key": ""}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["error"]["code"], "invalid_mail_config");

    let r = api
        .call(Some(a.clone()), Method::DELETE, uri, None, None, Vec::new())
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(api.get(Some(&a), uri, None).await.json()["configured"], false);
    // The OpenAPI document lists the routes.
    let doc = api.get(None, "/api/openapi.json", None).await.json();
    assert!(doc["paths"]["/api/v1/customize/email/provider"]["put"].is_object());
    assert!(doc["paths"]["/api/v1/customize/email/provider/test"]["post"].is_object());
}
