//! An in-process mock identity provider on a real local TCP port: authorize
//! (driven by the tests), token, userinfo, JWKS, discovery, plus the
//! GitHub / Discord / Facebook / WeChat shaped profile endpoints.

#![allow(dead_code, clippy::unwrap_used, clippy::result_large_err, missing_docs)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use num_bigint::BigUint;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::rsa_key::{D_HEX, N_HEX};

pub const CLIENT_SECRET: &str = "s3cret-client-secret";

#[derive(Clone)]
pub struct Profile {
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: String,
}

#[derive(Clone, Default)]
pub struct Quirks {
    pub aud: Option<String>,
    pub exp_delta: Option<i64>,
    pub iss: Option<String>,
    pub nonce: Option<String>,
    pub rsa: bool,
    pub tamper: bool,
    pub kid: Option<String>,
    pub alg: Option<String>,
    pub no_id_token: bool,
    pub discovery_issuer: Option<String>,
    pub userinfo_email: bool,
}

struct Grant {
    nonce: Option<String>,
    challenge: Option<String>,
    client_id: String,
    redirect_uri: String,
}

struct St {
    grants: HashMap<String, Grant>,
    redeemed: HashSet<String>,
    profile: Profile,
    quirks: Quirks,
    apple_verifier: Option<p256::ecdsa::VerifyingKey>,
    redeem_count: usize,
}

pub struct Idp {
    pub base: String,
    st: Mutex<St>,
    ec: SigningKey,
    pub jwks_hits: AtomicUsize,
}

fn b64(b: &[u8]) -> String {
    B64.encode(b)
}

fn big(hex: &str) -> BigUint {
    BigUint::parse_bytes(hex.as_bytes(), 16).unwrap()
}

pub fn rsa_public() -> (Vec<u8>, Vec<u8>) {
    (big(N_HEX).to_bytes_be(), vec![1, 0, 1])
}

fn rsa_sign(msg: &[u8]) -> Vec<u8> {
    const PREFIX: [u8; 19] = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04,
        0x20,
    ];
    let (n, d) = (big(N_HEX), big(D_HEX));
    let k = 256;
    let hash = Sha256::digest(msg);
    let mut em = vec![0, 1];
    em.extend(std::iter::repeat_n(0xff, k - 3 - 19 - 32));
    em.push(0);
    em.extend_from_slice(&PREFIX);
    em.extend_from_slice(&hash);
    let s = BigUint::from_bytes_be(&em).modpow(&d, &n).to_bytes_be();
    let mut out = vec![0; k - s.len()];
    out.extend(s);
    out
}

pub fn default_profile() -> Profile {
    Profile {
        sub: "idp-user-1".into(),
        email: Some("pat@example.com".into()),
        email_verified: true,
        name: "Pat Example".into(),
    }
}

impl Idp {
    pub async fn start() -> Arc<Idp> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let idp = Arc::new(Idp {
            base,
            st: Mutex::new(St {
                grants: HashMap::new(),
                redeemed: HashSet::new(),
                profile: default_profile(),
                quirks: Quirks::default(),
                apple_verifier: None,
                redeem_count: 0,
            }),
            ec: SigningKey::from_slice(&[9u8; 32]).unwrap(),
            jwks_hits: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/token", post(token))
            .route("/certs", get(certs))
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/userinfo", get(userinfo))
            .route("/gh/user", get(gh_user))
            .route("/gh/emails", get(gh_emails))
            .route("/discord/me", get(discord_me))
            .route("/fb/me", get(fb_me))
            .route("/wx/token", get(wx_token))
            .route("/wx/userinfo", get(wx_userinfo))
            .with_state(idp.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        idp
    }

    pub fn set_profile(&self, p: Profile) {
        self.st.lock().unwrap().profile = p;
    }

    pub fn quirks(&self, f: impl FnOnce(&mut Quirks)) {
        f(&mut self.st.lock().unwrap().quirks);
    }

    pub fn reset_quirks(&self) {
        self.st.lock().unwrap().quirks = Quirks::default();
    }

    pub fn set_apple_verifier(&self, v: p256::ecdsa::VerifyingKey) {
        self.st.lock().unwrap().apple_verifier = Some(v);
    }

    pub fn redeemed(&self) -> usize {
        self.st.lock().unwrap().redeem_count
    }

    /// Simulates the user approving: records the request parameters and
    /// returns `(code, state)`.
    pub fn authorize(&self, auth_url: &str) -> (String, String) {
        let u = url::Url::parse(auth_url).unwrap();
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        let code = format!("code-{}", uuid::Uuid::new_v4());
        self.st.lock().unwrap().grants.insert(
            code.clone(),
            Grant {
                nonce: q.get("nonce").cloned(),
                challenge: q.get("code_challenge").cloned(),
                client_id: q
                    .get("client_id")
                    .or_else(|| q.get("appid"))
                    .cloned()
                    .unwrap_or_default(),
                redirect_uri: q.get("redirect_uri").cloned().unwrap_or_default(),
            },
        );
        (code, q["state"].clone())
    }

    fn jwks(&self) -> Value {
        let point = self.ec.verifying_key().to_sec1_point(false);
        let (n, e) = rsa_public();
        json!({"keys": [
            {"kty": "EC", "crv": "P-256", "kid": "ec1", "use": "sig", "alg": "ES256",
             "x": b64(point.x().unwrap()), "y": b64(point.y().unwrap())},
            {"kty": "RSA", "kid": "rsa1", "use": "sig", "alg": "RS256", "n": b64(&n), "e": b64(&e)},
        ]})
    }

    fn id_token(&self, st: &St, grant: &Grant) -> String {
        let q = &st.quirks;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let mut claims = json!({
            "iss": q.iss.clone().unwrap_or_else(|| self.base.clone()),
            "aud": q.aud.clone().unwrap_or_else(|| grant.client_id.clone()),
            "sub": st.profile.sub,
            "iat": now,
            "exp": now + q.exp_delta.unwrap_or(3600),
            "name": st.profile.name,
        });
        if let Some(n) = q.nonce.clone().or_else(|| grant.nonce.clone()) {
            claims["nonce"] = json!(n);
        }
        if !q.userinfo_email
            && let Some(e) = &st.profile.email
        {
            claims["email"] = json!(e);
            claims["email_verified"] = json!(st.profile.email_verified);
        }
        let alg = q
            .alg
            .clone()
            .unwrap_or_else(|| if q.rsa { "RS256" } else { "ES256" }.into());
        let kid = q
            .kid
            .clone()
            .unwrap_or_else(|| if q.rsa { "rsa1" } else { "ec1" }.into());
        let h = b64(json!({"alg": alg, "kid": kid, "typ": "JWT"}).to_string().as_bytes());
        let p = b64(claims.to_string().as_bytes());
        let input = format!("{h}.{p}");
        let mut sig = match alg.as_str() {
            "none" => Vec::new(),
            "HS256" => vec![7; 32],
            "RS256" => rsa_sign(input.as_bytes()),
            _ => {
                let s: Signature = self.ec.sign(input.as_bytes());
                s.to_bytes().to_vec()
            }
        };
        if q.tamper
            && let Some(b) = sig.first_mut()
        {
            *b ^= 1;
        }
        format!("{input}.{}", b64(&sig))
    }

    fn redeem(
        &self,
        code: &str,
        redirect: Option<&str>,
        client_id: &str,
        verifier: Option<&str>,
    ) -> Result<Value, Response> {
        let err = |m: &str| Err((StatusCode::BAD_REQUEST, axum::Json(json!({"error": m}))).into_response());
        let mut st = self.st.lock().unwrap();
        if st.redeemed.contains(code) {
            return err("invalid_grant");
        }
        let Some(g) = st.grants.get(code) else {
            return err("invalid_grant");
        };
        if g.client_id != client_id || redirect.is_some_and(|r| r != g.redirect_uri) {
            return err("invalid_grant");
        }
        if let Some(ch) = &g.challenge {
            let ok = verifier.is_some_and(|v| b64(&Sha256::digest(v.as_bytes())) == *ch);
            if !ok {
                return err("invalid_grant");
            }
        }
        st.redeemed.insert(code.to_owned());
        st.redeem_count += 1;
        let g = st.grants.get(code).unwrap();
        let mut out = json!({"access_token": format!("at-{code}"), "token_type": "Bearer", "expires_in": 3600});
        if !st.quirks.no_id_token {
            out["id_token"] = json!(self.id_token(&st, g));
        }
        Ok(out)
    }

    fn profile(&self) -> Profile {
        self.st.lock().unwrap().profile.clone()
    }
}

fn form(body: &Bytes) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn query(uri: &Uri) -> HashMap<String, String> {
    url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

async fn token(State(idp): State<Arc<Idp>>, body: Bytes) -> Response {
    let f = form(&body);
    let secret = f.get("client_secret").cloned().unwrap_or_default();
    let verifier = idp.st.lock().unwrap().apple_verifier;
    let secret_ok = match verifier {
        // Apple: the client secret is an ES256 JWT signed with the team key.
        Some(v) => {
            let parts: Vec<&str> = secret.split('.').collect();
            parts.len() == 3 && {
                let sig = B64.decode(parts[2]).unwrap_or_default();
                let claims: Value =
                    serde_json::from_slice(&B64.decode(parts[1]).unwrap_or_default()).unwrap_or_default();
                Signature::from_slice(&sig)
                    .is_ok_and(|s| v.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &s).is_ok())
                    && claims["aud"] == "https://appleid.apple.com"
                    && claims["sub"] == f.get("client_id").map(String::as_str).unwrap_or_default()
                    && claims["iss"] == "TEAM123456"
            }
        }
        None => secret == CLIENT_SECRET,
    };
    if !secret_ok {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "invalid_client"}))).into_response();
    }
    match idp.redeem(
        f.get("code").map(String::as_str).unwrap_or_default(),
        f.get("redirect_uri").map(String::as_str),
        f.get("client_id").map(String::as_str).unwrap_or_default(),
        f.get("code_verifier").map(String::as_str),
    ) {
        Ok(v) => axum::Json(v).into_response(),
        Err(r) => r,
    }
}

async fn certs(State(idp): State<Arc<Idp>>) -> Response {
    idp.jwks_hits.fetch_add(1, Ordering::SeqCst);
    axum::Json(idp.jwks()).into_response()
}

async fn discovery(State(idp): State<Arc<Idp>>) -> Response {
    let issuer = idp
        .st
        .lock()
        .unwrap()
        .quirks
        .discovery_issuer
        .clone()
        .unwrap_or_else(|| idp.base.clone());
    axum::Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{}/authorize", idp.base),
        "token_endpoint": format!("{}/token", idp.base),
        "jwks_uri": format!("{}/certs", idp.base),
        "userinfo_endpoint": format!("{}/userinfo", idp.base),
    }))
    .into_response()
}

fn bearer_ok(h: &HeaderMap) -> bool {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer at-code-"))
}

async fn userinfo(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = idp.profile();
    axum::Json(json!({"sub": p.sub, "email": p.email, "email_verified": p.email_verified})).into_response()
}

async fn gh_user(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = idp.profile();
    // GitHub's numeric id; the profile's own `email` is deliberately unverified.
    axum::Json(json!({"id": 4242, "login": "octo", "name": p.name, "email": p.email})).into_response()
}

async fn gh_emails(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = idp.profile();
    let list: Vec<Value> = p
        .email
        .iter()
        .map(|e| json!({"email": e, "primary": true, "verified": p.email_verified}))
        .collect();
    axum::Json(list).into_response()
}

async fn discord_me(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = idp.profile();
    axum::Json(json!({"id": "9001", "username": "disco", "email": p.email, "verified": p.email_verified}))
        .into_response()
}

async fn fb_me(State(idp): State<Arc<Idp>>, h: HeaderMap) -> Response {
    if !bearer_ok(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = idp.profile();
    axum::Json(json!({"id": "fb-77", "name": p.name, "email": p.email})).into_response()
}

async fn wx_token(State(idp): State<Arc<Idp>>, uri: Uri) -> Response {
    let q = query(&uri);
    if q.get("secret").map(String::as_str) != Some(CLIENT_SECRET) {
        return axum::Json(json!({"errcode": 40125, "errmsg": "invalid appsecret"})).into_response();
    }
    match idp.redeem(
        q.get("code").map(String::as_str).unwrap_or_default(),
        None,
        q.get("appid").map(String::as_str).unwrap_or_default(),
        None,
    ) {
        // WeChat answers 200 with an errcode on failure.
        Err(_) => axum::Json(json!({"errcode": 40029, "errmsg": "invalid code"})).into_response(),
        Ok(v) => {
            let p = idp.profile();
            let mut out = json!({"access_token": v["access_token"], "openid": "wx-openid-1", "scope": "snsapi_login"});
            if p.sub.starts_with("union:") {
                out["unionid"] = json!(p.sub);
            }
            axum::Json(out).into_response()
        }
    }
}

async fn wx_userinfo(State(idp): State<Arc<Idp>>, uri: Uri) -> Response {
    let q = query(&uri);
    if !q.get("access_token").is_some_and(|t| t.starts_with("at-code-")) {
        return axum::Json(json!({"errcode": 40001})).into_response();
    }
    let p = idp.profile();
    axum::Json(json!({"openid": q.get("openid"), "nickname": p.name})).into_response()
}
