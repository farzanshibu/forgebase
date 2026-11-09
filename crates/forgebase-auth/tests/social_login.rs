//! Social login against an in-process mock identity provider on a local port.

#![allow(clippy::unwrap_used, clippy::type_complexity, missing_docs)]

mod common;

use axum::http::StatusCode;
use common::idp::{CLIENT_SECRET, Idp, Profile, default_profile};
use common::{H, Resp, assert_error, harness};
use forgebase_auth::social::{ProviderConfig, ProviderKind};
use serde_json::{Value, json};

fn provider(idp: &Idp, id: &str, kind: &str) -> ProviderConfig {
    let b = &idp.base;
    let mut v = json!({
        "id": id, "kind": kind, "client_id": format!("client-{id}"), "client_secret": CLIENT_SECRET,
        "authorize_url": format!("{b}/authorize"), "token_url": format!("{b}/token"),
        "jwks_url": format!("{b}/certs"), "userinfo_url": format!("{b}/userinfo"),
        "issuer": b,
    });
    let o = v.as_object_mut().unwrap();
    match kind {
        "github" => {
            o.insert("userinfo_url".into(), json!(format!("{b}/gh/user")));
            o.insert("emails_url".into(), json!(format!("{b}/gh/emails")));
        }
        "discord" => {
            o.insert("userinfo_url".into(), json!(format!("{b}/discord/me")));
        }
        "facebook" => {
            o.insert("userinfo_url".into(), json!(format!("{b}/fb/me")));
        }
        "wechat" => {
            o.insert("token_url".into(), json!(format!("{b}/wx/token")));
            o.insert("userinfo_url".into(), json!(format!("{b}/wx/userinfo")));
        }
        _ => {}
    }
    serde_json::from_value(v).unwrap()
}

async fn setup(kinds: &[(&str, &str)]) -> (H, std::sync::Arc<Idp>) {
    let h = harness(|_| {}).await;
    let idp = Idp::start().await;
    for (id, kind) in kinds {
        h.auth
            .providers()
            .register("default", provider(&idp, id, kind))
            .unwrap();
    }
    (h, idp)
}

async fn start(h: &H, p: &str) -> Value {
    let r = h.post(&format!("/api/auth/oauth/{p}/start"), None, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json
}

async fn callback(h: &H, p: &str, code: &str, state: &str) -> Resp {
    h.get(&format!("/api/auth/oauth/{p}/callback?code={code}&state={state}"), None)
        .await
}

/// Full round trip: start → (user approves at the IdP) → callback.
async fn sign_in(h: &H, idp: &Idp, p: &str) -> Resp {
    let s = start(h, p).await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    callback(h, p, &code, &state).await
}

fn profile(sub: &str, email: Option<&str>, verified: bool) -> Profile {
    Profile {
        sub: sub.into(),
        email: email.map(str::to_owned),
        email_verified: verified,
        name: "Pat".into(),
    }
}

async fn user_email(h: &H, token: &str) -> String {
    let me = h.get("/api/auth/me", Some(token)).await;
    me.json["user"]["email"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn oidc_signup_login_with_pkce_state_nonce() {
    let (h, idp) = setup(&[("google", "google")]).await;
    // Google defaults to its real endpoints; point it at the mock.
    h.auth
        .providers()
        .register("default", provider(&idp, "google", "google"))
        .unwrap();
    let s = start(&h, "google").await;
    let url = url::Url::parse(s["authorization_url"].as_str().unwrap()).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["scope"], "openid email profile");
    assert_eq!(q["client_id"], "client-google");
    assert!(q["redirect_uri"].ends_with("/api/auth/oauth/google/callback"));
    assert!(q["state"].len() >= 40 && q["nonce"].len() >= 20 && q["code_challenge"].len() >= 40);

    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let r = callback(&h, "google", &code, &state).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["email"], "pat@example.com");
    assert_eq!(r.json["user"]["verified"], true);
    let token = r.json["access_token"].as_str().unwrap();
    assert_eq!(h.get("/api/auth/me", Some(token)).await.json["subject"]["type"], "user");

    // Second sign-in: same user, no second account.
    let r2 = sign_in(&h, &idp, "google").await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.json);
    assert_eq!(r2.json["user"]["id"], r.json["user"]["id"]);
    let ids = h.get("/api/auth/oauth/identities", Some(token)).await;
    assert_eq!(ids.json["identities"].as_array().unwrap().len(), 1);

    // The session shows up in the history as an oauth login.
    let hist = h.get("/api/auth/login-history", Some(token)).await;
    assert_eq!(hist.json["items"][0]["detail"]["method"], "oauth", "{}", hist.json);
}

#[tokio::test]
async fn bad_replayed_and_foreign_state_are_rejected() {
    let (h, idp) = setup(&[("a", "oidc"), ("b", "oidc")]).await;
    // Unknown state, missing state.
    let r = callback(&h, "a", "whatever", "not-a-real-state").await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    let r = h.get("/api/auth/oauth/a/callback?code=x", None).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Replayed callback: the state is single use.
    let s = start(&h, "a").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    assert_eq!(callback(&h, "a", &code, &state).await.status, StatusCode::OK);
    let r = callback(&h, "a", &code, &state).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // A state is bound to its provider.
    let s = start(&h, "a").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let r = callback(&h, "b", &code, &state).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    // ... and it is burnt even by the failed attempt.
    let r = callback(&h, "a", &code, &state).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Expired state.
    let s = start(&h, "a").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    sqlx_expire_states(&h).await;
    let r = callback(&h, "a", &code, &state).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
}

async fn sqlx_expire_states(h: &H) {
    use forgebase_auth::data::db::{Backend, Statement, Value};
    use std::sync::Arc;
    let db: Arc<dyn Backend> = Arc::new(h.db.clone());
    let tx = db.begin(forgebase_auth::data::db::TxMode::Normal).await.unwrap();
    let mut tx = tx;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_oauth_states SET expires_at = 1",
        Vec::<Value>::new(),
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn replayed_authorization_code_is_refused_by_the_provider() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let s1 = start(&h, "a").await;
    let (code, state1) = idp.authorize(s1["authorization_url"].as_str().unwrap());
    assert_eq!(callback(&h, "a", &code, &state1).await.status, StatusCode::OK);
    // An attacker replays the stolen code with a fresh state of their own.
    let s2 = start(&h, "a").await;
    let (_, state2) = idp.authorize(s2["authorization_url"].as_str().unwrap());
    let r = callback(&h, "a", &code, &state2).await;
    assert_error(&r, StatusCode::BAD_GATEWAY, "provider_error");
    assert_eq!(idp.redeemed(), 1);
}

#[tokio::test]
async fn id_token_checks() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    type Tweak = Box<dyn Fn(&mut common::idp::Quirks)>;
    let cases: Vec<(&str, Tweak)> = vec![
        ("wrong audience", Box::new(|q| q.aud = Some("someone-else".into()))),
        ("expired", Box::new(|q| q.exp_delta = Some(-3600))),
        (
            "wrong issuer",
            Box::new(|q| q.iss = Some("https://evil.example".into())),
        ),
        ("wrong nonce", Box::new(|q| q.nonce = Some("attacker-nonce".into()))),
        ("alg none", Box::new(|q| q.alg = Some("none".into()))),
        ("alg HS256", Box::new(|q| q.alg = Some("HS256".into()))),
        ("tampered signature", Box::new(|q| q.tamper = true)),
        ("unknown kid", Box::new(|q| q.kid = Some("nope".into()))),
        ("rsa key, ec alg", Box::new(|q| q.kid = Some("rsa1".into()))),
    ];
    for (name, tweak) in cases {
        idp.reset_quirks();
        idp.quirks(|q| tweak(q));
        let r = sign_in(&h, &idp, "a").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{name}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "invalid_token", "{name}");
    }
    // No ID token at all from an OIDC provider.
    idp.reset_quirks();
    idp.quirks(|q| q.no_id_token = true);
    let r = sign_in(&h, &idp, "a").await;
    assert_error(&r, StatusCode::BAD_GATEWAY, "provider_error");
    // None of those created an account.
    idp.reset_quirks();
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn rs256_tokens_and_jwks_caching() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    idp.quirks(|q| q.rsa = true);
    for _ in 0..3 {
        let r = sign_in(&h, &idp, "a").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    }
    assert_eq!(
        idp.jwks_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "JWKS cached"
    );
    // A flood of unknown `kid`s does not refetch more than once per 30 s.
    idp.quirks(|q| q.kid = Some("rotated-key".into()));
    for _ in 0..3 {
        let r = sign_in(&h, &idp, "a").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    }
    assert!(idp.jwks_hits.load(std::sync::atomic::Ordering::SeqCst) <= 2);
}

#[tokio::test]
async fn provider_error_and_denied_consent() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let s = start(&h, "a").await;
    let r = h
        .get(
            &format!(
                "/api/auth/oauth/a/callback?error=access_denied&state={}",
                s["state"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    // The state is burnt.
    let (code, _) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let r = callback(&h, "a", &code, s["state"].as_str().unwrap()).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
    // Unknown provider.
    let r = h.post("/api/auth/oauth/nope/start", None, json!({})).await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn email_matching_rules() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let (local_id, _) = h.verified_user("pat@example.com").await;

    // Provider email NOT verified: never link.
    idp.set_profile(profile("sub-1", Some("pat@example.com"), false));
    let r = sign_in(&h, &idp, "a").await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");

    // Verified by the provider and by us: linked to the existing user.
    idp.set_profile(profile("sub-1", Some("PAT@example.com"), true));
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["id"], local_id);

    // Provider account is now bound by subject, whatever email it reports.
    idp.set_profile(profile("sub-1", Some("changed@example.com"), true));
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.json["user"]["id"], local_id);

    // Local account NOT verified: a provider cannot take it over.
    let victim = h.unverified_user("victim@example.com").await;
    idp.set_profile(profile("sub-2", Some("victim@example.com"), true));
    let r = sign_in(&h, &idp, "a").await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
    let _ = victim;

    // Auto-link switched off: even verified + verified is refused.
    let mut cfg = provider(&idp, "strict", "oidc");
    cfg.auto_link_verified_email = false;
    h.auth.providers().register("default", cfg).unwrap();
    idp.set_profile(profile("sub-3", Some("pat@example.com"), true));
    let r = sign_in(&h, &idp, "strict").await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");

    // An unverified provider email never becomes an account email: no squatting.
    idp.set_profile(profile("sub-4", Some("nobody@example.com"), false));
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let t = r.json["access_token"].as_str().unwrap();
    assert!(user_email(&h, t).await.ends_with("@oauth.invalid"));
    assert_eq!(r.json["user"]["verified"], false);
}

#[tokio::test]
async fn sign_up_can_be_disabled() {
    let (h, idp) = setup(&[]).await;
    let mut cfg = provider(&idp, "closed", "oidc");
    cfg.allow_signup = false;
    h.auth.providers().register("default", cfg).unwrap();
    let r = sign_in(&h, &idp, "closed").await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn linking_and_unlinking() {
    let (h, idp) = setup(&[("gh", "github"), ("a", "oidc")]).await;
    let (alice_id, alice) = h.verified_user("alice@example.com").await;
    let (_, bob) = h.verified_user("bob@example.com").await;

    // Linking needs a signed-in user.
    let r = h.post("/api/auth/oauth/gh/link", None, json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);

    idp.set_profile(profile("ignored", Some("someone-else@example.com"), true));
    let s = h.post("/api/auth/oauth/gh/link", Some(&alice), json!({})).await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.json);
    let (code, state) = idp.authorize(s.json["authorization_url"].as_str().unwrap());
    let r = callback(&h, "gh", &code, &state).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["linked"], true);
    let identity_id = r.json["identity"]["id"].as_str().unwrap().to_owned();
    assert_eq!(r.json["identity"]["provider"], "gh");

    // Signing in with it now reaches Alice.
    let r = sign_in(&h, &idp, "gh").await;
    assert_eq!(r.json["user"]["id"], alice_id);

    // Bob cannot link the same GitHub account.
    let s = h.post("/api/auth/oauth/gh/link", Some(&bob), json!({})).await;
    let (code, state) = idp.authorize(s.json["authorization_url"].as_str().unwrap());
    let r = callback(&h, "gh", &code, &state).await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");

    // Another user cannot unlink Alice's identity.
    let r = h
        .delete(&format!("/api/auth/oauth/identities/{identity_id}"), Some(&bob))
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
    let list = h.get("/api/auth/oauth/identities", Some(&alice)).await;
    assert_eq!(list.json["identities"].as_array().unwrap().len(), 1);

    // Alice unlinks; the provider no longer reaches her account.
    let r = h
        .delete(&format!("/api/auth/oauth/identities/{identity_id}"), Some(&alice))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let list = h.get("/api/auth/oauth/identities", Some(&alice)).await;
    assert!(list.json["identities"].as_array().unwrap().is_empty());
    let r = sign_in(&h, &idp, "gh").await;
    assert_eq!(r.status, StatusCode::OK, "creates a fresh account");
    assert_ne!(r.json["user"]["id"], alice_id);
}

#[tokio::test]
async fn cannot_unlink_the_only_way_in() {
    let (h, idp) = setup(&[("wx", "wechat")]).await;
    idp.set_profile(profile("union:u-1", None, false));
    let r = sign_in(&h, &idp, "wx").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let token = r.json["access_token"].as_str().unwrap().to_owned();
    let ids = h.get("/api/auth/oauth/identities", Some(&token)).await;
    let id = ids.json["identities"][0]["id"].as_str().unwrap().to_owned();
    let r = h
        .delete(&format!("/api/auth/oauth/identities/{id}"), Some(&token))
        .await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
}

#[tokio::test]
async fn github_uses_only_primary_verified_emails() {
    let (h, idp) = setup(&[("gh", "github")]).await;
    let (local, _) = h.verified_user("octo@example.com").await;
    // The profile's own email field says octo@example.com, but only the
    // /user/emails list carries a verification flag.
    idp.set_profile(profile("x", Some("octo@example.com"), false));
    let r = sign_in(&h, &idp, "gh").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_ne!(r.json["user"]["id"], local, "unverified GitHub email must not link");
    assert!(r.json["user"]["email"].as_str().unwrap().starts_with("gh."));
    idp.set_profile(profile("x", Some("octo@example.com"), true));
    // Same GitHub id 4242 → same identity as before; use a second provider id for the verified path.
    let (h2, idp2) = setup(&[("gh", "github")]).await;
    let (local2, _) = h2.verified_user("octo@example.com").await;
    idp2.set_profile(profile("x", Some("octo@example.com"), true));
    let r = sign_in(&h2, &idp2, "gh").await;
    assert_eq!(r.json["user"]["id"], local2);
}

#[tokio::test]
async fn discord_facebook_and_wechat() {
    let (h, idp) = setup(&[("dc", "discord"), ("fb", "facebook"), ("wx", "wechat")]).await;
    // Discord: `verified` is honoured.
    idp.set_profile(profile("x", Some("dc@example.com"), true));
    let r = sign_in(&h, &idp, "dc").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["email"], "dc@example.com");
    assert_eq!(r.json["user"]["verified"], true);

    // Facebook: the email is never trusted as verified.
    idp.set_profile(profile("x", Some("fb@example.com"), true));
    let r = sign_in(&h, &idp, "fb").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json["user"]["email"].as_str().unwrap().ends_with("@oauth.invalid"));

    // WeChat: openid / unionid, no PKCE, no email.
    idp.set_profile(profile("no-union", None, false));
    let s = start(&h, "wx").await;
    let u = s["authorization_url"].as_str().unwrap();
    assert!(
        u.contains("appid=client-wx") && u.ends_with("#wechat_redirect") && !u.contains("code_challenge"),
        "{u}"
    );
    let (code, state) = idp.authorize(u);
    let r = callback(&h, "wx", &code, &state).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let first = r.json["user"]["id"].clone();
    // With a unionid the identity key changes to it (new account, by design).
    idp.set_profile(profile("union:abc", None, false));
    let r = sign_in(&h, &idp, "wx").await;
    assert_ne!(r.json["user"]["id"], first);
    let r2 = sign_in(&h, &idp, "wx").await;
    assert_eq!(r2.json["user"]["id"], r.json["user"]["id"]);
    // WeChat's 200-with-errcode failure becomes a provider error.
    let s = start(&h, "wx").await;
    let (_, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let r = callback(&h, "wx", "forged-code", &state).await;
    assert_error(&r, StatusCode::BAD_GATEWAY, "provider_error");
}

#[tokio::test]
async fn apple_form_post_and_signed_client_secret() {
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePrivateKey;
    let (h, idp) = setup(&[]).await;
    let key = SigningKey::from_slice(&[5u8; 32]).unwrap();
    idp.set_apple_verifier(*key.verifying_key());
    let pem = key.to_pkcs8_pem(Default::default()).unwrap().to_string();
    let b = &idp.base;
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "apple", "kind": "apple", "client_id": "com.example.web",
        "authorize_url": format!("{b}/authorize"), "token_url": format!("{b}/token"),
        "jwks_url": format!("{b}/certs"), "issuer": b,
        "apple": {"team_id": "TEAM123456", "key_id": "KEY1", "private_key_pem": pem},
    }))
    .unwrap();
    assert_eq!(cfg.kind, ProviderKind::Apple);
    h.auth.providers().register("default", cfg).unwrap();
    let s = start(&h, "apple").await;
    let u = url::Url::parse(s["authorization_url"].as_str().unwrap()).unwrap();
    let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
    assert_eq!(q["response_mode"], "form_post");
    assert!(!q.contains_key("code_challenge"));
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let body = format!("code={code}&state={state}");
    let r = h
        .req_raw(
            axum::http::Method::POST,
            "/api/auth/oauth/apple/callback",
            None,
            Some(("application/x-www-form-urlencoded", body)),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["email"], "pat@example.com");
}

#[tokio::test]
async fn json_callback_for_single_page_apps() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let s = start(&h, "a").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    let r = h
        .post(
            "/api/auth/oauth/a/callback",
            None,
            json!({"code": code, "state": state}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn browser_start_redirects() {
    let (h, _idp) = setup(&[("a", "oidc")]).await;
    let r = h.get("/api/auth/oauth/a/start?cookie=true", None).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert!(
        r.headers["location"]
            .to_str()
            .unwrap()
            .contains("/authorize?client_id=client-a")
    );
}

#[tokio::test]
async fn generic_oidc_discovery() {
    let (h, idp) = setup(&[]).await;
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "corp", "kind": "oidc", "client_id": "client-corp", "client_secret": CLIENT_SECRET,
        "issuer": idp.base, "display_name": "Corp SSO",
    }))
    .unwrap();
    h.auth.providers().register("default", cfg).unwrap();
    let r = sign_in(&h, &idp, "corp").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let list = h.get("/api/auth/oauth/providers", None).await;
    assert_eq!(list.json["providers"][0]["display_name"], "Corp SSO");
    assert!(!list.json.to_string().contains(CLIENT_SECRET));

    // A discovery document claiming another issuer is refused.
    let (h2, idp2) = setup(&[]).await;
    idp2.quirks(|q| q.discovery_issuer = Some("https://evil.example".into()));
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": "corp", "kind": "oidc", "client_id": "c", "client_secret": CLIENT_SECRET, "issuer": idp2.base,
    }))
    .unwrap();
    h2.auth.providers().register("default", cfg).unwrap();
    let r = h2.post("/api/auth/oauth/corp/start", None, json!({})).await;
    assert_error(&r, StatusCode::BAD_GATEWAY, "provider_error");
}

#[tokio::test]
async fn email_from_userinfo_when_missing_from_id_token() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    idp.quirks(|q| q.userinfo_email = true);
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["email"], "pat@example.com");
}

#[tokio::test]
async fn totp_users_still_face_mfa_after_social_login() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let setup = h.post("/api/auth/mfa/totp/setup", Some(&token), json!({})).await;
    let secret = forgebase_auth::crypto::base32_decode(setup.json["secret"].as_str().unwrap()).unwrap();
    let now = forgebase_auth::store::now() as u64;
    let code = forgebase_auth::crypto::hotp(&secret, now / 30);
    let r = h
        .post("/api/auth/mfa/totp/confirm", Some(&token), json!({"code": code}))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = sign_in(&h, &idp, "a").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["mfa_required"], true, "{}", r.json);
    assert!(r.json.get("access_token").is_none());
}

#[tokio::test]
async fn secrets_stay_out_of_debug_output_and_the_database() {
    let (h, idp) = setup(&[("a", "oidc")]).await;
    let cfg = provider(&idp, "a", "oidc");
    assert!(!format!("{cfg:?}").contains(CLIENT_SECRET), "{cfg:?}");
    let s = start(&h, "a").await;
    let (code, state) = idp.authorize(s["authorization_url"].as_str().unwrap());
    assert_eq!(callback(&h, "a", &code, &state).await.status, StatusCode::OK);
    // The state and PKCE verifier are not stored in clear.
    let bytes = std::fs::read(h.dir.path().join("forgebase.db")).unwrap();
    let wal = std::fs::read(h.dir.path().join("forgebase.db-wal")).unwrap_or_default();
    for blob in [&bytes, &wal] {
        let hay = String::from_utf8_lossy(blob);
        assert!(!hay.contains(&state), "state in database");
        assert!(!hay.contains(&code), "code in database");
    }
}

#[tokio::test]
async fn providers_load_from_the_data_directory_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("auth")).unwrap();
    std::fs::write(
        dir.path().join("auth/oauth-providers.json"),
        json!({"providers": [{"id": "g", "kind": "google", "client_id": "x", "client_secret": "y"}]}).to_string(),
    )
    .unwrap();
    let cfg = forgebase_core::Config::defaults(dir.path());
    let db = forgebase_db::Database::open(forgebase_db::DbOptions::from_config(
        &cfg.database,
        dir.path().join("forgebase.db"),
    ))
    .await
    .unwrap();
    db.migrate(None).await.unwrap();
    let mut acfg = forgebase_auth::AuthConfig::for_data_dir(dir.path());
    acfg.argon2 = forgebase_auth::Argon2Config::insecure_fast_for_tests();
    let auth = forgebase_auth::AuthService::open_with_data(
        std::sync::Arc::new(db),
        std::sync::Arc::new(forgebase_auth::data::DefaultAuthorizer),
        dir.path(),
        acfg,
        None,
    )
    .await
    .unwrap();
    assert_eq!(auth.oauth_providers().len(), 1);
    // Also: invalid provider config is rejected.
    let bad: Result<ProviderConfig, _> =
        serde_json::from_value(json!({"id": "Bad Id", "kind": "github", "client_id": "x", "client_secret": "y"}));
    assert!(auth.providers().register("default", bad.unwrap()).is_err());
    let _ = default_profile();
}
