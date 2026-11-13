//! Login provider presets (ADR-0039) against the in-process mock identity
//! provider: authorize URLs, token exchange, ID token / userinfo mapping,
//! `email_verified` semantics, Apple's first-login name and client secret,
//! Entra's multi-tenant issuer check, self-hosted GitLab and Keycloak.

#![allow(clippy::unwrap_used, clippy::type_complexity, missing_docs)]

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use base64::Engine as _;
use common::idp::{CLIENT_SECRET, Idp, Profile};
use common::{H, Resp, assert_error, harness};
use forgebase_auth::social::{MSA_TENANT, ProviderConfig};
use serde_json::{Value, json};

const TENANT_A: &str = "11111111-aaaa-4aaa-8aaa-111111111111";
const TENANT_B: &str = "22222222-bbbb-4bbb-8bbb-222222222222";

async fn setup() -> (H, std::sync::Arc<Idp>) {
    (harness(|_| {}).await, Idp::start().await)
}

/// Registers a provider whose endpoints point at the mock (the preset's own
/// defaults are checked separately, on the authorize URL).
fn register(h: &H, idp: &Idp, extra: Value) {
    let b = &idp.base;
    let mut v = json!({
        "client_secret": CLIENT_SECRET,
        "authorize_url": format!("{b}/authorize"), "token_url": format!("{b}/token"),
        "jwks_url": format!("{b}/certs"), "userinfo_url": format!("{b}/userinfo"),
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v["client_id"] = json!(format!("client-{}", v["id"].as_str().unwrap()));
    let cfg: ProviderConfig = serde_json::from_value(v).unwrap();
    h.auth.providers().register("default", cfg).unwrap();
}

async fn start(h: &H, p: &str) -> Value {
    let r = h.post(&format!("/api/auth/oauth/{p}/start"), None, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json
}

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

async fn sign_in(h: &H, idp: &Idp, p: &str) -> Resp {
    let s = start(h, p).await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    h.get(&format!("/api/auth/oauth/{p}/callback?code={code}&state={state}"), None)
        .await
}

fn profile(sub: &str, email: Option<&str>, verified: bool, name: &str) -> Profile {
    Profile {
        sub: sub.into(),
        email: email.map(str::to_owned),
        email_verified: verified,
        name: name.into(),
    }
}

fn email_of(r: &Resp) -> &str {
    r.json["user"]["email"].as_str().unwrap()
}

async fn identities(h: &H, r: &Resp) -> Value {
    let t = r.json["access_token"].as_str().unwrap();
    h.get("/api/auth/oauth/identities", Some(t)).await.json["identities"][0].clone()
}

// ---------------------------------------------------------------------------
// Authorize URLs of the presets (their own endpoints, scopes and parameters)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn presets_build_their_own_authorize_urls() {
    let (h, _idp) = setup().await;
    let reg = |v: Value| {
        let mut v = v;
        v["client_id"] = json!(format!("client-{}", v["id"].as_str().unwrap()));
        v["client_secret"] = json!("s");
        h.auth
            .providers()
            .register("default", serde_json::from_value::<ProviderConfig>(v).unwrap())
            .unwrap();
    };
    reg(json!({"id": "ms", "kind": "microsoft"}));
    reg(json!({"id": "msorg", "kind": "microsoft", "tenant": "organizations"}));
    reg(json!({"id": "mst", "kind": "microsoft", "tenant": TENANT_A}));
    reg(json!({"id": "gl", "kind": "gitlab"}));
    reg(json!({"id": "gls", "kind": "gitlab", "base_url": "https://git.corp.example/gitlab/"}));
    reg(json!({"id": "slack", "kind": "slack", "authorize_params": {"team": "T0123"}}));
    reg(json!({"id": "li", "kind": "linkedin"}));
    reg(json!({"id": "tw", "kind": "twitch"}));
    reg(json!({"id": "bb", "kind": "bitbucket"}));
    reg(json!({"id": "sp", "kind": "spotify"}));

    let cases: [(&str, &str, &str, bool); 10] = [
        (
            "ms",
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            "openid email profile",
            true,
        ),
        (
            "msorg",
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize",
            "openid email profile",
            true,
        ),
        (
            "mst",
            &format!("https://login.microsoftonline.com/{TENANT_A}/oauth2/v2.0/authorize"),
            "openid email profile",
            true,
        ),
        ("gl", "https://gitlab.com/oauth/authorize", "openid email profile", true),
        (
            "gls",
            "https://git.corp.example/gitlab/oauth/authorize",
            "openid email profile",
            true,
        ),
        (
            "slack",
            "https://slack.com/openid/connect/authorize",
            "openid email profile",
            true,
        ),
        (
            "li",
            "https://www.linkedin.com/oauth/v2/authorization",
            "openid profile email",
            true,
        ),
        (
            "tw",
            "https://id.twitch.tv/oauth2/authorize",
            "openid user:read:email",
            true,
        ),
        (
            "bb",
            "https://bitbucket.org/site/oauth2/authorize",
            "account email",
            false,
        ),
        ("sp", "https://accounts.spotify.com/authorize", "user-read-email", true),
    ];
    for (id, endpoint, scope, pkce) in cases {
        let s = start(&h, id).await;
        let u = s["authorization_url"].as_str().unwrap();
        assert!(u.starts_with(&format!("{endpoint}?")), "{id}: {u}");
        let q = query(u);
        assert_eq!(q["scope"], scope, "{id}");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], format!("client-{id}"));
        assert_eq!(q.contains_key("code_challenge"), pkce, "{id} pkce");
        let oidc = !matches!(id, "bb" | "sp");
        assert_eq!(q.contains_key("nonce"), oidc, "{id} nonce");
    }
    // Slack: restrict to one workspace; Twitch: the claims it must be asked for.
    let q = query(start(&h, "slack").await["authorization_url"].as_str().unwrap());
    assert_eq!(q["team"], "T0123");
    let q = query(start(&h, "tw").await["authorization_url"].as_str().unwrap());
    let claims: Value = serde_json::from_str(&q["claims"]).unwrap();
    assert!(claims["id_token"].as_object().unwrap().contains_key("email_verified"));
    assert!(claims["userinfo"].as_object().unwrap().contains_key("email"));
}

#[tokio::test]
async fn preset_configuration_is_validated() {
    let (h, _idp) = setup().await;
    let bad = |v: Value| {
        let mut v = v;
        v["id"] = json!("x");
        v["client_id"] = json!("c");
        v["client_secret"] = json!("s");
        h.auth
            .providers()
            .register("default", serde_json::from_value::<ProviderConfig>(v).unwrap())
            .unwrap_err()
            .to_string()
    };
    assert!(bad(json!({"kind": "microsoft", "tenant": "contoso.onmicrosoft.com"})).contains("tenant"));
    assert!(bad(json!({"kind": "microsoft", "allowed_tenants": ["not-a-guid"]})).contains("allowed_tenants"));
    assert!(bad(json!({"kind": "gitlab", "base_url": "ftp://gitlab.example"})).contains("base_url"));
    assert!(bad(json!({"kind": "gitlab", "base_url": "https://user:pw@gitlab.example"})).contains("base_url"));
    assert!(bad(json!({"kind": "keycloak"})).contains("keycloak"));
    assert!(bad(json!({"kind": "keycloak", "base_url": "https://sso.example"})).contains("keycloak"));
    for reserved in [
        "state",
        "redirect_uri",
        "scope",
        "code_challenge",
        "nonce",
        "response_type",
        "client_id",
    ] {
        assert!(
            bad(json!({"kind": "slack", "authorize_params": {reserved: "x"}})).contains("authorize_params"),
            "{reserved}"
        );
    }
}

// ---------------------------------------------------------------------------
// Microsoft Entra ID
// ---------------------------------------------------------------------------

/// `iss` and `tid` of a v2.0 token for `tid`, on the mock's own host.
fn entra_claims(idp: &Idp, tid: &str, extra: Value) {
    let base = idp.base.clone();
    let tid = tid.to_owned();
    idp.quirks(|q| {
        q.iss = Some(format!("{base}/{tid}/v2.0"));
        q.extra_claims.insert("tid".into(), json!(tid));
        q.extra_claims.insert("oid".into(), json!("OID-0001"));
        for (k, v) in extra.as_object().unwrap() {
            q.extra_claims.insert(k.clone(), v.clone());
        }
    });
}

#[tokio::test]
async fn entra_multi_tenant_checks_the_issuer_against_the_tokens_own_tenant() {
    let (h, idp) = setup().await;
    register(&h, &idp, json!({"id": "ms", "kind": "microsoft", "base_url": idp.base}));

    entra_claims(&idp, TENANT_A, json!({}));
    let r = sign_in(&h, &idp, "ms").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    // A token whose issuer names another tenant than its `tid` is refused.
    idp.reset_quirks();
    let base = idp.base.clone();
    idp.quirks(|q| {
        q.iss = Some(format!("{base}/{TENANT_B}/v2.0"));
        q.extra_claims.insert("tid".into(), json!(TENANT_A));
        q.extra_claims.insert("oid".into(), json!("OID-0001"));
    });
    assert_error(
        &sign_in(&h, &idp, "ms").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );

    // No `tid`, a malformed `tid`, or the literal template are refused.
    for tid in [None, Some("../x"), Some("{tenantid}")] {
        idp.reset_quirks();
        let base = idp.base.clone();
        idp.quirks(|q| {
            q.iss = Some(format!("{base}/{{tenantid}}/v2.0"));
            if let Some(t) = tid {
                q.extra_claims.insert("tid".into(), json!(t));
            }
        });
        assert_error(
            &sign_in(&h, &idp, "ms").await,
            StatusCode::UNAUTHORIZED,
            "invalid_token",
        );
    }
    // The v1.0 `sts.windows.net` issuer is not v2.0's.
    idp.reset_quirks();
    idp.quirks(|q| {
        q.iss = Some(format!("https://sts.windows.net/{TENANT_A}/"));
        q.extra_claims.insert("tid".into(), json!(TENANT_A));
    });
    assert_error(
        &sign_in(&h, &idp, "ms").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );
}

#[tokio::test]
async fn entra_organizations_consumers_and_tenant_allow_list() {
    let (h, idp) = setup().await;
    register(
        &h,
        &idp,
        json!({"id": "org", "kind": "microsoft", "tenant": "organizations", "base_url": idp.base}),
    );
    register(
        &h,
        &idp,
        json!({"id": "con", "kind": "microsoft", "tenant": "consumers", "base_url": idp.base}),
    );
    register(
        &h,
        &idp,
        json!({"id": "only-a", "kind": "microsoft", "base_url": idp.base, "allowed_tenants": [TENANT_A.to_uppercase()]}),
    );
    register(
        &h,
        &idp,
        json!({"id": "pin-a", "kind": "microsoft", "tenant": TENANT_A, "base_url": idp.base}),
    );

    // Personal accounts (the MSA tenant) are not organizations.
    entra_claims(&idp, MSA_TENANT, json!({}));
    assert_error(
        &sign_in(&h, &idp, "org").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );
    assert_eq!(sign_in(&h, &idp, "con").await.status, StatusCode::OK);
    // ... and a work tenant is not a consumer.
    idp.reset_quirks();
    entra_claims(&idp, TENANT_A, json!({}));
    assert_eq!(sign_in(&h, &idp, "org").await.status, StatusCode::OK);
    assert_error(
        &sign_in(&h, &idp, "con").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );

    // Allow list (case-insensitive): tenant A in, tenant B out.
    assert_eq!(sign_in(&h, &idp, "only-a").await.status, StatusCode::OK);
    idp.reset_quirks();
    entra_claims(&idp, TENANT_B, json!({}));
    assert_error(
        &sign_in(&h, &idp, "only-a").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );

    // A single-tenant provider pins the issuer: tenant B's valid token fails.
    assert_error(
        &sign_in(&h, &idp, "pin-a").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );
    idp.reset_quirks();
    entra_claims(&idp, TENANT_A, json!({}));
    assert_eq!(sign_in(&h, &idp, "pin-a").await.status, StatusCode::OK);
}

#[tokio::test]
async fn entra_email_is_unverified_unless_the_domain_is_owner_verified() {
    let (h, idp) = setup().await;
    register(&h, &idp, json!({"id": "ms", "kind": "microsoft", "base_url": idp.base}));
    let (local_id, _) = h.verified_user("pat@example.com").await;

    // The mock's ID token says email_verified: true. Entra never does, and
    // even if a token said so it is not proof: no linking, no real email.
    idp.set_profile(profile("sub-x", Some("pat@example.com"), true, "Pat"));
    entra_claims(&idp, TENANT_A, json!({}));
    let r = sign_in(&h, &idp, "ms").await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");

    // A new person with an address nobody proved: placeholder, unverified.
    idp.set_profile(profile("sub-y", Some("ceo@victim.example"), true, "Mallory"));
    entra_claims(&idp, TENANT_A, json!({"oid": "OID-MALLORY"}));
    let r = sign_in(&h, &idp, "ms").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(email_of(&r).ends_with("@oauth.invalid"), "{}", r.json);
    assert_eq!(r.json["user"]["verified"], false);

    // `xms_edov` (the tenant owns the email domain) is trusted: the real
    // address, and linking to the verified local account.
    entra_claims(&idp, TENANT_A, json!({"xms_edov": true, "oid": "OID-PAT"}));
    idp.set_profile(profile("sub-z", Some("pat@example.com"), false, "Pat"));
    let r = sign_in(&h, &idp, "ms").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["id"], local_id);
    // The identity is tenant + object id, whatever `sub` or the email become.
    idp.set_profile(profile("another-sub", Some("renamed@example.com"), false, "Pat"));
    let r2 = sign_in(&h, &idp, "ms").await;
    assert_eq!(r2.json["user"]["id"], local_id);
    // The same person's oid in a different tenant is a different identity.
    entra_claims(&idp, TENANT_B, json!({"xms_edov": true, "oid": "OID-PAT"}));
    let r3 = sign_in(&h, &idp, "ms").await;
    assert_ne!(r3.json["user"]["id"], local_id);
}

// ---------------------------------------------------------------------------
// GitLab (self-hosted), Keycloak
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gitlab_derives_every_endpoint_from_the_self_hosted_base_url() {
    let (h, idp) = setup().await;
    // No endpoint overrides: /oauth/token, /oauth/discovery/keys and
    // /oauth/userinfo are the mock's, reached through `base_url` alone.
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "gl", "kind": "gitlab", "client_id": "client-gl", "client_secret": CLIENT_SECRET,
        "base_url": format!("{}/", idp.base),
    }))
    .unwrap();
    h.auth.providers().register("default", cfg).unwrap();
    let s = start(&h, "gl").await;
    assert!(
        s["authorization_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}/oauth/authorize?", idp.base))
    );

    // Verified by GitLab (confirmed address): the real email.
    idp.set_profile(profile("gl-1", Some("dev@corp.example"), true, "Dev"));
    idp.quirks(|q| {
        q.extra_claims
            .insert("picture".into(), json!("https://gitlab.example/uploads/a.png"));
    });
    let r = sign_in(&h, &idp, "gl").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(email_of(&r), "dev@corp.example");
    assert_eq!(r.json["user"]["verified"], true);
    let id = identities(&h, &r).await;
    assert_eq!(id["avatar_url"], "https://gitlab.example/uploads/a.png");

    // An unconfirmed address is not trusted.
    idp.set_profile(profile("gl-2", Some("new@corp.example"), false, "New"));
    idp.reset_quirks();
    let r = sign_in(&h, &idp, "gl").await;
    assert!(email_of(&r).ends_with("@oauth.invalid"));

    // A token from another instance (issuer) is refused.
    idp.quirks(|q| q.iss = Some("https://gitlab.com".into()));
    assert_error(
        &sign_in(&h, &idp, "gl").await,
        StatusCode::UNAUTHORIZED,
        "invalid_token",
    );
}

#[tokio::test]
async fn keycloak_uses_discovery_of_its_realm() {
    let (h, idp) = setup().await;
    let realm_issuer = format!("{}/realms/acme", idp.base);
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "kc", "kind": "keycloak", "client_id": "client-kc", "client_secret": CLIENT_SECRET,
        "base_url": idp.base, "tenant": "acme",
    }))
    .unwrap();
    h.auth.providers().register("default", cfg).unwrap();
    idp.quirks(|q| {
        q.iss = Some(realm_issuer.clone());
        q.discovery_issuer = Some(realm_issuer.clone());
    });
    let r = sign_in(&h, &idp, "kc").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(email_of(&r), "pat@example.com");

    // Discovery that names another issuer is refused (mix-up).
    let (h2, idp2) = setup().await;
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "kc", "kind": "keycloak", "client_id": "client-kc", "client_secret": CLIENT_SECRET,
        "base_url": idp2.base, "tenant": "acme",
    }))
    .unwrap();
    h2.auth.providers().register("default", cfg).unwrap();
    idp2.quirks(|q| q.discovery_issuer = Some("https://evil.example/realms/acme".into()));
    let s = h2.post("/api/auth/oauth/kc/start", None, json!({})).await;
    assert_error(&s, StatusCode::BAD_GATEWAY, "provider_error");
}

// ---------------------------------------------------------------------------
// Slack, LinkedIn, Twitch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slack_linkedin_and_twitch_map_id_token_claims() {
    let (h, idp) = setup().await;
    for (id, kind) in [("slack", "slack"), ("li", "linkedin"), ("tw", "twitch")] {
        register(&h, &idp, json!({"id": id, "kind": kind, "issuer": idp.base}));
    }
    // Verified email from the ID token, picture kept when https.
    idp.set_profile(profile("slack-U1", Some("s@team.example"), true, "Sam"));
    idp.quirks(|q| {
        q.extra_claims
            .insert("picture".into(), json!("https://avatars.slack-edge.com/a.png"));
    });
    let r = sign_in(&h, &idp, "slack").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(email_of(&r), "s@team.example");
    let id = identities(&h, &r).await;
    assert_eq!(id["display_name"], "Sam");
    assert_eq!(id["avatar_url"], "https://avatars.slack-edge.com/a.png");

    // An http:// or javascript: picture is dropped, not stored.
    for pic in ["http://insecure.example/a.png", "javascript:alert(1)"] {
        idp.set_profile(profile("li-1", Some("l@work.example"), true, "Lee"));
        idp.quirks(|q| {
            q.extra_claims.insert("picture".into(), json!(pic));
        });
        let r = sign_in(&h, &idp, "li").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert!(identities(&h, &r).await["avatar_url"].is_null(), "{pic}");
    }

    // LinkedIn: email_verified false stays false.
    idp.set_profile(profile("li-2", Some("unv@work.example"), false, "Un"));
    idp.reset_quirks();
    let r = sign_in(&h, &idp, "li").await;
    assert!(email_of(&r).ends_with("@oauth.invalid"));

    // Twitch: the display name is `preferred_username`; no `name` claim.
    idp.set_profile(profile("tw-1", Some("t@stream.example"), true, ""));
    idp.quirks(|q| {
        q.extra_claims.insert("name".into(), Value::Null);
        q.extra_claims.insert("preferred_username".into(), json!("streamer42"));
    });
    let r = sign_in(&h, &idp, "tw").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(identities(&h, &r).await["display_name"], "streamer42");
}

#[tokio::test]
async fn non_boolean_email_verified_never_verifies() {
    let (h, idp) = setup().await;
    register(&h, &idp, json!({"id": "li", "kind": "linkedin", "issuer": idp.base}));
    let (_, _) = h.verified_user("pat@example.com").await;
    // "false" as a string, and a number, never verify (only `true`/"true").
    for v in [json!("false"), json!(0), json!(null), json!("yes")] {
        idp.set_profile(profile("li-x", Some("pat@example.com"), true, "P"));
        idp.quirks(|q| {
            q.extra_claims.insert("email_verified".into(), v.clone());
        });
        let r = sign_in(&h, &idp, "li").await;
        assert_error(&r, StatusCode::CONFLICT, "conflict");
    }
}

// ---------------------------------------------------------------------------
// Bitbucket and Spotify (OAuth 2 with HTTP Basic client authentication)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bitbucket_basic_client_auth_and_confirmed_primary_email_only() {
    let (h, idp) = setup().await;
    let b = &idp.base;
    register(
        &h,
        &idp,
        json!({"id": "bb", "kind": "bitbucket", "userinfo_url": format!("{b}/bb/user"), "emails_url": format!("{b}/bb/emails")}),
    );
    idp.quirks(|q| q.basic_client_auth = true);
    idp.set_profile(profile("x", Some("bb@example.com"), true, "Bea"));
    let r = sign_in(&h, &idp, "bb").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // The primary, confirmed address (not the confirmed secondary one).
    assert_eq!(email_of(&r), "bb@example.com");
    assert_eq!(r.json["user"]["verified"], true);
    let (auth, secret_in_form) = idp.last_token_auth();
    let expect = base64::engine::general_purpose::STANDARD.encode(format!("client-bb:{CLIENT_SECRET}"));
    assert_eq!(auth.as_deref(), Some(format!("Basic {expect}").as_str()));
    assert!(!secret_in_form, "the secret is not also sent in the form");
    let id = identities(&h, &r).await;
    assert_eq!(id["display_name"], "Bea");
    assert_eq!(id["avatar_url"], "https://secure.gravatar.com/avatar/bb.png");

    // Unconfirmed primary: unverified, a placeholder account (a fresh
    // provider account: the mock's Bitbucket uuid is fixed).
    let (h, idp) = setup().await;
    let b = &idp.base;
    register(
        &h,
        &idp,
        json!({"id": "bb", "kind": "bitbucket", "userinfo_url": format!("{b}/bb/user"), "emails_url": format!("{b}/bb/emails")}),
    );
    idp.quirks(|q| q.basic_client_auth = true);
    idp.set_profile(profile("x", Some("raw@example.com"), false, "Raw"));
    let r = sign_in(&h, &idp, "bb").await;
    assert!(email_of(&r).ends_with("@oauth.invalid"));
    assert_eq!(r.json["user"]["verified"], false);
}

#[tokio::test]
async fn spotify_email_is_never_verified() {
    let (h, idp) = setup().await;
    register(
        &h,
        &idp,
        json!({"id": "sp", "kind": "spotify", "userinfo_url": format!("{}/spotify/me", idp.base)}),
    );
    idp.quirks(|q| q.basic_client_auth = true);
    let (_, _) = h.verified_user("pat@example.com").await;
    idp.set_profile(profile("x", Some("pat@example.com"), true, "Pat"));
    // Spotify's `email` is unconfirmed: it cannot take over the local account.
    let r = sign_in(&h, &idp, "sp").await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
    idp.set_profile(profile("x", Some("fresh@example.com"), true, "Fresh"));
    let r = sign_in(&h, &idp, "sp").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(email_of(&r).ends_with("@oauth.invalid"));
    let id = identities(&h, &r).await;
    assert_eq!(id["avatar_url"], "https://i.scdn.co/image/abc");
    assert!(idp.last_token_auth().0.unwrap().starts_with("Basic "));
}

#[tokio::test]
async fn discord_avatar_is_the_cdn_url() {
    let (h, idp) = setup().await;
    register(
        &h,
        &idp,
        json!({"id": "dc", "kind": "discord", "userinfo_url": format!("{}/discord/me", idp.base)}),
    );
    idp.set_profile(profile("x", Some("dc@example.com"), true, "D"));
    let r = sign_in(&h, &idp, "dc").await;
    assert_eq!(
        identities(&h, &r).await["avatar_url"],
        "https://cdn.discordapp.com/avatars/9001/abc123.png"
    );
}

// ---------------------------------------------------------------------------
// Apple
// ---------------------------------------------------------------------------

fn apple_key() -> (p256::ecdsa::SigningKey, String) {
    use p256::pkcs8::EncodePrivateKey;
    let key = p256::ecdsa::SigningKey::from_slice(&[5u8; 32]).unwrap();
    let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
    (key, pem)
}

async fn apple_callback(h: &H, idp: &Idp, user: Option<&str>) -> Resp {
    let s = start(h, "apple").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    body.append_pair("code", &code).append_pair("state", &state);
    if let Some(u) = user {
        body.append_pair("user", u);
    }
    h.req_raw(
        axum::http::Method::POST,
        "/api/auth/oauth/apple/callback",
        None,
        Some(("application/x-www-form-urlencoded", body.finish())),
    )
    .await
}

#[tokio::test]
async fn apple_name_comes_from_the_first_form_post_only_and_relay_email_is_verified() {
    let (h, idp) = setup().await;
    let (key, pem) = apple_key();
    idp.set_apple_verifier(*key.verifying_key());
    let b = &idp.base;
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "apple", "kind": "apple", "client_id": "com.example.web",
        "authorize_url": format!("{b}/authorize"), "token_url": format!("{b}/token"),
        "jwks_url": format!("{b}/certs"), "issuer": b,
        "apple": {"team_id": "TEAM123456", "key_id": "KEY1", "private_key_pem": pem},
    }))
    .unwrap();
    h.auth.providers().register("default", cfg).unwrap();

    // Apple's `email_verified` is the string "true" and the address a relay.
    idp.set_profile(profile(
        "apple-sub-1",
        Some("abc123@privaterelay.appleid.com"),
        true,
        "",
    ));
    idp.quirks(|q| {
        q.extra_claims.insert("name".into(), Value::Null);
        q.extra_claims.insert("email_verified".into(), json!("true"));
        q.extra_claims.insert("is_private_email".into(), json!("true"));
    });
    let user =
        json!({"name": {"firstName": "Ada", "lastName": "Lovelace"}, "email": "forged@evil.example"}).to_string();
    let r = apple_callback(&h, &idp, Some(&user)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // The relay address is a verified mailbox; the form's `email` is ignored.
    assert_eq!(email_of(&r), "abc123@privaterelay.appleid.com");
    assert_eq!(r.json["user"]["verified"], true);
    assert_eq!(identities(&h, &r).await["display_name"], "Ada Lovelace");

    // Later sign-ins carry no `user`: the stored name stays.
    let r2 = apple_callback(&h, &idp, None).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.json);
    assert_eq!(r2.json["user"]["id"], r.json["user"]["id"]);
    assert_eq!(identities(&h, &r2).await["display_name"], "Ada Lovelace");

    // A hostile `user` field is only ever a display name: control characters
    // dropped, length capped, garbage ignored.
    idp.set_profile(profile("apple-sub-2", Some("def@privaterelay.appleid.com"), true, ""));
    let long = "A".repeat(500);
    let user = json!({"name": {"firstName": format!("Bob\u{0}\n{long}"), "lastName": ""}}).to_string();
    let r3 = apple_callback(&h, &idp, Some(&user)).await;
    let name = identities(&h, &r3).await["display_name"].as_str().unwrap().to_owned();
    assert!(
        name.starts_with("Bob") && name.chars().count() <= 100 && !name.contains(['\n', '\0']),
        "{name}"
    );
    idp.set_profile(profile("apple-sub-3", Some("ghi@privaterelay.appleid.com"), true, ""));
    let r4 = apple_callback(&h, &idp, Some("not json {")).await;
    assert_eq!(r4.status, StatusCode::OK, "{}", r4.json);
    assert!(identities(&h, &r4).await["display_name"].is_null());
}

#[tokio::test]
async fn apple_client_secret_is_a_valid_short_lived_es256_jwt() {
    let (h, idp) = setup().await;
    let (key, pem) = apple_key();
    idp.set_apple_verifier(*key.verifying_key());
    let b = &idp.base;
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "apple", "kind": "apple", "client_id": "com.example.web",
        "authorize_url": format!("{b}/authorize"), "token_url": format!("{b}/token"),
        "jwks_url": format!("{b}/certs"), "issuer": b,
        "apple": {"team_id": "TEAM123456", "key_id": "KEY1", "private_key_pem": pem},
    }))
    .unwrap();
    h.auth.providers().register("default", cfg).unwrap();
    idp.quirks(|q| {
        q.extra_claims.insert("email_verified".into(), json!("true"));
    });
    let r = apple_callback(&h, &idp, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    let secret = idp.last_client_secret();
    let parts: Vec<&str> = secret.split('.').collect();
    assert_eq!(parts.len(), 3);
    let dec = |s: &str| -> Value {
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap()).unwrap()
    };
    let (head, claims) = (dec(parts[0]), dec(parts[1]));
    assert_eq!(
        (head["alg"].as_str(), head["kid"].as_str()),
        (Some("ES256"), Some("KEY1"))
    );
    assert_eq!(claims["iss"], "TEAM123456");
    assert_eq!(claims["sub"], "com.example.web");
    assert_eq!(claims["aud"], "https://appleid.apple.com");
    let (iat, exp) = (claims["iat"].as_i64().unwrap(), claims["exp"].as_i64().unwrap());
    // Apple allows up to six months; a request-scoped secret lives minutes.
    assert!(exp > iat && exp - iat <= 600, "{claims}");
    // And it verifies against the team key (the mock checked it too).
    use p256::ecdsa::signature::Verifier;
    let sig = p256::ecdsa::Signature::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap(),
    )
    .unwrap();
    key.verifying_key()
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
        .unwrap();
}
