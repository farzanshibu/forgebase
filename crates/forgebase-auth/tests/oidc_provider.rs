//! FB-26 acceptance: ForgeBase as identity provider. OpenID Connect
//! conformance-style tests over the real router and a real SQLite database:
//! code flow with PKCE, refused attacks (wrong verifier, redirect URI
//! mismatch, code replay, refresh reuse, cross-realm clients), device flow
//! (slow_down, expiry), prompt=none, max_age, consent, RP-initiated logout,
//! client credentials, introspection, revocation and ID tokens verified with
//! the published JWKS.

#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{H, PW, harness};
use forgebase_auth::ManualClock;
use forgebase_auth::social::sig::PublicKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const CB: &str = "https://app.example.com/cb";
const LOGOUT_CB: &str = "https://app.example.com/bye";

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
    json: Value,
}

impl Raw {
    fn location(&self) -> String {
        self.headers
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default()
    }

    /// Query parameters of the `Location` header.
    fn loc_params(&self) -> HashMap<String, String> {
        let loc = self.location();
        assert!(
            !loc.is_empty(),
            "no Location; status {} body {}",
            self.status,
            self.text
        );
        url::Url::parse(&loc).unwrap().query_pairs().into_owned().collect()
    }
}

async fn call(h: &H, method: Method, uri: &str, headers: &[(&str, String)], body: Option<(&str, String)>) -> Raw {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "oidc-test")
        .header("x-forwarded-for", "10.0.0.9");
    for (k, v) in headers {
        b = b.header(*k, v.as_str());
    }
    let req = match body {
        Some((ct, v)) => b.header("content-type", ct).body(Body::from(v)).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Raw {
        status,
        headers,
        json: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

fn enc(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

fn basic(id: &str, secret: &str) -> (&'static str, String) {
    (
        "authorization",
        format!("Basic {}", STANDARD.encode(format!("{id}:{secret}"))),
    )
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn issuer(realm: &str) -> String {
    format!("http://127.0.0.1:8090/api/auth/oidc/{realm}")
}

/// A browser: the session and CSRF cookies of a cookie sign-in.
#[derive(Clone)]
struct Browser {
    cookie: String,
    csrf: String,
}

impl Browser {
    fn header(&self) -> (&'static str, String) {
        ("cookie", format!("fb_session={}", self.cookie))
    }
}

async fn browser_login(h: &H, email: &str, realm: Option<&str>) -> Browser {
    let mut headers = Vec::new();
    if let Some(r) = realm {
        headers.push(("x-forgebase-realm", r.to_owned()));
    }
    let r = call(
        h,
        Method::POST,
        "/api/auth/login",
        &headers,
        Some((
            "application/json",
            json!({"email": email, "password": PW, "cookie": true}).to_string(),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let mut cookie = String::new();
    let mut csrf = String::new();
    for v in r.headers.get_all(SET_COOKIE) {
        let v = v.to_str().unwrap();
        let kv = v.split(';').next().unwrap();
        if let Some(c) = kv.strip_prefix("fb_session=") {
            cookie = c.to_owned();
        }
        if let Some(c) = kv.strip_prefix("fb_csrf=") {
            csrf = c.to_owned();
        }
    }
    assert!(!cookie.is_empty() && !csrf.is_empty());
    Browser { cookie, csrf }
}

async fn create_client(h: &H, admin: &str, body: Value) -> (String, Option<String>) {
    let r = h.post("/api/auth/clients", Some(admin), body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    (
        r.json["client"]["client_id"].as_str().unwrap().to_owned(),
        r.json["client_secret"].as_str().map(str::to_owned),
    )
}

async fn confidential(h: &H, admin: &str) -> (String, String) {
    let (id, secret) = create_client(
        h,
        admin,
        json!({
            "name": "Acme Web",
            "redirect_uris": [CB],
            "post_logout_redirect_uris": [LOGOUT_CB],
            "scopes": ["openid", "profile", "email", "offline_access"],
        }),
    )
    .await;
    (id, secret.unwrap())
}

fn authorize_url(realm: &str, client_id: &str, extra: &[(&str, &str)]) -> String {
    let mut p = vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", CB),
        ("scope", "openid email profile"),
        ("state", "st-123"),
        ("nonce", "n-456"),
    ];
    for (k, v) in extra {
        p.retain(|(pk, _)| pk != k);
        p.push((k, v));
    }
    p.retain(|(_, v)| !v.is_empty());
    format!("/api/auth/oidc/{realm}/authorize?{}", enc(&p))
}

/// Runs the browser part of the code flow (consenting when asked) and
/// returns the redirect parameters (code, state, iss, or error).
async fn browser_authorize(h: &H, realm: &str, b: &Browser, url: &str) -> HashMap<String, String> {
    let r = call(
        h,
        Method::GET,
        url,
        &[b.header(), ("accept", "application/json".into())],
        None,
    )
    .await;
    if r.status == StatusCode::OK && r.json["consent_required"] == true {
        let rid = r.json["request_id"].as_str().unwrap();
        let r = call(
            h,
            Method::POST,
            &format!("/api/auth/oidc/{realm}/consent"),
            &[b.header()],
            Some((
                "application/x-www-form-urlencoded",
                enc(&[("request_id", rid), ("csrf_token", &b.csrf), ("decision", "allow")]),
            )),
        )
        .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text);
        return r.loc_params();
    }
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.text);
    r.loc_params()
}

async fn token(h: &H, realm: &str, form: &[(&str, &str)], auth: Option<(&str, &str)>) -> Raw {
    let mut headers = Vec::new();
    if let Some((id, s)) = auth {
        headers.push(basic(id, s));
    }
    call(
        h,
        Method::POST,
        &format!("/api/auth/oidc/{realm}/token"),
        &headers,
        Some(("application/x-www-form-urlencoded", enc(form))),
    )
    .await
}

async fn introspect(h: &H, realm: &str, id: &str, secret: &str, tok: &str) -> Value {
    let r = call(
        h,
        Method::POST,
        &format!("/api/auth/oidc/{realm}/introspect"),
        &[basic(id, secret)],
        Some(("application/x-www-form-urlencoded", enc(&[("token", tok)]))),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

fn assert_oauth_error(r: &Raw, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.text);
    assert_eq!(r.json["error"], code, "{}", r.text);
}

/// Verifies a JWS with the realm's published JWKS and returns its claims.
async fn verify_with_jwks(h: &H, realm: &str, token: &str) -> (Value, Value) {
    let jwks = call(h, Method::GET, &format!("/api/auth/oidc/{realm}/jwks"), &[], None).await;
    assert_eq!(jwks.status, StatusCode::OK);
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    let key = jwks.json["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kid"] == header["kid"])
        .expect("kid published in the JWKS")
        .clone();
    assert_eq!(key["kty"], "EC");
    assert_eq!(key["crv"], "P-256");
    let x = URL_SAFE_NO_PAD.decode(key["x"].as_str().unwrap()).unwrap();
    let y = URL_SAFE_NO_PAD.decode(key["y"].as_str().unwrap()).unwrap();
    let pk = PublicKey::p256(&x, &y).unwrap();
    let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    assert!(
        pk.verify_jws(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig),
        "signature verifies with the JWKS"
    );
    let mut tampered = sig.clone();
    tampered[5] ^= 1;
    assert!(!pk.verify_jws(format!("{}.{}", parts[0], parts[1]).as_bytes(), &tampered));
    let claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    (header, claims)
}

async fn code_flow(h: &H, b: &Browser, id: &str, secret: &str, extra: &[(&str, &str)]) -> Value {
    let verifier = "verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    let ch = challenge(verifier);
    let mut ex = vec![("code_challenge", ch.as_str()), ("code_challenge_method", "S256")];
    ex.extend_from_slice(extra);
    let p = browser_authorize(h, "default", b, &authorize_url("default", id, &ex)).await;
    let code = p.get("code").unwrap_or_else(|| panic!("no code: {p:?}"));
    let r = token(
        h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CB),
            ("code_verifier", verifier),
        ],
        Some((id, secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_and_jwks_are_published_per_realm() {
    let h = harness(|_| {}).await;
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/default/.well-known/openid-configuration",
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let d = &r.json;
    let iss = issuer("default");
    assert_eq!(d["issuer"], iss.as_str());
    assert_eq!(d["authorization_endpoint"], format!("{iss}/authorize"));
    assert_eq!(d["token_endpoint"], format!("{iss}/token"));
    assert_eq!(d["jwks_uri"], format!("{iss}/jwks"));
    assert_eq!(
        d["device_authorization_endpoint"],
        format!("{iss}/device_authorization")
    );
    assert_eq!(d["end_session_endpoint"], format!("{iss}/logout"));
    assert_eq!(d["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(d["id_token_signing_alg_values_supported"], json!(["ES256"]));
    assert_eq!(d["authorization_response_iss_parameter_supported"], true);

    let k = call(&h, Method::GET, "/api/auth/oidc/default/jwks", &[], None).await;
    assert_eq!(k.status, StatusCode::OK);
    let keys = k.json["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["alg"], "ES256");
    assert_eq!(keys[0]["use"], "sig");
    assert!(keys[0].get("d").is_none(), "no private part");
    // The private scalar is sealed at rest.
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oidc_keys WHERE length(private_enc) > 32")
            .await,
        1
    );

    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/nowhere/.well-known/openid-configuration",
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Rotation: new kid, the old one stays published.
    let admin = h.root_token().await;
    let r = h
        .post("/api/auth/keys/oidc/rotate", Some(&admin), json!({"realm": "default"}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let k2 = call(&h, Method::GET, "/api/auth/oidc/default/jwks", &[], None).await;
    let kids: Vec<&str> = k2.json["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kid"].as_str().unwrap())
        .collect();
    assert_eq!(kids.len(), 2);
    assert!(kids.contains(&keys[0]["kid"].as_str().unwrap()));
    assert!(kids.contains(&r.json["kid"].as_str().unwrap()));
}

#[tokio::test]
async fn authorization_code_flow_with_pkce_issues_verified_tokens() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (uid, _) = h.verified_user("alice@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "alice@example.com", None).await;

    // First visit: the consent screen (HTML by default).
    let verifier = "verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    let ch = challenge(verifier);
    let url = authorize_url(
        "default",
        &id,
        &[("code_challenge", &ch), ("code_challenge_method", "S256")],
    );
    let page = call(&h, Method::GET, &url, &[b.header()], None).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    assert!(page.text.contains("Acme Web") && page.text.contains("Allow"));
    assert_eq!(page.headers["x-frame-options"], "DENY");
    assert!(
        page.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );

    // A consent POST without the CSRF token is refused.
    let rid = page.text.split("name=\"request_id\" value=\"").nth(1).unwrap();
    let rid = &rid[..rid.find('"').unwrap()];
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/consent",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[("request_id", rid), ("decision", "allow")]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    let p = browser_authorize(&h, "default", &b, &url).await;
    assert_eq!(p["state"], "st-123");
    assert_eq!(p["iss"], issuer("default"));
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", &p["code"]),
            ("redirect_uri", CB),
            ("code_verifier", verifier),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["cache-control"], "no-store");
    let t = r.json;
    assert_eq!(t["token_type"], "Bearer");
    assert!(t["refresh_token"].is_string());
    let access = t["access_token"].as_str().unwrap();

    // ID token: signature with the published JWKS, then every claim.
    let (header, c) = verify_with_jwks(&h, "default", t["id_token"].as_str().unwrap()).await;
    assert_eq!(header["typ"], "JWT");
    assert_eq!(c["iss"], issuer("default"));
    assert_eq!(c["aud"], id.as_str());
    assert_eq!(c["azp"], id.as_str());
    assert_eq!(c["sub"], uid.as_str());
    assert_eq!(c["nonce"], "n-456");
    assert_eq!(c["acr"], "urn:forgebase:sfa");
    assert_eq!(c["amr"], json!(["pwd"]));
    assert_eq!(c["email"], "alice@example.com");
    assert_eq!(c["email_verified"], true);
    assert!(c["auth_time"].as_i64().unwrap() <= c["iat"].as_i64().unwrap());
    assert!(c["exp"].as_i64().unwrap() > c["iat"].as_i64().unwrap());
    let at_hash = URL_SAFE_NO_PAD.encode(&Sha256::digest(access.as_bytes())[..16]);
    assert_eq!(c["at_hash"], at_hash.as_str());
    let sid = c["sid"].as_str().unwrap().to_owned();

    // Access token: an RFC 9068 JWT, verifiable the same way.
    let (ah, ac) = verify_with_jwks(&h, "default", access).await;
    assert_eq!(ah["typ"], "at+jwt");
    assert_eq!(ac["client_id"], id.as_str());
    assert_eq!(ac["sub"], uid.as_str());
    assert_eq!(ac["scope"], "email openid profile");

    // userinfo.
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/default/userinfo",
        &[("authorization", format!("Bearer {access}"))],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["sub"], uid.as_str());
    assert_eq!(r.json["email"], "alice@example.com");
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/default/userinfo",
        &[("authorization", "Bearer nope.nope.nope".into())],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Introspection.
    let i = introspect(&h, "default", &id, &secret, access).await;
    assert_eq!(i["active"], true);
    assert_eq!(i["sub"], uid.as_str());
    assert_eq!(i["client_id"], id.as_str());
    let i = introspect(&h, "default", &id, &secret, t["refresh_token"].as_str().unwrap()).await;
    assert_eq!(i["active"], true);
    assert_eq!(i["token_type"], "refresh_token");

    // Cross-app SSO: a second client of the realm signs the same session in
    // without a login; consent for the first client is remembered.
    let (id2, secret2) = confidential(&h, &admin).await;
    let t2 = code_flow(&h, &b, &id2, &secret2, &[]).await;
    let (_, c2) = verify_with_jwks(&h, "default", t2["id_token"].as_str().unwrap()).await;
    assert_eq!(c2["sid"], sid.as_str(), "one session for both apps");
    assert_eq!(c2["auth_time"], c["auth_time"]);
    let again = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(again.status, StatusCode::FOUND, "remembered consent: {}", again.text);
    assert!(again.loc_params().contains_key("code"));

    // No secret leaks into the database.
    for s in [access, t["refresh_token"].as_str().unwrap(), &secret, &p["code"]] {
        let q = format!(
            "SELECT count(*) FROM (SELECT secret_hash AS v FROM _forgebase_oauth_clients UNION ALL SELECT code_hash FROM _forgebase_oauth_codes UNION ALL SELECT token_hash FROM _forgebase_oauth_refresh_tokens) WHERE v = '{s}'"
        );
        assert_eq!(h.count(&q).await, 0);
    }
}

#[tokio::test]
async fn pkce_wrong_or_missing_verifier_is_refused() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("bob@example.com").await;
    let (id, _) = create_client(
        &h,
        &admin,
        json!({"name": "SPA", "type": "public", "redirect_uris": [CB], "skip_consent": true}),
    )
    .await;
    let b = browser_login(&h, "bob@example.com", None).await;

    // Public clients must send an S256 challenge.
    let p = browser_authorize(&h, "default", &b, &authorize_url("default", &id, &[])).await;
    assert_eq!(p["error"], "invalid_request", "{p:?}");
    assert_eq!(p["state"], "st-123");
    let ch = challenge("a-verifier-that-is-long-enough-1234567890abc");
    let p = browser_authorize(
        &h,
        "default",
        &b,
        &authorize_url(
            "default",
            &id,
            &[("code_challenge", &ch), ("code_challenge_method", "plain")],
        ),
    )
    .await;
    assert_eq!(p["error"], "invalid_request", "plain is not supported: {p:?}");

    let url = authorize_url(
        "default",
        &id,
        &[("code_challenge", &ch), ("code_challenge_method", "S256")],
    );
    let p = browser_authorize(&h, "default", &b, &url).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("code", &p["code"]),
            ("redirect_uri", CB),
            ("code_verifier", "another-verifier-that-is-long-enough-123456789"),
        ],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
    // The code is burnt by the failed attempt.
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("code", &p["code"]),
            ("redirect_uri", CB),
            ("code_verifier", "a-verifier-that-is-long-enough-1234567890abc"),
        ],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");

    // Missing verifier.
    let p = browser_authorize(&h, "default", &b, &url).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("code", &p["code"]),
            ("redirect_uri", CB),
        ],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");

    // The right verifier works; a public client never gets a secret check.
    let p = browser_authorize(&h, "default", &b, &url).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("code", &p["code"]),
            ("redirect_uri", CB),
            ("code_verifier", "a-verifier-that-is-long-enough-1234567890abc"),
        ],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("client_secret", "x"),
            ("code", "c"),
        ],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");
}

#[tokio::test]
async fn redirect_uri_mismatch_is_refused() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("carol@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "carol@example.com", None).await;

    for bad in [
        "https://evil.example.com/cb",
        "https://app.example.com/cb/",
        "https://app.example.com/cb?x=1",
        "https://APP.example.com/cb",
    ] {
        let r = call(
            &h,
            Method::GET,
            &authorize_url("default", &id, &[("redirect_uri", bad)]),
            &[b.header()],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
        assert!(r.headers.get(LOCATION).is_none(), "never redirected to {bad}");
        assert!(r.text.contains("redirect_uri"));
    }
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("redirect_uri", "")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", "fbc_unknown_client_id_000000", &[]),
        &[b.header(), ("accept", "application/json".into())],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");

    // The token request must repeat the exact redirect_uri.
    let p = browser_authorize(&h, "default", &b, &authorize_url("default", &id, &[])).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", &p["code"]),
            ("redirect_uri", "https://app.example.com/other"),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");

    // Repeated parameters are refused.
    let url = format!("{}&scope=openid", authorize_url("default", &id, &[]));
    let r = call(&h, Method::GET, &url, &[b.header()], None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn code_replay_is_refused_and_revokes_the_family() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("dan@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "dan@example.com", None).await;
    let p = browser_authorize(&h, "default", &b, &authorize_url("default", &id, &[])).await;
    let form = [
        ("grant_type", "authorization_code"),
        ("code", p["code"].as_str()),
        ("redirect_uri", CB),
    ];
    let first = token(&h, "default", &form, Some((&id, &secret))).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let access = first.json["access_token"].as_str().unwrap().to_owned();
    let refresh = first.json["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(introspect(&h, "default", &id, &secret, &access).await["active"], true);

    let replay = token(&h, "default", &form, Some((&id, &secret))).await;
    assert_oauth_error(&replay, StatusCode::BAD_REQUEST, "invalid_grant");
    assert!(replay.json["error_description"].as_str().unwrap().contains("revoked"));

    assert_eq!(introspect(&h, "default", &id, &secret, &access).await["active"], false);
    assert_eq!(introspect(&h, "default", &id, &secret, &refresh).await["active"], false);
    let r = token(
        &h,
        "default",
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oauth_families WHERE revoked_reason = 'code_replay'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_auth_audit WHERE event = 'oauth.code_replay'")
            .await,
        1
    );

    // A code bound to another client is no code at all.
    let (id2, secret2) = confidential(&h, &admin).await;
    let p = browser_authorize(&h, "default", &b, &authorize_url("default", &id, &[])).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", &p["code"]),
            ("redirect_uri", CB),
        ],
        Some((&id2, &secret2)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
}

#[tokio::test]
async fn refresh_rotation_and_reuse_revokes_the_family() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("erin@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "erin@example.com", None).await;
    let t = code_flow(&h, &b, &id, &secret, &[("scope", "openid email offline_access")]).await;
    let r1 = t["refresh_token"].as_str().unwrap().to_owned();

    let r = token(
        &h,
        "default",
        &[("grant_type", "refresh_token"), ("refresh_token", &r1)],
        Some((&id, &secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r2 = r.json["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(r1, r2, "rotated");
    let a2 = r.json["access_token"].as_str().unwrap().to_owned();
    let (_, idc) = verify_with_jwks(&h, "default", r.json["id_token"].as_str().unwrap()).await;
    assert!(idc.get("nonce").is_none(), "no nonce on refresh");

    // Narrowing is allowed, widening is not.
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &r2),
            ("scope", "openid profile"),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_scope");
    // A refused scope does not use up the token.
    assert_eq!(introspect(&h, "default", &id, &secret, &r2).await["active"], true);

    // Reusing r1 revokes the whole family.
    let r = token(
        &h,
        "default",
        &[("grant_type", "refresh_token"), ("refresh_token", &r1)],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
    assert!(r.json["error_description"].as_str().unwrap().contains("reuse"));
    assert_eq!(introspect(&h, "default", &id, &secret, &a2).await["active"], false);
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oauth_families WHERE revoked_reason = 'refresh_token_reuse'")
            .await,
        1
    );

    // A refresh token of another client is refused.
    let t = code_flow(&h, &b, &id, &secret, &[("prompt", "consent")]).await;
    let (id2, secret2) = confidential(&h, &admin).await;
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", t["refresh_token"].as_str().unwrap()),
        ],
        Some((&id2, &secret2)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
}

#[tokio::test]
async fn device_flow_with_slow_down_denial_and_expiry() {
    let clock = ManualClock::from_system();
    let c2 = clock.clone();
    let h = harness(move |cfg| cfg.clock = Arc::new(c2)).await;
    let admin = h.root_token().await;
    h.verified_user("tv@example.com").await;
    let (id, _) = create_client(
        &h,
        &admin,
        json!({
            "name": "Living Room TV",
            "type": "public",
            "grant_types": ["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"],
            "scopes": ["openid", "profile"],
        }),
    )
    .await;
    let start = |scope: &'static str| {
        let id = id.clone();
        let h = &h;
        async move {
            call(
                h,
                Method::POST,
                "/api/auth/oidc/default/device_authorization",
                &[],
                Some((
                    "application/x-www-form-urlencoded",
                    enc(&[("client_id", &id), ("scope", scope)]),
                )),
            )
            .await
        }
    };
    let poll = |dc: String| {
        let id = id.clone();
        let h = &h;
        async move {
            token(
                h,
                "default",
                &[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", &dc),
                    ("client_id", &id),
                ],
                None,
            )
            .await
        }
    };

    let r = start("openid profile").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let d = r.json;
    let dc = d["device_code"].as_str().unwrap().to_owned();
    let user_code = d["user_code"].as_str().unwrap().to_owned();
    assert_eq!(user_code.len(), 9);
    assert_eq!(d["interval"], 5);
    assert_eq!(d["verification_uri"], format!("{}/device", issuer("default")));
    assert!(d["verification_uri_complete"].as_str().unwrap().contains("user_code="));

    assert_oauth_error(
        &poll(dc.clone()).await,
        StatusCode::BAD_REQUEST,
        "authorization_pending",
    );
    assert_oauth_error(&poll(dc.clone()).await, StatusCode::BAD_REQUEST, "slow_down");
    clock.advance(Duration::from_secs(6));
    assert_oauth_error(&poll(dc.clone()).await, StatusCode::BAD_REQUEST, "slow_down");
    clock.advance(Duration::from_secs(16));
    assert_oauth_error(
        &poll(dc.clone()).await,
        StatusCode::BAD_REQUEST,
        "authorization_pending",
    );

    // The verification page: sign in first.
    let r = call(
        &h,
        Method::GET,
        &format!("/api/auth/oidc/default/device?user_code={user_code}"),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert!(r.location().contains("/auth/login?realm=default&return_to="));
    let b = browser_login(&h, "tv@example.com", None).await;
    let lower = user_code.to_lowercase();
    let r = call(
        &h,
        Method::GET,
        &format!("/api/auth/oidc/default/device?user_code={lower}"),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.text.contains("Living Room TV"));
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/default/device?user_code=BBBB-BBBB",
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "unknown code");
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/device",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[
                ("user_code", &user_code),
                ("csrf_token", &b.csrf),
                ("decision", "allow"),
            ]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.text.contains("Device connected"));

    clock.advance(Duration::from_secs(30));
    let r = poll(dc.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["access_token"].is_string());
    assert!(r.json["refresh_token"].is_string());
    let (_, c) = verify_with_jwks(&h, "default", r.json["id_token"].as_str().unwrap()).await;
    assert_eq!(c["aud"], id.as_str());
    clock.advance(Duration::from_secs(30));
    assert_oauth_error(&poll(dc).await, StatusCode::BAD_REQUEST, "invalid_grant");

    // Denied.
    let d = start("openid").await.json;
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/device",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[
                ("user_code", d["user_code"].as_str().unwrap()),
                ("csrf_token", &b.csrf),
                ("decision", "deny"),
            ]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_oauth_error(
        &poll(d["device_code"].as_str().unwrap().to_owned()).await,
        StatusCode::BAD_REQUEST,
        "access_denied",
    );

    // Expired.
    let d = start("openid").await.json;
    clock.advance(Duration::from_secs(601));
    assert_oauth_error(
        &poll(d["device_code"].as_str().unwrap().to_owned()).await,
        StatusCode::BAD_REQUEST,
        "expired_token",
    );
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/device",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[
                ("user_code", d["user_code"].as_str().unwrap()),
                ("csrf_token", &b.csrf),
                ("decision", "allow"),
            ]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "an expired code cannot be approved");

    // Scope beyond the client's allow list.
    let r = start("email").await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_scope");
}

#[tokio::test]
async fn prompt_none_without_session_is_login_required() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("fay@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;

    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none")]),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND);
    let p = r.loc_params();
    assert!(r.location().starts_with(CB));
    assert_eq!(p["error"], "login_required");
    assert_eq!(p["state"], "st-123");
    assert_eq!(p["iss"], issuer("default"));

    // Without prompt=none the browser goes to the login page and back.
    let r = call(&h, Method::GET, &authorize_url("default", &id, &[]), &[], None).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let p = r.loc_params();
    assert!(r.location().starts_with("http://127.0.0.1:8090/auth/login?"));
    assert_eq!(p["realm"], "default");
    let back = p["return_to"].clone();
    assert!(back.starts_with(&format!("{}/authorize?request_id=", issuer("default"))));
    let b = browser_login(&h, "fay@example.com", None).await;
    let path = back.trim_start_matches("http://127.0.0.1:8090");
    let p = browser_authorize(&h, "default", &b, path).await;
    assert_eq!(p["state"], "st-123", "the saved request resumes: {p:?}");
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", &p["code"]),
            ("redirect_uri", CB),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, c) = verify_with_jwks(&h, "default", r.json["id_token"].as_str().unwrap()).await;
    assert_eq!(c["nonce"], "n-456");

    // With a session but no consent for a new scope: consent_required.
    let r = call(
        &h,
        Method::GET,
        &authorize_url(
            "default",
            &id,
            &[("prompt", "none"), ("scope", "openid offline_access")],
        ),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "consent_required");
    // Remembered consent: prompt=none succeeds silently.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none")]),
        &[b.header()],
        None,
    )
    .await;
    assert!(r.loc_params().contains_key("code"), "{}", r.location());
    // prompt=none cannot be combined.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none login")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "invalid_request");
    // An unknown or stale request handle is an error page.
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/default/authorize?request_id=nope",
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn max_age_and_prompt_login_demand_a_fresh_sign_in() {
    let clock = ManualClock::from_system();
    let c2 = clock.clone();
    let h = harness(move |cfg| cfg.clock = Arc::new(c2)).await;
    let admin = h.root_token().await;
    h.verified_user("gus@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "gus@example.com", None).await;
    code_flow(&h, &b, &id, &secret, &[]).await;
    clock.advance(Duration::from_secs(120));

    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none"), ("max_age", "60")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "login_required");
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("max_age", "600")]),
        &[b.header()],
        None,
    )
    .await;
    assert!(r.loc_params().contains_key("code"), "recent enough");

    for extra in [[("max_age", "60")], [("prompt", "login")]] {
        let r = call(
            &h,
            Method::GET,
            &authorize_url("default", &id, &extra),
            &[b.header()],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::FOUND);
        let back = r.loc_params()["return_to"].clone();
        assert!(back.contains("request_id="), "{extra:?}: {}", r.location());
        // Coming back with the old session: still not fresh.
        let path = back.trim_start_matches("http://127.0.0.1:8090").to_owned();
        let r = call(&h, Method::GET, &path, &[b.header()], None).await;
        assert!(r.location().contains("/auth/login?"), "{extra:?}");
        // A new sign-in satisfies it.
        clock.advance(Duration::from_secs(1));
        let fresh = browser_login(&h, "gus@example.com", None).await;
        let p = browser_authorize(&h, "default", &fresh, &path).await;
        let r = token(
            &h,
            "default",
            &[
                ("grant_type", "authorization_code"),
                ("code", &p["code"]),
                ("redirect_uri", CB),
            ],
            Some((&id, &secret)),
        )
        .await;
        let (_, c) = verify_with_jwks(&h, "default", r.json["id_token"].as_str().unwrap()).await;
        assert!(c["auth_time"].as_i64().unwrap() >= c["iat"].as_i64().unwrap() - 1);
        clock.advance(Duration::from_secs(120));
    }
}

#[tokio::test]
async fn cross_realm_client_is_refused() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.sql("INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES ('acme', 'Acme', '{}', 0)")
        .await;
    let r = h
        .send(
            Method::PUT,
            "/api/auth/realms/acme",
            Some(&admin),
            Some(json!({"config": {}})),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    h.verified_user("hal@example.com").await;
    let (default_id, default_secret) = confidential(&h, &admin).await;
    let (acme_id, acme_secret) = create_client(
        &h,
        &admin,
        json!({"realm": "acme", "name": "Acme Portal", "redirect_uris": [CB]}),
    )
    .await;
    let acme_secret = acme_secret.unwrap();
    let b = browser_login(&h, "hal@example.com", None).await;

    // A default-realm client at the acme issuer: unknown client, no redirect.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("acme", &default_id, &[]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(r.headers.get(LOCATION).is_none());
    assert!(r.text.contains("invalid_client"));
    // ...and at the acme token endpoint.
    let r = token(
        &h,
        "acme",
        &[
            ("grant_type", "authorization_code"),
            ("code", "x"),
            ("redirect_uri", CB),
        ],
        Some((&default_id, &default_secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");
    // An acme client at the default issuer.
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "authorization_code"),
            ("code", "x"),
            ("redirect_uri", CB),
        ],
        Some((&acme_id, &acme_secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");

    // The default-realm session does not sign anyone in to acme.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("acme", &acme_id, &[("prompt", "none")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "login_required");

    // Tokens of one realm are inactive at the other.
    let t = code_flow(&h, &b, &default_id, &default_secret, &[]).await;
    let i = introspect(&h, "acme", &acme_id, &acme_secret, t["access_token"].as_str().unwrap()).await;
    assert_eq!(i["active"], false);
    let r = call(
        &h,
        Method::GET,
        "/api/auth/oidc/acme/userinfo",
        &[(
            "authorization",
            format!("Bearer {}", t["access_token"].as_str().unwrap()),
        )],
        None,
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    // Each realm signs with its own keys.
    let k1 = call(&h, Method::GET, "/api/auth/oidc/default/jwks", &[], None)
        .await
        .json;
    let k2 = call(&h, Method::GET, "/api/auth/oidc/acme/jwks", &[], None).await.json;
    assert_ne!(k1["keys"][0]["kid"], k2["keys"][0]["kid"]);
}

#[tokio::test]
async fn client_credentials_introspection_and_revocation() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (id, secret) = create_client(
        &h,
        &admin,
        json!({"name": "Billing worker", "grant_types": ["client_credentials"], "scopes": ["collections:read", "kv:write"]}),
    )
    .await;
    let secret = secret.unwrap();

    let r = token(
        &h,
        "default",
        &[("grant_type", "client_credentials")],
        Some((&id, &secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json.get("refresh_token").is_none() && r.json.get("id_token").is_none());
    assert_eq!(r.json["scope"], "collections:read kv:write");
    let at = r.json["access_token"].as_str().unwrap().to_owned();
    let (_, c) = verify_with_jwks(&h, "default", &at).await;
    assert_eq!(c["sub"], id.as_str());

    // client_secret_post works too; a narrower scope is fine.
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "client_credentials"),
            ("client_id", &id),
            ("client_secret", &secret),
            ("scope", "kv:write"),
        ],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["scope"], "kv:write");

    for (form, code) in [
        (
            vec![("grant_type", "client_credentials"), ("scope", "openid")],
            "invalid_scope",
        ),
        (
            vec![("grant_type", "client_credentials"), ("scope", "kv:read")],
            "invalid_scope",
        ),
        (vec![("grant_type", "password")], "unsupported_grant_type"),
        (
            vec![("grant_type", "authorization_code"), ("code", "x")],
            "unauthorized_client",
        ),
    ] {
        let r = token(&h, "default", &form, Some((&id, &secret))).await;
        assert_oauth_error(&r, StatusCode::BAD_REQUEST, code);
    }
    let r = token(
        &h,
        "default",
        &[("grant_type", "client_credentials")],
        Some((&id, "wrong")),
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");
    assert!(r.headers.get("www-authenticate").is_some());
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/token",
        &[basic(&id, &secret)],
        Some((
            "application/json",
            json!({"grant_type": "client_credentials"}).to_string(),
        )),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_request");

    let i = introspect(&h, "default", &id, &secret, &at).await;
    assert_eq!(i["active"], true);
    assert_eq!(i["client_id"], id.as_str());
    assert_eq!(
        introspect(&h, "default", &id, &secret, "garbage").await,
        json!({"active": false})
    );
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/introspect",
        &[],
        Some(("application/x-www-form-urlencoded", enc(&[("token", &at)]))),
    )
    .await;
    assert_oauth_error(&r, StatusCode::UNAUTHORIZED, "invalid_client");

    // Revocation (RFC 7009): always 200; the token is inactive afterwards.
    let rv = |tok: String| {
        let (id, secret) = (id.clone(), secret.clone());
        let h = &h;
        async move {
            call(
                h,
                Method::POST,
                "/api/auth/oidc/default/revoke",
                &[basic(&id, &secret)],
                Some(("application/x-www-form-urlencoded", enc(&[("token", &tok)]))),
            )
            .await
        }
    };
    assert_eq!(rv("unknown-token".into()).await.status, StatusCode::OK);
    assert_eq!(rv(at.clone()).await.status, StatusCode::OK);
    assert_eq!(introspect(&h, "default", &id, &secret, &at).await["active"], false);
}

#[tokio::test]
async fn consent_is_remembered_revocable_and_never_granted_by_impersonation() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (_, user_token) = h.verified_user("ivy@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "ivy@example.com", None).await;
    let t = code_flow(&h, &b, &id, &secret, &[]).await;

    let r = h.get("/api/auth/consents", Some(&user_token)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["client_id"], id.as_str());
    assert_eq!(items[0]["scopes"], json!(["email", "openid", "profile"]));
    let cid = items[0]["id"].as_str().unwrap().to_owned();

    // Denying goes back to the client with access_denied.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "consent")]),
        &[b.header(), ("accept", "application/json".into())],
        None,
    )
    .await;
    let rid = r.json["request_id"].as_str().unwrap().to_owned();
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/consent",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[("request_id", &rid), ("csrf_token", &b.csrf), ("decision", "deny")]),
        )),
    )
    .await;
    assert_eq!(r.loc_params()["error"], "access_denied");

    // Revoking the consent kills the tokens and brings the screen back.
    let r = h.delete(&format!("/api/auth/consents/{cid}"), Some(&user_token)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", t["refresh_token"].as_str().unwrap()),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "consent_required");

    // An impersonated session (ADR-0014) may not grant consent.
    let session_hash = {
        use forgebase_db::Backend;
        let rows =
            h.db.fetch_all(&forgebase_db::Statement::raw(
                "SELECT id FROM _forgebase_auth_sessions WHERE cookie_hash IS NOT NULL",
            ))
            .await
            .unwrap();
        rows.rows[0][0].as_str().unwrap().to_owned()
    };
    h.sql(&format!(
        "UPDATE _forgebase_auth_sessions SET impersonator = 'admin-1' WHERE id = '{session_hash}'"
    ))
    .await;
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[]),
        &[b.header(), ("accept", "application/json".into())],
        None,
    )
    .await;
    let p = r.loc_params();
    assert_eq!(p["error"], "access_denied");
    assert!(p["error_description"].contains("impersonated"));
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/consent",
        &[b.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[("request_id", &rid), ("csrf_token", &b.csrf), ("decision", "allow")]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert!(r.text.contains("impersonat"));
}

#[tokio::test]
async fn rp_initiated_logout_ends_the_sso_session() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("jo@example.com").await;
    let (id, secret) = confidential(&h, &admin).await;
    let b = browser_login(&h, "jo@example.com", None).await;
    let t = code_flow(&h, &b, &id, &secret, &[]).await;
    let offline = code_flow(
        &h,
        &b,
        &id,
        &secret,
        &[("scope", "openid offline_access"), ("prompt", "consent")],
    )
    .await;
    let id_token = t["id_token"].as_str().unwrap();

    // Without a hint the user confirms first.
    let r = call(&h, Method::GET, "/api/auth/oidc/default/logout", &[b.header()], None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text.contains("Do you want to sign out?"));
    // A post_logout_redirect_uri that is not registered is refused.
    let url = format!(
        "/api/auth/oidc/default/logout?{}",
        enc(&[
            ("id_token_hint", id_token),
            ("post_logout_redirect_uri", "https://evil.example.com/")
        ])
    );
    let r = call(&h, Method::GET, &url, &[b.header()], None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.headers.get(LOCATION).is_none());

    let url = format!(
        "/api/auth/oidc/default/logout?{}",
        enc(&[
            ("id_token_hint", id_token),
            ("post_logout_redirect_uri", LOGOUT_CB),
            ("state", "bye-1")
        ])
    );
    let r = call(&h, Method::GET, &url, &[b.header()], None).await;
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.text);
    assert!(r.location().starts_with(LOGOUT_CB));
    assert_eq!(r.loc_params()["state"], "bye-1");
    assert!(
        r.headers
            .get_all(SET_COOKIE)
            .iter()
            .any(|c| c.to_str().unwrap().starts_with("fb_session=;"))
    );

    // The session is gone for every app of the realm.
    let r = call(
        &h,
        Method::GET,
        &authorize_url("default", &id, &[("prompt", "none")]),
        &[b.header()],
        None,
    )
    .await;
    assert_eq!(r.loc_params()["error"], "login_required");
    // Session-bound tokens end; offline_access tokens survive.
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", t["refresh_token"].as_str().unwrap()),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_oauth_error(&r, StatusCode::BAD_REQUEST, "invalid_grant");
    assert_eq!(
        introspect(&h, "default", &id, &secret, t["access_token"].as_str().unwrap()).await["active"],
        false
    );
    let r = token(
        &h,
        "default",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", offline["refresh_token"].as_str().unwrap()),
        ],
        Some((&id, &secret)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // Confirmed POST logout without a hint (fresh session).
    let b2 = browser_login(&h, "jo@example.com", None).await;
    let r = call(
        &h,
        Method::POST,
        "/api/auth/oidc/default/logout",
        &[b2.header()],
        Some((
            "application/x-www-form-urlencoded",
            enc(&[("csrf_token", &b2.csrf), ("confirm", "yes")]),
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.text.contains("signed out"));
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_auth_sessions WHERE revoked_reason = 'oidc_logout'")
            .await,
        2
    );
}

#[tokio::test]
async fn client_management_and_secret_rotation() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (_, user_token) = h.verified_user("kim@example.com").await;

    let r = h
        .post(
            "/api/auth/clients",
            Some(&user_token),
            json!({"name": "x", "redirect_uris": [CB]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    for bad in [
        json!({"name": "x", "redirect_uris": ["http://app.example.com/cb"]}),
        json!({"name": "x", "redirect_uris": ["https://app.example.com/cb#frag"]}),
        json!({"name": "x", "type": "public", "grant_types": ["client_credentials"]}),
        json!({"name": "x", "redirect_uris": [CB], "scopes": ["root"]}),
        json!({"name": "", "redirect_uris": [CB]}),
        json!({"name": "x", "redirect_uris": [CB], "realm": "nowhere"}),
    ] {
        let r = h.post("/api/auth/clients", Some(&admin), bad.clone()).await;
        assert!(r.status.is_client_error(), "{bad}: {}", r.json);
    }
    let r = h
        .post(
            "/api/auth/clients",
            Some(&admin),
            json!({"name": "Native", "type": "public", "redirect_uris": ["com.example.app:/oauth"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert!(r.json.get("client_secret").is_none(), "public clients have no secret");

    let (id, s1) = confidential(&h, &admin).await;
    let r = h.get("/api/auth/clients", Some(&admin)).await;
    assert_eq!(r.json["items"].as_array().unwrap().len(), 2);
    assert!(!r.json.to_string().contains(&s1), "the secret is never listed");
    assert!(!r.json.to_string().contains("secret_hash"));

    let cc = |s: String| {
        let id = id.clone();
        let h = &h;
        async move {
            token(
                h,
                "default",
                &[("grant_type", "refresh_token"), ("refresh_token", "x")],
                Some((&id, &s)),
            )
            .await
        }
    };
    // A wrong refresh token with the right secret: invalid_grant (client authenticated).
    assert_eq!(cc(s1.clone()).await.json["error"], "invalid_grant");
    // Rotate with a grace period: both work.
    let r = h
        .post(
            &format!("/api/auth/clients/{id}/secret"),
            Some(&admin),
            json!({"grace_seconds": 3600}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let s2 = r.json["client_secret"].as_str().unwrap().to_owned();
    assert_ne!(s1, s2);
    assert_eq!(cc(s1.clone()).await.json["error"], "invalid_grant");
    assert_eq!(cc(s2.clone()).await.json["error"], "invalid_grant");
    // Rotate without grace: the previous secret stops at once.
    let r = h
        .post(&format!("/api/auth/clients/{id}/secret"), Some(&admin), json!({}))
        .await;
    let s3 = r.json["client_secret"].as_str().unwrap().to_owned();
    assert_eq!(cc(s2).await.json["error"], "invalid_client");
    assert_eq!(cc(s3.clone()).await.json["error"], "invalid_grant");

    // Disabling refuses tokens; deleting removes the client.
    let r = h
        .req(
            Method::PATCH,
            &format!("/api/auth/clients/{id}"),
            Some(&admin),
            Some(json!({"disabled": true})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["disabled"], true);
    assert_eq!(cc(s3).await.json["error"], "invalid_client");
    let r = h.delete(&format!("/api/auth/clients/{id}"), Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = h.get(&format!("/api/auth/clients/{id}"), Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        h.count("SELECT count(*) FROM _forgebase_auth_audit WHERE event LIKE 'oauth.client.%'")
            .await
            >= 5
    );
}
