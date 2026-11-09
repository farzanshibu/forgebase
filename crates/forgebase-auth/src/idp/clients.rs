//! Client Apps: the applications that sign users in through ForgeBase.
//!
//! A client belongs to one realm and is either **confidential** (a server
//! that keeps a secret) or **public** (a browser or native app; no secret,
//! PKCE required). Redirect URIs are an allow list compared byte for byte.
//! The secret is shown once, stored as HMAC-SHA256 under the pepper, and can
//! be rotated with an optional grace period for the previous secret.

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use super::{GRANT_CLIENT_CREDENTIALS, GRANT_CODE, GRANT_DEVICE, GRANT_REFRESH, GRANT_TYPES, OAuthError, scope_known};
use crate::crypto;
use crate::service::{ClientInfo, audit_in, require_admin};
use crate::store::{Row, Tx, first, i, ot, read, rfc3339, rows, t};
use crate::{AuthError, AuthService, Authenticated};

/// Confidential or public client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    /// Keeps a secret (server-side application).
    #[default]
    Confidential,
    /// Cannot keep a secret (single-page or native app): PKCE `S256` required.
    Public,
}

impl ClientType {
    fn as_str(self) -> &'static str {
        match self {
            ClientType::Confidential => "confidential",
            ClientType::Public => "public",
        }
    }
}

/// A new Client App.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientInput {
    /// Realm (default: the request's realm, else `default`).
    #[serde(default)]
    pub realm: Option<String>,
    /// Display name (consent screen).
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// `confidential` (default) or `public`.
    #[serde(default, rename = "type")]
    pub client_type: ClientType,
    /// Allowed redirect URIs (exact match).
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Allowed `post_logout_redirect_uri` values (exact match).
    #[serde(default)]
    pub post_logout_redirect_uris: Vec<String>,
    /// Allowed grant types (default `authorization_code` + `refresh_token`).
    #[serde(default)]
    pub grant_types: Option<Vec<String>>,
    /// Scopes the client may request (default `openid profile email offline_access`).
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// First-party app: no consent screen.
    #[serde(default)]
    pub skip_consent: bool,
    /// Access-token lifetime in seconds (60 to 86400).
    #[serde(default)]
    pub access_token_ttl: Option<i64>,
    /// Refresh-token family lifetime in seconds (300 to 31536000).
    #[serde(default)]
    pub refresh_token_ttl: Option<i64>,
}

/// Changes to a Client App (absent fields stay).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientPatch {
    /// Display name.
    pub name: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Redirect URIs.
    pub redirect_uris: Option<Vec<String>>,
    /// Post-logout redirect URIs.
    pub post_logout_redirect_uris: Option<Vec<String>>,
    /// Grant types.
    pub grant_types: Option<Vec<String>>,
    /// Scopes.
    pub scopes: Option<Vec<String>>,
    /// Skip the consent screen.
    pub skip_consent: Option<bool>,
    /// Access-token lifetime (seconds).
    pub access_token_ttl: Option<i64>,
    /// Refresh-token lifetime (seconds).
    pub refresh_token_ttl: Option<i64>,
    /// Disable (no new authorizations or tokens) or re-enable.
    pub disabled: Option<bool>,
}

/// A Client App as administrators see it (never the secret).
#[derive(Debug, Clone, Serialize)]
pub struct ClientView {
    /// `client_id`.
    pub client_id: String,
    /// Realm.
    pub realm: String,
    /// Name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// `confidential` or `public`.
    #[serde(rename = "type")]
    pub client_type: ClientType,
    /// Redirect URIs.
    pub redirect_uris: Vec<String>,
    /// Post-logout redirect URIs.
    pub post_logout_redirect_uris: Vec<String>,
    /// Grant types.
    pub grant_types: Vec<String>,
    /// Scopes.
    pub scopes: Vec<String>,
    /// No consent screen.
    pub skip_consent: bool,
    /// Access-token lifetime override.
    pub access_token_ttl: Option<i64>,
    /// Refresh-token lifetime override.
    pub refresh_token_ttl: Option<i64>,
    /// Disabled.
    pub disabled: bool,
    /// When the current secret was issued.
    pub secret_created_at: Option<String>,
    /// Until when the previous secret is still accepted.
    pub previous_secret_expires_at: Option<String>,
    /// Created.
    pub created_at: String,
    /// Updated.
    pub updated_at: String,
}

/// A created client, or a rotated secret: the secret is shown this once.
#[derive(Debug, Clone, Serialize)]
pub struct CreatedClient {
    /// The client.
    pub client: ClientView,
    /// The client secret (confidential clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// A client row with its secret hashes (crate-internal).
#[derive(Debug, Clone)]
pub(crate) struct ClientRecord {
    pub(crate) id: String,
    pub(crate) realm: String,
    pub(crate) name: String,
    pub(crate) client_type: ClientType,
    secret_hash: Option<String>,
    previous_secret_hash: Option<String>,
    previous_secret_expires_at: Option<i64>,
    pub(crate) redirect_uris: Vec<String>,
    pub(crate) post_logout_redirect_uris: Vec<String>,
    pub(crate) grant_types: Vec<String>,
    pub(crate) scopes: Vec<String>,
    pub(crate) skip_consent: bool,
    pub(crate) access_token_ttl: Option<i64>,
    pub(crate) refresh_token_ttl: Option<i64>,
    pub(crate) disabled: bool,
    view: ClientView,
}

impl ClientRecord {
    pub(crate) fn allows_grant(&self, g: &str) -> bool {
        self.grant_types.iter().any(|x| x == g)
    }

    pub(crate) fn is_public(&self) -> bool {
        self.client_type == ClientType::Public
    }
}

/// Client credentials presented at the token, introspection, revocation or
/// device-authorization endpoint.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClientCreds {
    pub(crate) id: Option<String>,
    pub(crate) secret: Option<String>,
}

const CLIENT_COLS: &str =
    "id, realm, name, description, client_type, secret_hash, secret_created_at, previous_secret_hash,
    previous_secret_expires_at, redirect_uris, post_logout_redirect_uris, grant_types, scopes, skip_consent,
    access_token_ttl, refresh_token_ttl, disabled, created_at, updated_at";

fn json_list(row: &Row<'_>, col: &str) -> Vec<String> {
    serde_json::from_str(&row.str(col)).unwrap_or_default()
}

fn record(row: Row<'_>) -> ClientRecord {
    let client_type = if row.str("client_type") == "public" {
        ClientType::Public
    } else {
        ClientType::Confidential
    };
    let view = ClientView {
        client_id: row.str("id"),
        realm: row.str("realm"),
        name: row.str("name"),
        description: row.opt_str("description"),
        client_type,
        redirect_uris: json_list(&row, "redirect_uris"),
        post_logout_redirect_uris: json_list(&row, "post_logout_redirect_uris"),
        grant_types: json_list(&row, "grant_types"),
        scopes: json_list(&row, "scopes"),
        skip_consent: row.i64("skip_consent") == 1,
        access_token_ttl: row.opt_i64("access_token_ttl"),
        refresh_token_ttl: row.opt_i64("refresh_token_ttl"),
        disabled: row.i64("disabled") == 1,
        secret_created_at: row.opt_i64("secret_created_at").map(rfc3339),
        previous_secret_expires_at: row.opt_i64("previous_secret_expires_at").map(rfc3339),
        created_at: rfc3339(row.i64("created_at")),
        updated_at: rfc3339(row.i64("updated_at")),
    };
    ClientRecord {
        id: view.client_id.clone(),
        realm: view.realm.clone(),
        name: view.name.clone(),
        client_type,
        secret_hash: row.opt_str("secret_hash"),
        previous_secret_hash: row.opt_str("previous_secret_hash"),
        previous_secret_expires_at: row.opt_i64("previous_secret_expires_at"),
        redirect_uris: view.redirect_uris.clone(),
        post_logout_redirect_uris: view.post_logout_redirect_uris.clone(),
        grant_types: view.grant_types.clone(),
        scopes: view.scopes.clone(),
        skip_consent: view.skip_consent,
        access_token_ttl: view.access_token_ttl,
        refresh_token_ttl: view.refresh_token_ttl,
        disabled: view.disabled,
        view,
    }
}

/// Checks a redirect URI for registration: absolute, no fragment, no user
/// info; `https`, or `http` on a loopback host, or (public clients) a private
/// reverse-domain scheme such as `com.example.app:/callback` (RFC 8252).
pub(crate) fn check_redirect_uri(u: &str, public: bool) -> Result<(), AuthError> {
    let bad = |why: &str| AuthError::invalid(format!("redirect URI {u:?}: {why}"));
    if u.is_empty() || u.len() > 2000 {
        return Err(bad("must be 1 to 2000 characters"));
    }
    if u.contains('#') {
        return Err(bad("must not contain a fragment"));
    }
    if u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("must not contain whitespace"));
    }
    let url = url::Url::parse(u).map_err(|_| bad("must be an absolute URI"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("must not contain user information"));
    }
    match url.scheme() {
        "https" if url.host_str().is_some_and(|h| !h.is_empty()) => Ok(()),
        "http" if matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) => Ok(()),
        "http" => Err(bad("plain http is allowed for loopback hosts only")),
        s if public && s.contains('.') => Ok(()),
        _ => Err(bad(
            "scheme must be https (or a private reverse-domain scheme for public clients)",
        )),
    }
}

struct Settings {
    redirect_uris: Vec<String>,
    post_logout_redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    scopes: Vec<String>,
    access_token_ttl: Option<i64>,
    refresh_token_ttl: Option<i64>,
}

fn check_settings(public: bool, s: &Settings) -> Result<(), AuthError> {
    if s.redirect_uris.len() > 20 || s.post_logout_redirect_uris.len() > 20 {
        return Err(AuthError::invalid("at most 20 redirect URIs"));
    }
    for u in s.redirect_uris.iter().chain(&s.post_logout_redirect_uris) {
        check_redirect_uri(u, public)?;
    }
    if s.grant_types.is_empty() {
        return Err(AuthError::invalid("grant_types must not be empty"));
    }
    for g in &s.grant_types {
        if !GRANT_TYPES.contains(&g.as_str()) {
            return Err(AuthError::invalid(format!("unsupported grant type {g:?}")));
        }
    }
    let has = |g: &str| s.grant_types.iter().any(|x| x == g);
    if public && has(GRANT_CLIENT_CREDENTIALS) {
        return Err(AuthError::invalid("public clients cannot use client_credentials"));
    }
    if has(GRANT_CODE) && s.redirect_uris.is_empty() {
        return Err(AuthError::invalid("authorization_code needs at least one redirect URI"));
    }
    if has(GRANT_REFRESH) && !has(GRANT_CODE) && !has(GRANT_DEVICE) {
        return Err(AuthError::invalid(
            "refresh_token needs authorization_code or the device grant",
        ));
    }
    if s.scopes.len() > 100 {
        return Err(AuthError::invalid("at most 100 scopes"));
    }
    for sc in &s.scopes {
        if !scope_known(sc) {
            return Err(AuthError::invalid(format!(
                "unknown scope {sc:?}: use openid, profile, email, phone, offline_access or a ForgeBase API scope"
            )));
        }
    }
    if s.access_token_ttl.is_some_and(|v| !(60..=86_400).contains(&v)) {
        return Err(AuthError::invalid("access_token_ttl must be 60 to 86400 seconds"));
    }
    if s.refresh_token_ttl.is_some_and(|v| !(300..=31_536_000).contains(&v)) {
        return Err(AuthError::invalid("refresh_token_ttl must be 300 to 31536000 seconds"));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<String, AuthError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 200 {
        return Err(AuthError::invalid("name must be 1 to 200 characters"));
    }
    Ok(n.to_owned())
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for s in v {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn to_json(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn new_secret() -> Result<String, AuthError> {
    Ok(format!("fbcs_{}", crypto::random_token(32)?))
}

impl AuthService {
    pub(crate) async fn load_client(&self, id: &str) -> Result<Option<ClientRecord>, AuthError> {
        if id.is_empty() || id.len() > 64 {
            return Ok(None);
        }
        let r = read(
            self.data().backend(),
            &format!("SELECT {CLIENT_COLS} FROM _forgebase_oauth_clients WHERE id = ?1"),
            vec![t(id)],
        )
        .await?;
        Ok(first(&r).map(record))
    }

    /// Registers a Client App (administrators).
    pub async fn create_client(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        input: ClientInput,
    ) -> Result<CreatedClient, AuthError> {
        require_admin(caller)?;
        let realm = input
            .realm
            .clone()
            .or_else(|| ci.realm.clone())
            .unwrap_or_else(|| forgebase_data::DEFAULT_REALM.to_owned());
        self.realm_identity(&realm)?;
        let name = check_name(&input.name)?;
        let public = input.client_type == ClientType::Public;
        let grant_types = dedup(
            input
                .grant_types
                .unwrap_or_else(|| vec![GRANT_CODE.into(), GRANT_REFRESH.into()]),
        );
        let only_machine = grant_types.iter().all(|g| g == GRANT_CLIENT_CREDENTIALS);
        let scopes = dedup(input.scopes.unwrap_or_else(|| {
            if only_machine {
                Vec::new()
            } else {
                ["openid", "profile", "email", "offline_access"]
                    .map(String::from)
                    .to_vec()
            }
        }));
        let s = Settings {
            redirect_uris: dedup(input.redirect_uris),
            post_logout_redirect_uris: dedup(input.post_logout_redirect_uris),
            grant_types,
            scopes,
            access_token_ttl: input.access_token_ttl,
            refresh_token_ttl: input.refresh_token_ttl,
        };
        check_settings(public, &s)?;
        let id = format!("fbc_{}", crypto::random_id(24)?);
        let secret = if public { None } else { Some(new_secret()?) };
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_clients
                (id, realm, name, description, client_type, secret_hash, secret_created_at, redirect_uris,
                 post_logout_redirect_uris, grant_types, scopes, skip_consent, access_token_ttl, refresh_token_ttl,
                 created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)",
            vec![
                t(id.clone()),
                t(realm.clone()),
                t(name),
                ot(input.description.filter(|d| !d.is_empty())),
                t(input.client_type.as_str()),
                ot(secret
                    .as_deref()
                    .map(|s| self.keyring().token_hash("oauth_client_secret", s))),
                if public {
                    forgebase_data::db::Value::Null
                } else {
                    i(now)
                },
                t(to_json(&s.redirect_uris)),
                t(to_json(&s.post_logout_redirect_uris)),
                t(to_json(&s.grant_types)),
                t(to_json(&s.scopes)),
                i(i64::from(input.skip_consent)),
                s.access_token_ttl.map_or(forgebase_data::db::Value::Null, i),
                s.refresh_token_ttl.map_or(forgebase_data::db::Value::Null, i),
                t(caller.subject.actor()),
                i(now),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.client.created",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"client_id": id, "realm": realm, "type": input.client_type.as_str()}),
        )
        .await?;
        tx.commit().await?;
        let client = self
            .load_client(&id)
            .await?
            .ok_or_else(|| AuthError::Internal("client vanished".into()))?;
        Ok(CreatedClient {
            client: client.view,
            client_secret: secret,
        })
    }

    /// Client Apps of a realm (all realms when `realm` is `None`).
    pub async fn list_clients(
        &self,
        caller: &Authenticated,
        realm: Option<&str>,
    ) -> Result<Vec<ClientView>, AuthError> {
        require_admin(caller)?;
        let r =
            match realm {
                Some(r) => read(
                    self.data().backend(),
                    &format!(
                        "SELECT {CLIENT_COLS} FROM _forgebase_oauth_clients WHERE realm = ?1 ORDER BY created_at, id"
                    ),
                    vec![t(r)],
                )
                .await?,
                None => {
                    read(
                        self.data().backend(),
                        &format!("SELECT {CLIENT_COLS} FROM _forgebase_oauth_clients ORDER BY realm, created_at, id"),
                        vec![],
                    )
                    .await?
                }
            };
        Ok(rows(&r).map(|row| record(row).view).collect())
    }

    /// One Client App.
    pub async fn get_client(&self, caller: &Authenticated, id: &str) -> Result<ClientView, AuthError> {
        require_admin(caller)?;
        self.load_client(id)
            .await?
            .map(|c| c.view)
            .ok_or_else(|| AuthError::NotFound("client not found".into()))
    }

    /// Changes a Client App. Narrowing redirect URIs, grants or scopes applies
    /// to new requests; disabling refuses every new authorization and token.
    pub async fn update_client(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        id: &str,
        patch: ClientPatch,
    ) -> Result<ClientView, AuthError> {
        require_admin(caller)?;
        let cur = self
            .load_client(id)
            .await?
            .ok_or_else(|| AuthError::NotFound("client not found".into()))?;
        let name = match &patch.name {
            Some(n) => check_name(n)?,
            None => cur.name.clone(),
        };
        let s = Settings {
            redirect_uris: dedup(patch.redirect_uris.clone().unwrap_or(cur.redirect_uris.clone())),
            post_logout_redirect_uris: dedup(
                patch
                    .post_logout_redirect_uris
                    .clone()
                    .unwrap_or(cur.post_logout_redirect_uris.clone()),
            ),
            grant_types: dedup(patch.grant_types.clone().unwrap_or(cur.grant_types.clone())),
            scopes: dedup(patch.scopes.clone().unwrap_or(cur.scopes.clone())),
            access_token_ttl: patch.access_token_ttl.or(cur.access_token_ttl),
            refresh_token_ttl: patch.refresh_token_ttl.or(cur.refresh_token_ttl),
        };
        check_settings(cur.is_public(), &s)?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "UPDATE _forgebase_oauth_clients SET name = ?2, description = ?3, redirect_uris = ?4,
                post_logout_redirect_uris = ?5, grant_types = ?6, scopes = ?7, skip_consent = ?8,
                access_token_ttl = ?9, refresh_token_ttl = ?10, disabled = ?11, updated_at = ?12
             WHERE id = ?1",
            vec![
                t(id),
                t(name),
                ot(patch.description.clone().or(cur.view.description.clone())),
                t(to_json(&s.redirect_uris)),
                t(to_json(&s.post_logout_redirect_uris)),
                t(to_json(&s.grant_types)),
                t(to_json(&s.scopes)),
                i(i64::from(patch.skip_consent.unwrap_or(cur.skip_consent))),
                s.access_token_ttl.map_or(forgebase_data::db::Value::Null, i),
                s.refresh_token_ttl.map_or(forgebase_data::db::Value::Null, i),
                i(i64::from(patch.disabled.unwrap_or(cur.disabled))),
                i(self.now()),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.client.updated",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"client_id": id, "disabled": patch.disabled}),
        )
        .await?;
        tx.commit().await?;
        self.get_client(caller, id).await
    }

    /// Deletes a Client App with its consents, codes and token families.
    pub async fn delete_client(&self, caller: &Authenticated, ci: &ClientInfo, id: &str) -> Result<(), AuthError> {
        require_admin(caller)?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_revoked_tokens WHERE family_id IN
                (SELECT id FROM _forgebase_oauth_families WHERE client_id = ?1)",
            vec![t(id)],
        )
        .await?;
        let n = tx
            .exec("DELETE FROM _forgebase_oauth_clients WHERE id = ?1", vec![t(id)])
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound("client not found".into()));
        }
        audit_in(
            &mut tx,
            "oauth.client.deleted",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"client_id": id}),
        )
        .await?;
        tx.commit().await
    }

    /// Issues a new secret for a confidential client. The previous secret
    /// keeps working for `grace_seconds` (default 0, at most 30 days).
    pub async fn rotate_client_secret(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        id: &str,
        grace_seconds: Option<i64>,
    ) -> Result<CreatedClient, AuthError> {
        require_admin(caller)?;
        let grace = grace_seconds.unwrap_or(0);
        if !(0..=30 * 24 * 3600).contains(&grace) {
            return Err(AuthError::invalid("grace_seconds must be 0 to 2592000"));
        }
        let cur = self
            .load_client(id)
            .await?
            .ok_or_else(|| AuthError::NotFound("client not found".into()))?;
        if cur.is_public() {
            return Err(AuthError::invalid("public clients have no secret"));
        }
        let secret = new_secret()?;
        let now = self.now();
        let (prev, prev_until) = if grace > 0 {
            (cur.secret_hash.clone(), Some(now + grace))
        } else {
            (None, None)
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "UPDATE _forgebase_oauth_clients SET secret_hash = ?2, secret_created_at = ?3, previous_secret_hash = ?4,
                previous_secret_expires_at = ?5, updated_at = ?3 WHERE id = ?1",
            vec![
                t(id),
                t(self.keyring().token_hash("oauth_client_secret", &secret)),
                i(now),
                ot(prev),
                prev_until.map_or(forgebase_data::db::Value::Null, i),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.client.secret_rotated",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"client_id": id, "grace_seconds": grace}),
        )
        .await?;
        tx.commit().await?;
        let client = self
            .load_client(id)
            .await?
            .ok_or_else(|| AuthError::Internal("client vanished".into()))?;
        Ok(CreatedClient {
            client: client.view,
            client_secret: Some(secret),
        })
    }

    /// Authenticates the client of a token-endpoint style request in `realm`.
    /// Unknown clients, clients of another realm, disabled clients and wrong
    /// secrets all answer the same `invalid_client`.
    pub(crate) async fn authenticate_client(
        &self,
        realm: &str,
        creds: &ClientCreds,
        ci: &ClientInfo,
    ) -> Result<ClientRecord, OAuthError> {
        self.limit_attempt(ci)?;
        let fail = || OAuthError::invalid_client("client authentication failed");
        let Some(id) = creds.id.as_deref().filter(|s| !s.is_empty()) else {
            return Err(fail());
        };
        let client = match self.load_client(id).await? {
            Some(c) if c.realm == realm && !c.disabled => c,
            _ => {
                self.audit_client_failure(ci, id, "unknown_client").await;
                return Err(fail());
            }
        };
        match (client.client_type, creds.secret.as_deref()) {
            (ClientType::Public, None) => Ok(client),
            (ClientType::Public, Some(_)) => Err(OAuthError::invalid_client(
                "public clients authenticate with client_id only",
            )),
            (ClientType::Confidential, None) => {
                self.audit_client_failure(ci, id, "missing_secret").await;
                Err(fail())
            }
            (ClientType::Confidential, Some(secret)) => {
                let kr = self.keyring();
                let current = client
                    .secret_hash
                    .as_deref()
                    .is_some_and(|h| kr.verify_token_hash("oauth_client_secret", secret, h));
                let previous = client
                    .previous_secret_expires_at
                    .is_some_and(|until| until > self.now())
                    && client
                        .previous_secret_hash
                        .as_deref()
                        .is_some_and(|h| kr.verify_token_hash("oauth_client_secret", secret, h));
                if current || previous {
                    Ok(client)
                } else {
                    self.audit_client_failure(ci, id, "wrong_secret").await;
                    Err(fail())
                }
            }
        }
    }

    async fn audit_client_failure(&self, ci: &ClientInfo, client_id: &str, reason: &str) {
        let actor = format!("client:{}", client_id.chars().take(64).collect::<String>());
        if let Err(e) = self
            .audit("oauth.client_auth", false, ci, &actor, None, json!({"reason": reason}))
            .await
        {
            tracing::warn!(error = %e, "could not audit a client authentication failure");
        }
    }
}

/// The public part of a client for consent screens.
pub(crate) fn client_brief(c: &ClientRecord) -> Json {
    json!({"client_id": c.id, "name": c.name, "realm": c.realm})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uri_rules() {
        assert!(check_redirect_uri("https://app.example.com/cb", false).is_ok());
        assert!(check_redirect_uri("http://127.0.0.1:8080/cb", false).is_ok());
        assert!(check_redirect_uri("http://localhost/cb", true).is_ok());
        assert!(check_redirect_uri("http://[::1]:9/cb", true).is_ok());
        assert!(check_redirect_uri("http://app.example.com/cb", false).is_err());
        assert!(check_redirect_uri("https://app.example.com/cb#x", false).is_err());
        assert!(check_redirect_uri("https://user:pw@app.example.com/cb", false).is_err());
        assert!(check_redirect_uri("com.example.app:/oauth", true).is_ok());
        assert!(check_redirect_uri("com.example.app:/oauth", false).is_err());
        assert!(check_redirect_uri("javascript:alert(1)", true).is_err());
        assert!(check_redirect_uri("/relative", false).is_err());
    }

    #[test]
    fn settings_rules() {
        let base = || Settings {
            redirect_uris: vec!["https://a.example/cb".into()],
            post_logout_redirect_uris: vec![],
            grant_types: vec![GRANT_CODE.into()],
            scopes: vec!["openid".into()],
            access_token_ttl: None,
            refresh_token_ttl: None,
        };
        assert!(check_settings(false, &base()).is_ok());
        let mut s = base();
        s.grant_types = vec![GRANT_CLIENT_CREDENTIALS.into()];
        assert!(check_settings(true, &s).is_err(), "public + client_credentials");
        let mut s = base();
        s.redirect_uris.clear();
        assert!(check_settings(false, &s).is_err(), "code without redirect");
        let mut s = base();
        s.scopes = vec!["root".into()];
        assert!(check_settings(false, &s).is_err(), "unknown scope");
        let mut s = base();
        s.grant_types = vec![GRANT_REFRESH.into()];
        assert!(check_settings(false, &s).is_err(), "refresh alone");
    }
}
