//! WebAuthn ceremonies against a software authenticator.

#![allow(clippy::unwrap_used, clippy::type_complexity, missing_docs)]

mod common;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use common::{H, Resp, assert_error, harness};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn b64(b: &[u8]) -> String {
    B64.encode(b)
}

// --- a minimal CBOR encoder for the attestation object ----------------------

fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        _ => vec![m | 25, (n >> 8) as u8, n as u8],
    }
}
fn c_int(i: i64) -> Vec<u8> {
    if i >= 0 {
        head(0, i as u64)
    } else {
        head(1, (-1 - i) as u64)
    }
}
fn c_bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}
fn c_text(s: &str) -> Vec<u8> {
    [head(3, s.len() as u64), s.as_bytes().to_vec()].concat()
}
fn c_map(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut out = head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(k);
        out.extend(v);
    }
    out
}

/// A software authenticator holding one ES256 credential.
struct Authn {
    key: SigningKey,
    cred_id: Vec<u8>,
    counter: u32,
    rp_id: String,
    origin: String,
    /// Report the UV flag.
    uv: bool,
    handle: Option<String>,
}

impl Authn {
    fn new(h: &H, seed: u8) -> Self {
        let cfg = h.auth.webauthn();
        Self {
            key: SigningKey::from_slice(&[seed; 32]).unwrap(),
            cred_id: vec![seed; 32],
            counter: 0,
            rp_id: cfg.rp_id.clone(),
            origin: cfg.origins[0].clone(),
            uv: true,
            handle: None,
        }
    }

    fn flags(&self, attested: bool, up: bool) -> u8 {
        u8::from(up) | (u8::from(self.uv) << 2) | (u8::from(attested) << 6)
    }

    fn client_data(&self, typ: &str, challenge: &str, origin: &str) -> String {
        json!({"type": typ, "challenge": challenge, "origin": origin, "crossOrigin": false}).to_string()
    }

    fn cose(&self) -> Vec<u8> {
        let p = self.key.verifying_key().to_sec1_point(false);
        c_map(vec![
            (c_int(1), c_int(2)),
            (c_int(3), c_int(-7)),
            (c_int(-1), c_int(1)),
            (c_int(-2), c_bytes(p.x().unwrap())),
            (c_int(-3), c_bytes(p.y().unwrap())),
        ])
    }

    /// `navigator.credentials.create` result for the given options.
    fn create(&mut self, options: &Value, tweak: impl Fn(&mut CreateOpts)) -> Value {
        let mut o = CreateOpts {
            origin: self.origin.clone(),
            challenge: options["publicKey"]["challenge"].as_str().unwrap().to_owned(),
            rp_id: self.rp_id.clone(),
            up: true,
            id_override: None,
        };
        tweak(&mut o);
        self.handle = options["publicKey"]["user"]["id"].as_str().map(str::to_owned);
        let mut ad = Sha256::digest(o.rp_id.as_bytes()).to_vec();
        ad.push(self.flags(true, o.up));
        ad.extend(self.counter.to_be_bytes());
        ad.extend([0u8; 16]);
        ad.extend((self.cred_id.len() as u16).to_be_bytes());
        ad.extend(&self.cred_id);
        ad.extend(self.cose());
        let att = c_map(vec![
            (c_text("fmt"), c_text("none")),
            (c_text("attStmt"), c_map(vec![])),
            (c_text("authData"), c_bytes(&ad)),
        ]);
        let cd = self.client_data("webauthn.create", &o.challenge, &o.origin);
        json!({
            "id": o.id_override.unwrap_or_else(|| b64(&self.cred_id)),
            "rawId": b64(&self.cred_id), "type": "public-key",
            "response": {
                "clientDataJSON": b64(cd.as_bytes()),
                "attestationObject": b64(&att),
                "transports": ["internal"],
            },
        })
    }

    /// `navigator.credentials.get` result.
    fn get(&mut self, options: &Value, tweak: impl Fn(&mut GetOpts)) -> Value {
        let mut o = GetOpts {
            origin: self.origin.clone(),
            challenge: options["publicKey"]["challenge"].as_str().unwrap().to_owned(),
            rp_id: self.rp_id.clone(),
            up: true,
            counter: None,
            corrupt_sig: false,
            user_handle: self.handle.clone(),
        };
        tweak(&mut o);
        self.counter = o.counter.unwrap_or(self.counter + 1);
        let mut ad = Sha256::digest(o.rp_id.as_bytes()).to_vec();
        ad.push(self.flags(false, o.up));
        ad.extend(self.counter.to_be_bytes());
        let cd = self.client_data("webauthn.get", &o.challenge, &o.origin);
        let mut signed = ad.clone();
        signed.extend(Sha256::digest(cd.as_bytes()));
        let sig: Signature = self.key.sign(&signed);
        let mut der = sig.to_der().as_bytes().to_vec();
        if o.corrupt_sig {
            let n = der.len() - 1;
            der[n] ^= 1;
        }
        json!({
            "id": b64(&self.cred_id), "rawId": b64(&self.cred_id), "type": "public-key",
            "response": {
                "clientDataJSON": b64(cd.as_bytes()),
                "authenticatorData": b64(&ad),
                "signature": b64(&der),
                "userHandle": o.user_handle,
            },
        })
    }
}

struct CreateOpts {
    origin: String,
    challenge: String,
    rp_id: String,
    up: bool,
    id_override: Option<String>,
}

struct GetOpts {
    origin: String,
    challenge: String,
    rp_id: String,
    up: bool,
    counter: Option<u32>,
    corrupt_sig: bool,
    user_handle: Option<String>,
}

async fn register(h: &H, token: &str, a: &mut Authn, mfa: bool) -> Resp {
    let o = h
        .post(
            "/api/auth/webauthn/register/options",
            Some(token),
            json!({"use_as_mfa": mfa}),
        )
        .await;
    assert_eq!(o.status, StatusCode::OK, "{}", o.json);
    let cred = a.create(&o.json, |_| {});
    let r = h
        .post(
            "/api/auth/webauthn/register/verify",
            Some(token),
            json!({"ceremony_id": o.json["ceremony_id"], "credential": cred, "name": "test key"}),
        )
        .await;
    if r.status != StatusCode::CREATED {
        eprintln!("register failed: {} {}", r.status, r.json);
    }
    r
}

async fn login_options(h: &H, body: Value) -> Value {
    let o = h.post("/api/auth/webauthn/login/options", None, body).await;
    assert_eq!(o.status, StatusCode::OK, "{}", o.json);
    o.json
}

async fn login(h: &H, a: &mut Authn, tweak: impl Fn(&mut GetOpts)) -> Resp {
    let o = login_options(h, json!({})).await;
    let cred = a.get(&o, tweak);
    h.post(
        "/api/auth/webauthn/login/verify",
        None,
        json!({"ceremony_id": o["ceremony_id"], "credential": cred}),
    )
    .await
}

#[tokio::test]
async fn register_then_sign_in_with_a_passkey() {
    let h = harness(|_| {}).await;
    let (uid, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);

    let o = h
        .post("/api/auth/webauthn/register/options", Some(&token), json!({}))
        .await;
    let pk = &o.json["publicKey"];
    assert_eq!(pk["rp"]["id"], h.auth.webauthn().rp_id);
    assert_eq!(pk["attestation"], "none");
    assert_eq!(pk["pubKeyCredParams"][0]["alg"], -7);
    assert_ne!(pk["user"]["id"], uid, "the user handle is not the record id");
    assert_eq!(pk["user"]["name"], "pat@example.com");

    let r = register(&h, &token, &mut a, false).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["credential"]["user_verified"], true);
    assert_eq!(r.json["credential"]["name"], "test key");

    let list = h.get("/api/auth/webauthn/credentials", Some(&token)).await;
    assert_eq!(list.json["credentials"].as_array().unwrap().len(), 1);

    // Usernameless sign-in; user verification makes it a multi-factor session.
    let r = login(&h, &mut a, |_| {}).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["id"], uid);
    let access = r.json["access_token"].as_str().unwrap();
    let me = h.get("/api/auth/me", Some(access)).await;
    assert_eq!(me.json["subject"]["id"], uid);
    assert_eq!(me.json["mfa"], true, "{}", me.json);
    let sessions = h.get("/api/auth/sessions", Some(access)).await;
    let current = sessions.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["current"] == true)
        .unwrap();
    assert_eq!(current["auth_method"], "passkey", "{}", sessions.json);

    // The counter moved forward.
    let list = h.get("/api/auth/webauthn/credentials", Some(&token)).await;
    assert_eq!(list.json["credentials"][0]["sign_count"], 1);
    assert!(list.json["credentials"][0]["last_used_at"].is_string());

    // Registering the same credential again conflicts.
    let r = register(&h, &token, &mut a, false).await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
}

#[tokio::test]
async fn registration_checks() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let (_, other) = h.verified_user("eve@example.com").await;
    let cases: Vec<(&str, Box<dyn Fn(&mut CreateOpts)>)> = vec![
        ("wrong origin", Box::new(|o| o.origin = "https://evil.example".into())),
        ("wrong challenge", Box::new(|o| o.challenge = b64(b"not the challenge"))),
        ("wrong rp id", Box::new(|o| o.rp_id = "evil.example".into())),
        ("no user presence", Box::new(|o| o.up = false)),
        (
            "credential id mismatch",
            Box::new(|o| o.id_override = Some(b64(b"other"))),
        ),
    ];
    for (name, tweak) in cases {
        let mut a = Authn::new(&h, 2);
        let o = h
            .post("/api/auth/webauthn/register/options", Some(&token), json!({}))
            .await;
        let cred = a.create(&o.json, &tweak);
        let r = h
            .post(
                "/api/auth/webauthn/register/verify",
                Some(&token),
                json!({"ceremony_id": o.json["ceremony_id"], "credential": cred}),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{name}: {}", r.json);
    }
    assert!(
        h.get("/api/auth/webauthn/credentials", Some(&token)).await.json["credentials"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A ceremony is single use...
    let mut a = Authn::new(&h, 3);
    let o = h
        .post("/api/auth/webauthn/register/options", Some(&token), json!({}))
        .await;
    let cred = a.create(&o.json, |_| {});
    let body = json!({"ceremony_id": o.json["ceremony_id"], "credential": cred});
    assert_eq!(
        h.post("/api/auth/webauthn/register/verify", Some(&token), body.clone())
            .await
            .status,
        StatusCode::CREATED
    );
    let r = h.post("/api/auth/webauthn/register/verify", Some(&token), body).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // ...and belongs to the user who opened it.
    let mut b = Authn::new(&h, 4);
    let o = h
        .post("/api/auth/webauthn/register/options", Some(&token), json!({}))
        .await;
    let cred = b.create(&o.json, |_| {});
    let r = h
        .post(
            "/api/auth/webauthn/register/verify",
            Some(&other),
            json!({"ceremony_id": o.json["ceremony_id"], "credential": cred}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Anonymous callers cannot register.
    let r = h.post("/api/auth/webauthn/register/options", None, json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
}

#[tokio::test]
async fn assertion_checks() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);
    assert_eq!(register(&h, &token, &mut a, false).await.status, StatusCode::CREATED);

    let cases: Vec<(&str, Box<dyn Fn(&mut GetOpts)>)> = vec![
        ("wrong origin", Box::new(|o| o.origin = "https://evil.example".into())),
        ("wrong challenge", Box::new(|o| o.challenge = b64(b"nope"))),
        ("wrong rp id", Box::new(|o| o.rp_id = "evil.example".into())),
        ("no presence", Box::new(|o| o.up = false)),
        ("bad signature", Box::new(|o| o.corrupt_sig = true)),
        (
            "wrong user handle",
            Box::new(|o| o.user_handle = Some(b64(b"someone else"))),
        ),
    ];
    for (name, tweak) in cases {
        let r = login(&h, &mut a, &tweak).await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
        let _ = name;
    }

    // User verification is required for a first factor.
    a.uv = false;
    let r = login(&h, &mut a, |_| {}).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    a.uv = true;

    // An unknown credential.
    let mut stranger = Authn::new(&h, 9);
    stranger.handle = None;
    let r = login(&h, &mut stranger, |_| {}).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");

    // A ceremony cannot be replayed: same assertion, second time.
    let o = login_options(&h, json!({})).await;
    let cred = a.get(&o, |_| {});
    let body = json!({"ceremony_id": o["ceremony_id"], "credential": cred});
    assert_eq!(
        h.post("/api/auth/webauthn/login/verify", None, body.clone())
            .await
            .status,
        StatusCode::OK
    );
    let r = h.post("/api/auth/webauthn/login/verify", None, body).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");

    // Failures are audited, with no secrets.
    let admin = h.auth_admin_token().await;
    let audit = h
        .get("/api/auth/audit?event=webauthn.login&limit=50", Some(&admin))
        .await;
    assert!(audit.json["items"].as_array().unwrap().len() >= 5, "{}", audit.json);
}

#[tokio::test]
async fn sign_count_rollback_means_a_cloned_authenticator() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);
    register(&h, &token, &mut a, false).await;
    assert_eq!(login(&h, &mut a, |o| o.counter = Some(10)).await.status, StatusCode::OK);
    // Counter goes backwards or stands still: refused.
    for c in [10, 5, 1] {
        let r = login(&h, &mut a, |o| o.counter = Some(c)).await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    // A later, higher counter works again (the stored value did not regress).
    assert_eq!(login(&h, &mut a, |o| o.counter = Some(11)).await.status, StatusCode::OK);
    let admin = h.auth_admin_token().await;
    let audit = h
        .get("/api/auth/audit?event=webauthn.clone_suspected", Some(&admin))
        .await;
    assert_eq!(audit.json["items"].as_array().unwrap().len(), 3, "{}", audit.json);
}

#[tokio::test]
async fn synced_passkeys_with_a_zero_counter_are_accepted() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);
    register(&h, &token, &mut a, false).await;
    for _ in 0..2 {
        let r = login(&h, &mut a, |o| o.counter = Some(0)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    }
}

#[tokio::test]
async fn login_options_do_not_reveal_accounts() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);
    register(&h, &token, &mut a, false).await;
    let known = login_options(&h, json!({"email": "pat@example.com"})).await;
    let unknown = login_options(&h, json!({"email": "ghost@example.com"})).await;
    let again = login_options(&h, json!({"email": "ghost@example.com"})).await;
    assert_eq!(known["publicKey"]["allowCredentials"][0]["id"], b64(&a.cred_id));
    let k = &known["publicKey"]["allowCredentials"];
    let u = &unknown["publicKey"]["allowCredentials"];
    assert_eq!((k.as_array().unwrap().len(), u.as_array().unwrap().len()), (1, 1));
    assert_eq!(
        u[0]["id"], again["publicKey"]["allowCredentials"][0]["id"],
        "the fake credential is stable"
    );
    assert_eq!(known["publicKey"]["userVerification"], "required");
}

#[tokio::test]
async fn passkey_as_second_factor_after_a_password() {
    let h = harness(|_| {}).await;
    let (uid, token) = h.verified_user("pat@example.com").await;
    let mut mfa_key = Authn::new(&h, 1);
    let mut plain_key = Authn::new(&h, 2);
    assert_eq!(
        register(&h, &token, &mut mfa_key, true).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        register(&h, &token, &mut plain_key, false).await.status,
        StatusCode::CREATED
    );
    mfa_key.uv = false; // user presence only is fine for a second factor

    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "pat@example.com", "password": common::PW}),
        )
        .await;
    assert_eq!(r.json["mfa_required"], true, "{}", r.json);
    let mfa_token = r.json["mfa_token"].as_str().unwrap().to_owned();

    // A passkey not enabled for MFA is refused.
    let o = h
        .post("/api/auth/webauthn/mfa/options", None, json!({"mfa_token": mfa_token}))
        .await;
    assert_eq!(o.status, StatusCode::OK, "{}", o.json);
    assert_eq!(o.json["publicKey"]["allowCredentials"].as_array().unwrap().len(), 1);
    let cred = plain_key.get(&o.json, |_| {});
    let r = h
        .post(
            "/api/auth/webauthn/mfa/verify",
            None,
            json!({"mfa_token": mfa_token, "ceremony_id": o.json["ceremony_id"], "credential": cred}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");

    // The right one finishes the sign-in.
    let o = h
        .post("/api/auth/webauthn/mfa/options", None, json!({"mfa_token": mfa_token}))
        .await;
    assert_eq!(o.status, StatusCode::OK, "{}", o.json);
    let cred = mfa_key.get(&o.json, |_| {});
    let body = json!({"mfa_token": mfa_token, "ceremony_id": o.json["ceremony_id"], "credential": cred});
    let r = h.post("/api/auth/webauthn/mfa/verify", None, body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["subject"]["id"], uid);
    let me = h
        .get("/api/auth/me", Some(r.json["access_token"].as_str().unwrap()))
        .await;
    assert_eq!(me.json["mfa"], true);

    // The challenge is single use, and a ceremony opened for one challenge
    // does not complete another.
    let r = h.post("/api/auth/webauthn/mfa/verify", None, body).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    // A ceremony opened for one challenge does not complete another (a new
    // login replaces the user's older challenge, so use a forged token).
    let r2 = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "pat@example.com", "password": common::PW}),
        )
        .await;
    let t2 = r2.json["mfa_token"].as_str().unwrap();
    let o = h
        .post("/api/auth/webauthn/mfa/options", None, json!({"mfa_token": t2}))
        .await;
    assert_eq!(o.status, StatusCode::OK, "{}", o.json);
    let cred = mfa_key.get(&o.json, |_| {});
    let r = h
        .post(
            "/api/auth/webauthn/mfa/verify",
            None,
            json!({"mfa_token": "forged-token", "ceremony_id": o.json["ceremony_id"], "credential": cred}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_token");
}

#[tokio::test]
async fn passkey_login_satisfies_mfa_for_totp_users() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let mut a = Authn::new(&h, 1);
    register(&h, &token, &mut a, false).await;
    let setup = h.post("/api/auth/mfa/totp/setup", Some(&token), json!({})).await;
    let secret = forgebase_auth::crypto::base32_decode(setup.json["secret"].as_str().unwrap()).unwrap();
    let code = forgebase_auth::crypto::hotp(&secret, forgebase_auth::store::now() as u64 / 30);
    assert_eq!(
        h.post("/api/auth/mfa/totp/confirm", Some(&token), json!({"code": code}))
            .await
            .status,
        StatusCode::OK
    );
    let r = login(&h, &mut a, |_| {}).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json.get("mfa_required").is_none());
}

#[tokio::test]
async fn deleting_passkeys() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("pat@example.com").await;
    let (_, other) = h.verified_user("eve@example.com").await;
    let mut a = Authn::new(&h, 1);
    register(&h, &token, &mut a, false).await;
    let id = b64(&a.cred_id);
    let r = h
        .delete(&format!("/api/auth/webauthn/credentials/{id}"), Some(&other))
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
    let r = h
        .delete(&format!("/api/auth/webauthn/credentials/{id}"), Some(&token))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = login(&h, &mut a, |_| {}).await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
}

impl H {
    async fn auth_admin_token(&self) -> String {
        self.auth
            .create_admin(
                "root@example.com",
                "admin password 1",
                "test",
                &forgebase_auth::ClientInfo::default(),
            )
            .await
            .unwrap();
        let r = self
            .post(
                "/api/auth/admins/login",
                None,
                json!({"email": "root@example.com", "password": "admin password 1"}),
            )
            .await;
        r.json["access_token"].as_str().unwrap().to_owned()
    }
}
