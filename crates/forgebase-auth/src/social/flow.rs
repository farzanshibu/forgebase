//! The OAuth/OIDC login and linking flows.

use forgebase_data::AccessContext;
use serde::Serialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use super::DEFAULT_REALM;
use super::http_client::HttpRequest;
use super::idtoken::{self, Expect};
use super::registry::{Endpoints, Provider, ProviderConfig, ProviderInfo, ProviderKind};
use crate::crypto::{self, b64url};
use crate::service::{AuthService, ClientInfo, LoginOptions, LoginOutcome, audit_in, is_unique_violation};
use crate::store::{Tx, first, i, now, ot, read, rfc3339, rows, t};
use crate::{AuthError, AuthMethod, Authenticated, Subject};

/// Lifetime of an authorization request.
const STATE_TTL_SECS: i64 = 600;

/// Result of starting a flow.
#[derive(Debug, Clone, Serialize)]
pub struct OAuthStart {
    /// Where to send the user's browser.
    pub authorization_url: String,
    /// The `state` value (also inside the URL).
    pub state: String,
    /// Seconds the request stays valid.
    pub expires_in: i64,
}

/// What the provider sent back to the redirect URI.
#[derive(Debug, Clone, Default)]
pub struct OAuthCallback {
    /// Authorization code.
    pub code: Option<String>,
    /// State.
    pub state: Option<String>,
    /// Provider error code (`access_denied`, ...).
    pub error: Option<String>,
    /// Apple's `user` form field (JSON with the name, sent on the first
    /// authorization only). Client supplied: used for the display name,
    /// never for identity or email.
    pub user: Option<String>,
}

/// A linked provider account as shown to its owner.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityView {
    /// Identity id.
    pub id: String,
    /// Provider id.
    pub provider: String,
    /// Email the provider reported.
    pub email: Option<String>,
    /// Whether the provider vouched for it.
    pub email_verified: bool,
    /// Display name.
    pub display_name: Option<String>,
    /// Profile picture the provider reported (https URL).
    pub avatar_url: Option<String>,
    /// Linked at (RFC 3339).
    pub created_at: String,
    /// Last sign-in with it (RFC 3339).
    pub last_login_at: Option<String>,
}

/// Result of a callback.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum OAuthOutcome {
    /// A sign-in: a session or an MFA challenge.
    Login(LoginOutcome),
    /// A provider account was linked to the signed-in user.
    Linked(IdentityView),
}

/// A normalised provider profile.
#[derive(Debug, Clone, Default)]
pub(crate) struct Profile {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    pub(crate) email_verified: bool,
    pub(crate) name: Option<String>,
    pub(crate) picture: Option<String>,
}

/// An avatar URL worth keeping: `https`, no credentials, at most 2048 bytes.
fn clean_avatar(u: Option<&str>) -> Option<String> {
    let u = u?;
    let p = url::Url::parse(u).ok()?;
    (u.len() <= 2048
        && p.scheme() == "https"
        && p.host_str().is_some()
        && p.username().is_empty()
        && p.password().is_none())
    .then(|| u.to_owned())
}

/// A display name: control characters out, at most 100 characters, `None` if empty.
fn clean_name(n: &str) -> Option<String> {
    let n: String = n.chars().filter(|c| !c.is_control()).take(100).collect();
    let n = n.trim().to_owned();
    (!n.is_empty()).then_some(n)
}

/// The name in Apple's first-authorization `user` JSON
/// (`{"name": {"firstName": "..", "lastName": ".."}}`).
fn apple_user_name(user: Option<&str>) -> Option<String> {
    let v: Json = serde_json::from_str(user.filter(|u| u.len() <= 4096)?).ok()?;
    let part = |k: &str| v["name"][k].as_str().map(str::trim).filter(|s| !s.is_empty());
    clean_name(
        &[part("firstName"), part("lastName")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn is_truthy(v: Option<&Json>) -> bool {
    match v {
        Some(Json::Bool(b)) => *b,
        Some(Json::String(s)) => s == "true" || s == "1",
        Some(Json::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

fn json_id(v: Option<&Json>) -> Option<String> {
    v.and_then(|v| {
        v.as_i64()
            .map(|n| n.to_string())
            .or_else(|| v.as_str().map(str::to_owned))
    })
}

/// Basic credentials of RFC 6749 §2.3.1: both parts form-urlencoded.
fn client_basic(id: &str, secret: &str) -> String {
    use base64::Engine as _;
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", enc(id), enc(secret)))
    )
}

fn provider_err(m: &str) -> AuthError {
    AuthError::Provider(m.to_owned())
}

/// A state row, claimed.
struct StateRow {
    realm: String,
    purpose: String,
    nonce: String,
    verifier: Option<String>,
    redirect_uri: String,
    collection: String,
    link_user_id: Option<String>,
    options: LoginOptions,
}

impl AuthService {
    /// The provider registry (realm-keyed).
    pub fn providers(&self) -> &super::registry::ProviderRegistry {
        &self.social().registry
    }

    /// Replaces the outbound HTTP client (TLS-capable in production; the
    /// in-process mock IdP in tests).
    pub fn set_http_client(&self, client: std::sync::Arc<dyn super::HttpClient>) {
        *self.social().http.write().unwrap_or_else(|e| e.into_inner()) = client;
    }

    /// Loads `<data_dir>/auth/oauth-providers.json` when it exists
    /// (`{"providers": [...]}`); the file holds client secrets, keep it 0600.
    pub(crate) async fn load_provider_file(&self) -> Result<(), AuthError> {
        let Some(dir) = self.config().master_key_path.parent() else {
            return Ok(());
        };
        let path = dir.join("oauth-providers.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => {
                let n = self.providers().load_json(&text)?;
                tracing::info!(providers = n, "loaded identity providers");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::Internal(format!("reading oauth-providers.json: {e}"))),
        }
    }

    /// Providers a login page of realm `default` may offer.
    pub fn oauth_providers(&self) -> Vec<ProviderInfo> {
        self.providers().list(DEFAULT_REALM)
    }

    /// Providers a login page of `realm` may offer.
    pub fn oauth_providers_in(&self, realm: &str) -> Vec<ProviderInfo> {
        self.providers().list(realm)
    }

    fn provider_in(&self, realm: &str, id: &str) -> Result<std::sync::Arc<Provider>, AuthError> {
        self.providers()
            .get(realm, id)
            .ok_or_else(|| AuthError::NotFound("unknown identity provider".into()))
    }

    fn redirect_uri(&self, p: &Provider) -> String {
        p.config.redirect_uri.clone().unwrap_or_else(|| {
            format!(
                "{}/api/auth/oauth/{}/callback",
                self.config().public_url.trim_end_matches('/'),
                p.config.id
            )
        })
    }

    async fn endpoints(&self, p: &Provider) -> Result<Endpoints, AuthError> {
        p.endpoints
            .get_or_try_init(|| async {
                if let Some(e) = p.static_endpoints() {
                    return Ok(e);
                }
                // Generic OIDC with only an issuer: discovery.
                let issuer = p
                    .issuer()
                    .ok_or_else(|| provider_err("provider has no endpoints"))?
                    .trim_end_matches('/')
                    .to_owned();
                let res = self
                    .social()
                    .http()
                    .send(HttpRequest::get(format!("{issuer}/.well-known/openid-configuration")))
                    .await?;
                if res.status != 200 {
                    return Err(provider_err("OIDC discovery failed"));
                }
                let doc = res.json()?;
                let get = |k: &str| doc.get(k).and_then(Json::as_str).map(str::to_owned);
                if get("issuer").map(|i| i.trim_end_matches('/').to_owned()).as_deref() != Some(issuer.as_str()) {
                    return Err(provider_err("OIDC discovery issuer mismatch"));
                }
                let c = &p.config;
                Ok(Endpoints {
                    authorize: c
                        .authorize_url
                        .clone()
                        .or_else(|| get("authorization_endpoint"))
                        .ok_or_else(|| provider_err("discovery has no authorization_endpoint"))?,
                    token: c
                        .token_url
                        .clone()
                        .or_else(|| get("token_endpoint"))
                        .ok_or_else(|| provider_err("discovery has no token_endpoint"))?,
                    userinfo: c.userinfo_url.clone().or_else(|| get("userinfo_endpoint")),
                    jwks: Some(
                        c.jwks_url
                            .clone()
                            .or_else(|| get("jwks_uri"))
                            .ok_or_else(|| provider_err("discovery has no jwks_uri"))?,
                    ),
                    emails: None,
                    issuers: vec![issuer],
                    tenant: None,
                })
            })
            .await
            .cloned()
    }

    // -----------------------------------------------------------------------
    // Start
    // -----------------------------------------------------------------------

    /// Starts a sign-in with a provider.
    pub async fn oauth_start(
        &self,
        ci: &ClientInfo,
        provider: &str,
        collection: Option<&str>,
        opts: &LoginOptions,
    ) -> Result<OAuthStart, AuthError> {
        self.limit_attempt(ci)?;
        let (realm, coll) = self.request_collection(ci, collection).await?;
        realm.require(crate::realm::Method::Oauth)?;
        self.oauth_begin(&realm.realm, provider, "login", &coll.name, None, opts)
            .await
    }

    /// Starts linking a provider account to the signed-in user.
    pub async fn oauth_link_start(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        provider: &str,
    ) -> Result<OAuthStart, AuthError> {
        crate::lifecycle::not_impersonated(caller)?;
        self.limit_attempt(ci)?;
        let (collection, id) = Self::session_user(caller)?;
        let realm = self.realm_of(&collection)?;
        realm.require(crate::realm::Method::Oauth)?;
        self.oauth_begin(
            &realm.realm,
            provider,
            "link",
            &collection,
            Some(id),
            &LoginOptions::default(),
        )
        .await
    }

    fn session_user(caller: &Authenticated) -> Result<(String, String), AuthError> {
        match (&caller.subject, caller.session_id()) {
            (Subject::User { collection, id }, Some(_)) => Ok((collection.clone(), id.clone())),
            _ => Err(AuthError::Forbidden(
                "provider accounts are managed by signed-in users".into(),
            )),
        }
    }

    async fn oauth_begin(
        &self,
        realm: &str,
        provider: &str,
        purpose: &str,
        collection: &str,
        link_user: Option<String>,
        opts: &LoginOptions,
    ) -> Result<OAuthStart, AuthError> {
        let p = self.provider_in(realm, provider)?;
        let ep = self.endpoints(&p).await?;
        let state = crypto::random_token(32)?;
        let nonce = crypto::random_token(24)?;
        let verifier = p.uses_pkce().then(|| crypto::random_token(32)).transpose()?;
        let redirect_uri = self.redirect_uri(&p);
        let state_hash = self.keyring().token_hash("oauth_state", &state);
        let sealed = verifier
            .as_ref()
            .map(|v| self.keyring().seal(v.as_bytes(), state_hash.as_bytes()))
            .transpose()?;
        let ts = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_states WHERE expires_at < ?1",
            vec![i(ts - 3600)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_states
                (state_hash, realm, provider, purpose, nonce, verifier_enc, redirect_uri, collection, link_user_id,
                 options, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            vec![
                t(state_hash),
                t(realm),
                t(provider),
                t(purpose),
                t(nonce.clone()),
                sealed.map_or(forgebase_data::db::Value::Null, forgebase_data::db::Value::Blob),
                t(redirect_uri.clone()),
                t(collection),
                ot(link_user),
                t(serde_json::to_string(opts).map_err(|e| AuthError::Internal(e.to_string()))?),
                i(ts),
                i(ts + STATE_TTL_SECS),
            ],
        )
        .await?;
        tx.commit().await?;

        let mut url = url::Url::parse(&ep.authorize).map_err(|_| provider_err("bad authorization URL"))?;
        {
            let mut q = url.query_pairs_mut();
            let id_key = if p.config.kind == ProviderKind::Wechat {
                "appid"
            } else {
                "client_id"
            };
            q.append_pair(id_key, &p.config.client_id);
            q.append_pair("response_type", "code");
            q.append_pair("redirect_uri", &redirect_uri);
            q.append_pair("scope", &p.scopes().join(" "));
            q.append_pair("state", &state);
            if p.uses_id_token() {
                q.append_pair("nonce", &nonce);
            }
            if p.config.kind == ProviderKind::Apple {
                q.append_pair("response_mode", "form_post");
            }
            if let Some(v) = &verifier {
                q.append_pair("code_challenge", &b64url(&Sha256::digest(v.as_bytes())));
                q.append_pair("code_challenge_method", "S256");
            }
            for (k, v) in p.extra_authorize_params() {
                q.append_pair(&k, &v);
            }
        }
        if p.config.kind == ProviderKind::Wechat {
            url.set_fragment(Some("wechat_redirect"));
        }
        Ok(OAuthStart {
            authorization_url: url.to_string(),
            state,
            expires_in: STATE_TTL_SECS,
        })
    }

    // -----------------------------------------------------------------------
    // Callback
    // -----------------------------------------------------------------------

    /// Completes a flow from the provider's redirect.
    pub async fn oauth_callback(
        &self,
        ci: &ClientInfo,
        provider: &str,
        cb: &OAuthCallback,
    ) -> Result<OAuthOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let bad_state = AuthError::InvalidToken("invalid or expired authorization state");
        let state = cb
            .state
            .as_deref()
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or(bad_state)?;
        let st = self.oauth_claim_state(provider, state).await?;
        let p = self.provider_in(&st.realm, provider)?;
        let audit_fail = |reason: &str| {
            self.audit(
                "oauth.login",
                false,
                ci,
                "anonymous",
                None,
                json!({"provider": provider, "reason": reason}),
            )
        };
        if let Some(e) = &cb.error {
            // The provider's error code is a short token, safe to record.
            let code: String = e
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(40)
                .collect();
            audit_fail(&code).await?;
            return Err(AuthError::Forbidden("the identity provider refused the sign-in".into()));
        }
        let code = cb
            .code
            .as_deref()
            .filter(|c| !c.is_empty() && c.len() <= 2048)
            .ok_or_else(|| AuthError::invalid("missing authorization code"))?;
        let profile = match self.oauth_fetch_profile(&p, code, &st, cb.user.as_deref()).await {
            Ok(pr) => pr,
            Err(e) => {
                audit_fail("exchange_failed").await?;
                return Err(e);
            }
        };
        let r = self.oauth_resolve(ci, &p, &st, profile).await;
        if r.is_err() {
            audit_fail("resolve_failed").await?;
        }
        r
    }

    /// Atomically consumes a state row (single use).
    async fn oauth_claim_state(&self, provider: &str, state: &str) -> Result<StateRow, AuthError> {
        let bad = AuthError::InvalidToken("invalid or expired authorization state");
        let hash = self.keyring().token_hash("oauth_state", state);
        let r = read(
            self.data().backend(),
            "SELECT realm, provider, purpose, nonce, verifier_enc, redirect_uri, collection, link_user_id, options
             FROM _forgebase_oauth_states
             WHERE state_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
            vec![t(hash.clone()), i(now())],
        )
        .await?;
        let Some(row) = first(&r) else { return Err(bad) };
        let mut tx = Tx::begin(self.data().backend()).await?;
        let claimed = tx
            .exec(
                "UPDATE _forgebase_oauth_states SET used_at = ?2 WHERE state_hash = ?1 AND used_at IS NULL",
                vec![t(hash.clone()), i(now())],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        // A state is bound to the provider it was created for.
        if !claimed || row.str("provider") != provider {
            return Err(bad);
        }
        let verifier = {
            let b = row.blob("verifier_enc");
            if b.is_empty() {
                None
            } else {
                let plain = self.keyring().open(&b, hash.as_bytes())?;
                Some(String::from_utf8(plain).map_err(|_| AuthError::Internal("bad verifier".into()))?)
            }
        };
        Ok(StateRow {
            realm: row.str("realm"),
            purpose: row.str("purpose"),
            nonce: row.str("nonce"),
            verifier,
            redirect_uri: row.str("redirect_uri"),
            collection: row.str("collection"),
            link_user_id: row.opt_str("link_user_id"),
            options: serde_json::from_str(&row.str("options")).unwrap_or_default(),
        })
    }

    // -----------------------------------------------------------------------
    // Token exchange and profile
    // -----------------------------------------------------------------------

    async fn client_secret(&self, p: &Provider) -> Result<String, AuthError> {
        match (&p.config.apple, p.config.kind) {
            (Some(k), ProviderKind::Apple) => apple_client_secret(&p.config, k),
            _ => Ok(p.config.client_secret.expose().to_owned()),
        }
    }

    async fn oauth_fetch_profile(
        &self,
        p: &Provider,
        code: &str,
        st: &StateRow,
        user_hint: Option<&str>,
    ) -> Result<Profile, AuthError> {
        let ep = self.endpoints(p).await?;
        let http = self.social().http();
        let secret = self.client_secret(p).await?;
        let c = &p.config;

        let req = if c.kind == ProviderKind::Wechat {
            let mut u = url::Url::parse(&ep.token).map_err(|_| provider_err("bad token URL"))?;
            u.query_pairs_mut()
                .append_pair("appid", &c.client_id)
                .append_pair("secret", &secret)
                .append_pair("code", code)
                .append_pair("grant_type", "authorization_code");
            HttpRequest::get(u.to_string())
        } else {
            let mut f: Vec<(&str, &str)> = vec![
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &st.redirect_uri),
            ];
            // Bitbucket and Spotify authenticate the client with HTTP Basic;
            // the others take it in the form.
            if !p.token_auth_basic() {
                f.push(("client_id", &c.client_id));
                f.push(("client_secret", &secret));
            }
            if let Some(v) = &st.verifier {
                f.push(("code_verifier", v));
            }
            let mut r = HttpRequest::post_form(&ep.token, &f);
            if p.token_auth_basic() {
                r = r.header("authorization", client_basic(&c.client_id, &secret));
            }
            r
        };
        let res = http.send(req).await?;
        let tok = res.json()?;
        // WeChat answers 200 with an `errcode`; others use 4xx and `error`.
        if res.status != 200 || tok.get("error").is_some() || tok.get("errcode").is_some_and(|c| c != 0) {
            tracing::warn!(provider = %c.id, status = res.status, "token exchange refused");
            return Err(provider_err("the identity provider rejected the authorization code"));
        }
        let access = tok.get("access_token").and_then(Json::as_str).map(str::to_owned);
        let bearer = |url: &str, token: &str| HttpRequest::get(url).header("authorization", format!("Bearer {token}"));
        let need_access = || {
            access
                .clone()
                .ok_or_else(|| provider_err("provider returned no access token"))
        };

        match c.kind {
            ProviderKind::Google
            | ProviderKind::Apple
            | ProviderKind::Oidc
            | ProviderKind::Microsoft
            | ProviderKind::Gitlab
            | ProviderKind::Slack
            | ProviderKind::Linkedin
            | ProviderKind::Twitch
            | ProviderKind::Keycloak => {
                let id_token = tok
                    .get("id_token")
                    .and_then(Json::as_str)
                    .ok_or_else(|| provider_err("provider returned no ID token"))?;
                let jwks = ep.jwks.as_deref().ok_or_else(|| provider_err("provider has no JWKS"))?;
                let claims = idtoken::validate(
                    &*http,
                    &self.social().jwks,
                    jwks,
                    id_token,
                    &Expect {
                        issuers: &ep.issuers,
                        tenant: ep.tenant.as_ref(),
                        audience: &c.client_id,
                        nonce: &st.nonce,
                        now: now(),
                    },
                )
                .await?;
                let claim = |k: &str| claims.raw.get(k).and_then(Json::as_str).map(str::to_owned);
                let mut profile = Profile {
                    subject: claims.sub.clone(),
                    email: claims.email.clone(),
                    email_verified: claims.email_verified,
                    name: claims.name.clone(),
                    picture: claims.picture.clone(),
                };
                if profile.email.is_none()
                    && let (Some(u), Some(at)) = (ep.userinfo.as_deref(), access.as_deref())
                {
                    let ui = http.send(bearer(u, at)).await?;
                    if ui.status == 200 {
                        let j = ui.json()?;
                        if j.get("sub").and_then(Json::as_str) == Some(claims.sub.as_str()) {
                            profile.email = j.get("email").and_then(Json::as_str).map(str::to_owned);
                            profile.email_verified = matches!(j.get("email_verified"), Some(Json::Bool(true)));
                            profile.name = profile
                                .name
                                .or_else(|| j.get("name").and_then(Json::as_str).map(str::to_owned));
                            profile.picture = profile
                                .picture
                                .or_else(|| j.get("picture").and_then(Json::as_str).map(str::to_owned));
                        }
                    }
                }
                match c.kind {
                    ProviderKind::Apple => {
                        // Apple never puts the name in the ID token; it sends
                        // it once, in the `user` form field of the first
                        // authorization. A relay address (`is_private_email`)
                        // is a real, verified mailbox that forwards.
                        profile.name = profile.name.or_else(|| apple_user_name(user_hint));
                    }
                    ProviderKind::Microsoft => {
                        // The `email` claim is whatever the account holder
                        // typed: it is not proof of a mailbox (the "nOAuth"
                        // account takeover). Only Entra's optional
                        // `xms_edov` claim (the email's domain is owned by
                        // the tenant) counts, and the stable identity is
                        // tenant + object id, never the email.
                        profile.email_verified = profile.email.is_some() && is_truthy(claims.raw.get("xms_edov"));
                        if let (Some(tid), Some(oid)) = (claim("tid"), claim("oid")) {
                            profile.subject = format!("{}:{}", tid.to_ascii_lowercase(), oid.to_ascii_lowercase());
                        }
                    }
                    ProviderKind::Twitch => {
                        profile.name = profile.name.or_else(|| claim("preferred_username"));
                    }
                    ProviderKind::Gitlab => {
                        profile.name = profile
                            .name
                            .or_else(|| claim("nickname"))
                            .or_else(|| claim("preferred_username"));
                    }
                    _ => {}
                }
                profile.name = profile.name.as_deref().and_then(clean_name);
                profile.picture = clean_avatar(profile.picture.as_deref());
                Ok(profile)
            }
            ProviderKind::Github => {
                let at = need_access()?;
                let u = ep.userinfo.as_deref().ok_or_else(|| provider_err("no user endpoint"))?;
                let r = http.send(bearer(u, &at)).await?;
                if r.status != 200 {
                    return Err(provider_err("could not read the GitHub profile"));
                }
                let j = r.json()?;
                let id = j
                    .get("id")
                    .and_then(|v| {
                        v.as_i64()
                            .map(|n| n.to_string())
                            .or_else(|| v.as_str().map(str::to_owned))
                    })
                    .ok_or_else(|| provider_err("GitHub profile has no id"))?;
                // Only a primary, verified address counts; the profile's own
                // `email` field carries no verification flag.
                let mut email = None;
                if let Some(eu) = ep.emails.as_deref() {
                    let er = http.send(bearer(eu, &at)).await?;
                    if er.status == 200
                        && let Some(list) = er.json()?.as_array()
                    {
                        email = list
                            .iter()
                            .find(|e| e["primary"] == true && e["verified"] == true)
                            .and_then(|e| e["email"].as_str())
                            .map(str::to_owned);
                    }
                }
                Ok(Profile {
                    subject: id,
                    email_verified: email.is_some(),
                    email,
                    name: j
                        .get("name")
                        .or_else(|| j.get("login"))
                        .and_then(Json::as_str)
                        .and_then(clean_name),
                    picture: clean_avatar(j.get("avatar_url").and_then(Json::as_str)),
                })
            }
            ProviderKind::Facebook => {
                let at = need_access()?;
                let u = ep.userinfo.as_deref().ok_or_else(|| provider_err("no user endpoint"))?;
                let mut url = url::Url::parse(u).map_err(|_| provider_err("bad user URL"))?;
                url.query_pairs_mut().append_pair("fields", "id,name,email,picture");
                let r = http.send(bearer(url.as_str(), &at)).await?;
                if r.status != 200 {
                    return Err(provider_err("could not read the Facebook profile"));
                }
                let j = r.json()?;
                Ok(Profile {
                    subject: j
                        .get("id")
                        .and_then(Json::as_str)
                        .ok_or_else(|| provider_err("Facebook profile has no id"))?
                        .to_owned(),
                    email: j.get("email").and_then(Json::as_str).map(str::to_owned),
                    // Facebook does not say whether an address is verified.
                    email_verified: false,
                    name: j.get("name").and_then(Json::as_str).and_then(clean_name),
                    picture: clean_avatar(j["picture"]["data"]["url"].as_str()),
                })
            }
            ProviderKind::Discord => {
                let at = need_access()?;
                let u = ep.userinfo.as_deref().ok_or_else(|| provider_err("no user endpoint"))?;
                let r = http.send(bearer(u, &at)).await?;
                if r.status != 200 {
                    return Err(provider_err("could not read the Discord profile"));
                }
                let j = r.json()?;
                let id = j
                    .get("id")
                    .and_then(Json::as_str)
                    .ok_or_else(|| provider_err("Discord profile has no id"))?
                    .to_owned();
                // Avatars are a hash on Discord's CDN.
                let picture = j.get("avatar").and_then(Json::as_str).and_then(|h| {
                    (h.len() <= 64 && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
                        .then(|| format!("https://cdn.discordapp.com/avatars/{id}/{h}.png"))
                });
                Ok(Profile {
                    subject: id,
                    email: j.get("email").and_then(Json::as_str).map(str::to_owned),
                    email_verified: j.get("verified") == Some(&Json::Bool(true)),
                    name: j
                        .get("global_name")
                        .or_else(|| j.get("username"))
                        .and_then(Json::as_str)
                        .and_then(clean_name),
                    picture,
                })
            }
            ProviderKind::Bitbucket => {
                let at = need_access()?;
                let u = ep.userinfo.as_deref().ok_or_else(|| provider_err("no user endpoint"))?;
                let r = http.send(bearer(u, &at)).await?;
                if r.status != 200 {
                    return Err(provider_err("could not read the Bitbucket profile"));
                }
                let j = r.json()?;
                let subject = j
                    .get("uuid")
                    .and_then(Json::as_str)
                    .ok_or_else(|| provider_err("Bitbucket profile has no uuid"))?
                    .to_owned();
                // Only the primary address Bitbucket itself confirmed counts.
                let mut email = None;
                if let Some(eu) = ep.emails.as_deref() {
                    let er = http.send(bearer(eu, &at)).await?;
                    if er.status == 200 {
                        email = er.json()?["values"]
                            .as_array()
                            .and_then(|l| l.iter().find(|e| e["is_primary"] == true && e["is_confirmed"] == true))
                            .and_then(|e| e["email"].as_str())
                            .map(str::to_owned);
                    }
                }
                Ok(Profile {
                    subject,
                    email_verified: email.is_some(),
                    email,
                    name: j
                        .get("display_name")
                        .or_else(|| j.get("nickname"))
                        .and_then(Json::as_str)
                        .and_then(clean_name),
                    picture: clean_avatar(j["links"]["avatar"]["href"].as_str()),
                })
            }
            ProviderKind::Spotify => {
                let at = need_access()?;
                let u = ep.userinfo.as_deref().ok_or_else(|| provider_err("no user endpoint"))?;
                let r = http.send(bearer(u, &at)).await?;
                if r.status != 200 {
                    return Err(provider_err("could not read the Spotify profile"));
                }
                let j = r.json()?;
                Ok(Profile {
                    subject: json_id(j.get("id")).ok_or_else(|| provider_err("Spotify profile has no id"))?,
                    email: j.get("email").and_then(Json::as_str).map(str::to_owned),
                    // Spotify does not confirm the address it reports.
                    email_verified: false,
                    name: j.get("display_name").and_then(Json::as_str).and_then(clean_name),
                    picture: clean_avatar(j["images"][0]["url"].as_str()),
                })
            }
            ProviderKind::Wechat => {
                let at = need_access()?;
                let openid = tok
                    .get("openid")
                    .and_then(Json::as_str)
                    .ok_or_else(|| provider_err("WeChat returned no openid"))?;
                let mut unionid = tok.get("unionid").and_then(Json::as_str).map(str::to_owned);
                let mut name = None;
                if let Some(u) = ep.userinfo.as_deref() {
                    let mut url = url::Url::parse(u).map_err(|_| provider_err("bad user URL"))?;
                    url.query_pairs_mut()
                        .append_pair("access_token", &at)
                        .append_pair("openid", openid);
                    let r = http.send(HttpRequest::get(url.to_string())).await?;
                    if r.status == 200 {
                        let j = r.json()?;
                        if j.get("errcode").is_none_or(|c| c == 0) {
                            unionid = unionid.or_else(|| j.get("unionid").and_then(Json::as_str).map(str::to_owned));
                            name = j.get("nickname").and_then(Json::as_str).map(str::to_owned);
                        }
                    }
                }
                // `unionid` is stable across a company's apps; `openid` only
                // within one app. Prefer the former when WeChat provides it.
                Ok(Profile {
                    subject: unionid.unwrap_or_else(|| openid.to_owned()),
                    email: None,
                    email_verified: false,
                    name: name.as_deref().and_then(clean_name),
                    picture: None,
                })
            }
        }
    }

    // -----------------------------------------------------------------------
    // Account resolution
    // -----------------------------------------------------------------------

    pub(crate) async fn identity_by_subject(
        &self,
        realm: &str,
        provider: &str,
        subject: &str,
    ) -> Result<Option<(String, String, String)>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT id, collection, user_id FROM _forgebase_oauth_identities
             WHERE realm = ?1 AND provider = ?2 AND provider_subject = ?3",
            vec![t(realm), t(provider), t(subject)],
        )
        .await?;
        Ok(first(&r).map(|row| (row.str("id"), row.str("collection"), row.str("user_id"))))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn insert_identity(
        &self,
        ci: &ClientInfo,
        realm: &str,
        provider: &str,
        profile: &Profile,
        collection: &str,
        user_id: &str,
        how: &str,
    ) -> Result<String, AuthError> {
        let id = crypto::random_id(20)?;
        let subject = Subject::User {
            collection: collection.to_owned(),
            id: user_id.to_owned(),
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_identities
                (id, realm, provider, provider_subject, collection, user_id, email, email_verified, display_name, created_at,
                 avatar_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            vec![
                t(id.clone()),
                t(realm),
                t(provider),
                t(profile.subject.clone()),
                t(collection),
                t(user_id),
                ot(profile.email.clone()),
                i(i64::from(profile.email_verified)),
                ot(profile.name.clone()),
                i(now()),
                ot(profile.picture.clone()),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.linked",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({"provider": provider, "identity_id": id, "how": how}),
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn oauth_resolve(
        &self,
        ci: &ClientInfo,
        p: &Provider,
        st: &StateRow,
        mut profile: Profile,
    ) -> Result<OAuthOutcome, AuthError> {
        let provider = p.config.id.as_str();
        profile.email = profile
            .email
            .as_deref()
            .and_then(|e| crate::service::normalize_email(e).ok());
        if profile.email.is_none() {
            profile.email_verified = false;
        }
        let existing = self.identity_by_subject(&st.realm, provider, &profile.subject).await?;

        if st.purpose == "link" {
            let user_id = st
                .link_user_id
                .clone()
                .ok_or_else(|| AuthError::Internal("link state without a user".into()))?;
            return match existing {
                Some((id, coll, uid)) if coll == st.collection && uid == user_id => {
                    Ok(OAuthOutcome::Linked(self.identity_view(&id).await?))
                }
                Some(_) => Err(AuthError::Conflict(
                    "this provider account is already linked to another user".into(),
                )),
                None => {
                    if !self
                        .subject_exists(&Subject::User {
                            collection: st.collection.clone(),
                            id: user_id.clone(),
                        })
                        .await?
                    {
                        return Err(AuthError::Unauthenticated);
                    }
                    let id = self
                        .insert_identity(ci, &st.realm, provider, &profile, &st.collection, &user_id, "explicit")
                        .await
                        .map_err(|e| {
                            if is_unique_violation(&e) {
                                AuthError::Conflict("this provider account is already linked to another user".into())
                            } else {
                                e
                            }
                        })?;
                    Ok(OAuthOutcome::Linked(self.identity_view(&id).await?))
                }
            };
        }

        // Sign-in.
        let (collection, user_id) = if let Some((identity_id, coll, uid)) = existing {
            let subject = Subject::User {
                collection: coll.clone(),
                id: uid.clone(),
            };
            if !self.subject_exists(&subject).await? {
                return Err(AuthError::InvalidCredentials);
            }
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                "UPDATE _forgebase_oauth_identities SET last_login_at = ?2, email = COALESCE(?3, email),
                    email_verified = CASE WHEN CAST(?3 AS TEXT) IS NULL THEN email_verified ELSE ?4 END,
                    avatar_url = COALESCE(?5, avatar_url) WHERE id = ?1",
                vec![
                    t(identity_id),
                    i(now()),
                    ot(profile.email.clone()),
                    i(i64::from(profile.email_verified)),
                    ot(profile.picture.clone()),
                ],
            )
            .await?;
            tx.commit().await?;
            (coll, uid)
        } else {
            self.oauth_first_login(ci, p, st, &profile).await?
        };
        let subject = Subject::User {
            collection,
            id: user_id,
        };
        let out = self
            .complete_first_factor(&subject, AuthMethod::Oauth, ci, &st.options)
            .await?;
        Ok(OAuthOutcome::Login(out))
    }

    /// First sign-in of a provider account: link by verified email or sign up.
    async fn oauth_first_login(
        &self,
        ci: &ClientInfo,
        p: &Provider,
        st: &StateRow,
        profile: &Profile,
    ) -> Result<(String, String), AuthError> {
        let realm = self.realm_identity(&st.realm)?;
        let coll = self.auth_collection(Some(&st.collection)).await?;
        self.external_first_login(
            ci,
            &realm,
            &p.config.id,
            p.config.auto_link_verified_email,
            p.config.allow_signup,
            &coll,
            profile,
            "oauth.invalid",
        )
        .await
    }

    /// First sign-in of an external account (provider, LDAP): link to a
    /// local account with the same email only when both sides proved it and
    /// `auto_link` is on, otherwise `409`; else sign up when allowed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn external_first_login(
        &self,
        ci: &ClientInfo,
        realm: &crate::realm::RealmIdentity,
        provider: &str,
        auto_link: bool,
        allow_signup: bool,
        coll: &forgebase_data::schema::Collection,
        profile: &Profile,
        placeholder_domain: &str,
    ) -> Result<(String, String), AuthError> {
        let event = if provider == "ldap" {
            "ldap.login"
        } else {
            "oauth.login"
        };
        if let Some(email) = &profile.email
            && let Some(user) = self.find_user(coll, email).await?
        {
            // Never merge accounts on an address nobody proved to us. Both
            // sides must have proved it: the provider (verified flag) and the
            // local account (verified), else a pre-registered, unverified
            // account could be taken over through the provider.
            let allowed = auto_link && profile.email_verified && user.verified;
            if !allowed {
                self.audit(
                    event,
                    false,
                    ci,
                    "anonymous",
                    None,
                    json!({"provider": provider, "reason": "email_conflict"}),
                )
                .await?;
                return Err(AuthError::Conflict(
                    "an account with this email already exists; sign in and link this provider from your account"
                        .into(),
                ));
            }
            self.insert_identity(
                ci,
                &realm.realm,
                provider,
                profile,
                &coll.name,
                &user.id,
                "verified_email",
            )
            .await?;
            return Ok((coll.name.clone(), user.id));
        }
        if !allow_signup || !realm.config.signup {
            return Err(AuthError::Forbidden("sign-up with this provider is disabled".into()));
        }
        // Only a verified address becomes the account's email; otherwise a
        // reserved placeholder, so nobody can squat someone else's address.
        let (email, verified) = match (&profile.email, profile.email_verified) {
            (Some(e), true) => (e.clone(), true),
            _ => {
                let key = if realm.realm == DEFAULT_REALM {
                    format!("{provider}:{}", profile.subject)
                } else {
                    format!("{}:{provider}:{}", realm.realm, profile.subject)
                };
                let digest = Sha256::digest(key.as_bytes());
                (
                    format!("{provider}.{}@{placeholder_domain}", &crypto::hex(&digest)[..24]),
                    false,
                )
            }
        };
        let password = crypto::random_token(32)?;
        self.hasher().prehash(&password).await?;
        let doc = json!({
            "email": email,
            "password": password,
            "verified": verified,
            "token_key": crypto::random_token(24)?,
        });
        let rec = self
            .data()
            .create_record(&AccessContext::system(), &coll.name, doc)
            .await
            .map_err(|e| {
                let e = AuthError::from(e);
                if is_unique_violation(&e) {
                    AuthError::EmailTaken
                } else {
                    e
                }
            })?;
        let uid = rec.get("id").and_then(Json::as_str).unwrap_or_default().to_owned();
        match self
            .insert_identity(ci, &realm.realm, provider, profile, &coll.name, &uid, "signup")
            .await
        {
            Ok(_) => {}
            Err(e) => {
                // Lost a race for the same provider account: drop our orphan
                // user and sign in as the winner.
                let _ = self
                    .data()
                    .delete_record(&AccessContext::system(), &coll.name, &uid, None)
                    .await;
                if is_unique_violation(&e)
                    && let Some((_, c, u)) = self
                        .identity_by_subject(&realm.realm, provider, &profile.subject)
                        .await?
                {
                    return Ok((c, u));
                }
                return Err(e);
            }
        }
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: uid.clone(),
        };
        self.audit(
            "signup",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({"collection": coll.name, "provider": provider}),
        )
        .await?;
        Ok((coll.name.clone(), uid))
    }

    // -----------------------------------------------------------------------
    // Linked identities
    // -----------------------------------------------------------------------

    async fn identity_view(&self, id: &str) -> Result<IdentityView, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT id, provider, email, email_verified, display_name, avatar_url, created_at, last_login_at
             FROM _forgebase_oauth_identities WHERE id = ?1",
            vec![t(id)],
        )
        .await?;
        first(&r)
            .map(identity_row)
            .ok_or_else(|| AuthError::NotFound("identity not found".into()))
    }

    /// The caller's linked provider accounts.
    pub async fn oauth_identities(&self, caller: &Authenticated) -> Result<Vec<IdentityView>, AuthError> {
        let (coll, id) = Self::session_user(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT id, provider, email, email_verified, display_name, avatar_url, created_at, last_login_at
             FROM _forgebase_oauth_identities WHERE collection = ?1 AND user_id = ?2 ORDER BY created_at",
            vec![t(coll), t(id)],
        )
        .await?;
        Ok(rows(&r).map(identity_row).collect())
    }

    /// Whether removing one identity or passkey would leave an account with
    /// no way back in: a placeholder email (no mailbox to reset a password
    /// at), and nothing else linked.
    pub(crate) async fn sole_way_in(
        &self,
        coll: &str,
        uid: &str,
        skip_identity: Option<&str>,
        skip_credential: Option<&str>,
        skip_phone: bool,
    ) -> Result<bool, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT
               (SELECT COUNT(*) FROM _forgebase_oauth_identities WHERE collection = ?1 AND user_id = ?2 AND id <> ?3) AS ids,
               (SELECT COUNT(*) FROM _forgebase_webauthn_credentials WHERE collection = ?1 AND user_id = ?2 AND credential_id <> ?4) AS keys,
               (SELECT COUNT(*) FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2) AS phones",
            vec![t(coll), t(uid), t(skip_identity.unwrap_or("")), t(skip_credential.unwrap_or(""))],
        )
        .await?;
        let (ids, keys, phones) = first(&r).map_or((0, 0, 0), |r| (r.i64("ids"), r.i64("keys"), r.i64("phones")));
        let phones = if skip_phone { 0 } else { phones };
        let user = self.user_view(coll, uid).await?;
        // Placeholder addresses (`@oauth.invalid`, `@ldap.invalid`,
        // `@phone.invalid`) have no mailbox to reset a password at.
        let placeholder = user
            .get("email")
            .and_then(Json::as_str)
            .is_some_and(|e| e.ends_with(".invalid"));
        Ok(placeholder && ids == 0 && keys == 0 && phones == 0)
    }

    /// Unlinks a provider account. Refused when it would leave an account
    /// with no way back in (a placeholder email, no other identity, no passkey).
    pub async fn oauth_unlink(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        identity_id: &str,
    ) -> Result<(), AuthError> {
        crate::lifecycle::not_impersonated(caller)?;
        let (coll, uid) = Self::session_user(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT provider FROM _forgebase_oauth_identities WHERE id = ?1 AND collection = ?2 AND user_id = ?3",
            vec![t(identity_id), t(coll.clone()), t(uid.clone())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Err(AuthError::NotFound("identity not found".into()));
        };
        let provider = row.str("provider");
        if self.sole_way_in(&coll, &uid, Some(identity_id), None, false).await? {
            return Err(AuthError::Conflict(
                "this is the only way to sign in to the account; add a passkey or another provider first".into(),
            ));
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_identities WHERE id = ?1",
            vec![t(identity_id)],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.unlinked",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"provider": provider, "identity_id": identity_id}),
        )
        .await?;
        tx.commit().await
    }
}

fn identity_row(row: crate::store::Row<'_>) -> IdentityView {
    IdentityView {
        id: row.str("id"),
        provider: row.str("provider"),
        email: row.opt_str("email"),
        email_verified: row.i64("email_verified") == 1,
        display_name: row.opt_str("display_name"),
        avatar_url: row.opt_str("avatar_url"),
        created_at: rfc3339(row.i64("created_at")),
        last_login_at: row.opt_i64("last_login_at").map(rfc3339),
    }
}

/// Apple's client secret: an ES256 JWT signed with the team's key.
fn apple_client_secret(c: &ProviderConfig, k: &super::registry::AppleKey) -> Result<String, AuthError> {
    use p256::ecdsa::signature::Signer;
    use p256::pkcs8::DecodePrivateKey;
    let key = p256::ecdsa::SigningKey::from_pkcs8_pem(k.private_key_pem.expose())
        .map_err(|_| AuthError::Internal("invalid Apple signing key".into()))?;
    let ts = now();
    let header = json!({"alg": "ES256", "kid": k.key_id});
    let claims = json!({
        "iss": k.team_id, "iat": ts, "exp": ts + 300,
        "aud": "https://appleid.apple.com", "sub": c.client_id,
    });
    let enc = |v: &Json| -> Result<String, AuthError> {
        Ok(b64url(
            &serde_json::to_vec(v).map_err(|e| AuthError::Internal(e.to_string()))?,
        ))
    };
    let input = format!("{}.{}", enc(&header)?, enc(&claims)?);
    let sig: p256::ecdsa::Signature = key.sign(input.as_bytes());
    Ok(format!("{input}.{}", b64url(&sig.to_bytes())))
}
