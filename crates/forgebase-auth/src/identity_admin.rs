//! FIX-4 (MDF-59) administrator endpoints under `/api/auth/realms/{realm}`:
//! consents by realm and client, OIDC key ids, social login providers and the
//! SMS gateway. Secrets are write-only: the API shows `"********"`, a string
//! sets one, `""` clears it, and `"********"`, `null` or an absent field keep
//! the stored value. At rest they are sealed under the master key.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::realm::SECRET_MASK;
use crate::service::{ClientInfo, audit_in, require_admin};
use crate::social::ProviderConfig;
use crate::store::{Tx, first, i, read, rfc3339, rows, t};
use crate::{AuthError, AuthService, Authenticated};

/// One remembered consent, as an administrator sees it.
#[derive(Debug, Clone, Serialize)]
pub struct AdminConsent {
    /// Consent id.
    pub id: String,
    /// Client app id.
    pub client_id: String,
    /// Client app name.
    pub client_name: String,
    /// Realm of the client.
    pub realm: String,
    /// Auth collection of the user.
    pub collection: String,
    /// User id.
    pub user_id: String,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Updated (RFC 3339).
    pub updated_at: String,
}

/// Secret paths inside a provider configuration.
const PROVIDER_SECRETS: &[&[&str]] = &[&["client_secret"], &["apple", "private_key_pem"]];
/// Secret fields of an SMS gateway configuration.
const SMS_SECRETS: &[&str] = &["auth_token", "bearer_token"];

fn aad(kind: &str, realm: &str, id: &str, path: &str) -> Vec<u8> {
    format!("{kind}:{realm}:{id}:{path}").into_bytes()
}

fn get_path<'a>(v: &'a mut Json, path: &[&str]) -> Option<&'a mut Json> {
    let mut cur = v;
    for p in path {
        cur = cur.get_mut(*p)?;
    }
    Some(cur)
}

impl AuthService {
    fn seal_str(&self, plain: &str, aad: &[u8]) -> Result<Json, AuthError> {
        let sealed = self.keyring().seal(plain.as_bytes(), aad)?;
        Ok(Json::String(format!("sealed:{}", URL_SAFE_NO_PAD.encode(sealed))))
    }

    fn open_str(&self, v: &Json, aad: &[u8]) -> Result<Option<String>, AuthError> {
        let Some(s) = v.as_str().and_then(|s| s.strip_prefix("sealed:")) else {
            return Ok(None);
        };
        let blob = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| AuthError::Internal("corrupt sealed secret".into()))?;
        let plain = self.keyring().open(&blob, aad)?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|_| AuthError::Internal("corrupt sealed secret".into()))
    }

    /// Replaces each secret path of `input` by the plain secret to store:
    /// the input's own, the old one (keep), or nothing (clear). `old` holds
    /// the stored (sealed) object. Returns the plain object.
    fn resolve_secrets(
        &self,
        input: &mut Json,
        old: Option<&Json>,
        paths: &[Vec<&str>],
        aad_of: &dyn Fn(&str) -> Vec<u8>,
    ) -> Result<(), AuthError> {
        for path in paths {
            let key = path.join(".");
            let given = get_path(input, path).map(|v| v.clone());
            let keep = match &given {
                None | Some(Json::Null) => true,
                Some(Json::String(s)) if s == SECRET_MASK => true,
                Some(Json::String(_)) => false,
                Some(_) => return Err(AuthError::invalid(format!("{key} must be a string"))),
            };
            let value = if keep {
                let mut old = old.cloned();
                match old.as_mut().and_then(|o| get_path(o, path)) {
                    Some(v) => self.open_str(v, &aad_of(&key))?,
                    None => None,
                }
            } else {
                given
                    .and_then(|g| g.as_str().map(str::to_owned))
                    .filter(|s| !s.is_empty())
            };
            // Write back into the input (creating nothing when the parent is absent).
            let Some((last, parents)) = path.split_last() else {
                continue;
            };
            let parent = if parents.is_empty() {
                Some(&mut *input)
            } else {
                get_path(input, parents)
            };
            if let Some(Json::Object(o)) = parent {
                match value {
                    Some(s) => {
                        o.insert((*last).to_owned(), Json::String(s));
                    }
                    None => {
                        o.remove(*last);
                    }
                }
            }
        }
        Ok(())
    }

    /// Seals the secret paths of a plain object for storage.
    fn seal_secrets(
        &self,
        plain: &mut Json,
        paths: &[Vec<&str>],
        aad_of: &dyn Fn(&str) -> Vec<u8>,
    ) -> Result<(), AuthError> {
        for path in paths {
            let key = path.join(".");
            if let Some(v) = get_path(plain, path)
                && let Some(s) = v.as_str().map(str::to_owned)
            {
                *v = self.seal_str(&s, &aad_of(&key))?;
            }
        }
        Ok(())
    }

    /// Unseals the secret paths of a stored object.
    fn unseal_secrets(
        &self,
        stored: &mut Json,
        paths: &[Vec<&str>],
        aad_of: &dyn Fn(&str) -> Vec<u8>,
    ) -> Result<(), AuthError> {
        for path in paths {
            let key = path.join(".");
            if let Some(v) = get_path(stored, path) {
                match self.open_str(v, &aad_of(&key))? {
                    Some(s) => *v = Json::String(s),
                    None => *v = Json::Null,
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Consents
    // -----------------------------------------------------------------------

    /// Remembered consents of a realm, optionally for one client
    /// (administrators). Newest first.
    pub async fn list_realm_consents(
        &self,
        caller: &Authenticated,
        realm: &str,
        client_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AdminConsent>, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        let r = read(
            self.data().backend(),
            "SELECT c.id, c.client_id, c.realm, c.collection, c.user_id, c.scopes, c.created_at, c.updated_at, k.name
             FROM _forgebase_oauth_consents c JOIN _forgebase_oauth_clients k ON k.id = c.client_id
             WHERE c.realm = ?1 AND (?2 IS NULL OR c.client_id = ?2)
             ORDER BY c.created_at DESC, c.id LIMIT ?3 OFFSET ?4",
            vec![
                t(realm),
                crate::store::ot(client_id.map(str::to_owned)),
                i(limit.clamp(1, 500)),
                i(offset.max(0)),
            ],
        )
        .await?;
        Ok(rows(&r)
            .map(|row| AdminConsent {
                id: row.str("id"),
                client_id: row.str("client_id"),
                client_name: row.str("name"),
                realm: row.str("realm"),
                collection: row.str("collection"),
                user_id: row.str("user_id"),
                scopes: serde_json::from_str(&row.str("scopes")).unwrap_or_default(),
                created_at: rfc3339(row.i64("created_at")),
                updated_at: rfc3339(row.i64("updated_at")),
            })
            .collect())
    }

    /// Revokes any remembered consent of a realm and the token families the
    /// client holds for that user (administrators).
    pub async fn revoke_realm_consent(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
        id: &str,
    ) -> Result<(), AuthError> {
        require_admin(caller)?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT client_id, collection, user_id FROM _forgebase_oauth_consents WHERE id = ?1 AND realm = ?2",
                vec![t(id), t(realm)],
            )
            .await?;
        let Some((client_id, coll, uid)) =
            first(&r).map(|row| (row.str("client_id"), row.str("collection"), row.str("user_id")))
        else {
            return Err(AuthError::NotFound("consent not found".into()));
        };
        tx.exec("DELETE FROM _forgebase_oauth_consents WHERE id = ?1", vec![t(id)])
            .await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_oauth_families SET revoked_at = ?4, revoked_reason = 'consent_revoked'
                 WHERE client_id = ?1 AND collection = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                vec![t(client_id.clone()), t(coll), t(uid), i(self.now())],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            "oauth.consent.revoked_by_admin",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"client_id": client_id, "realm": realm, "families_revoked": n}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // OIDC keys
    // -----------------------------------------------------------------------

    /// Current and retired OIDC signing key ids of a realm (administrators).
    /// Never exposes key material; `kid` is public (it is in the JWKS).
    pub async fn list_oidc_keys(&self, caller: &Authenticated, realm: &str) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        // Make sure the realm has a signing key, like the JWKS endpoint does.
        self.oidc_jwks(realm).await?;
        let now = self.now();
        let r = read(
            self.data().backend(),
            "SELECT kid, alg, status, created_at, verify_until FROM _forgebase_oidc_keys
             WHERE realm = ?1 ORDER BY created_at DESC, kid",
            vec![t(realm)],
        )
        .await?;
        let items: Vec<Json> = rows(&r)
            .map(|row| {
                let status = row.str("status");
                let until = row.opt_i64("verify_until");
                json!({
                    "kid": row.str("kid"),
                    "alg": row.str("alg"),
                    "status": status,
                    "created_at": rfc3339(row.i64("created_at")),
                    "verify_until": until.map(rfc3339),
                    "verifying": status == "active" || until.is_some_and(|u| u >= now),
                })
            })
            .collect();
        let current = items.iter().find(|k| k["status"] == "active").map(|k| k["kid"].clone());
        Ok(json!({"realm": realm, "current": current, "items": items}))
    }

    // -----------------------------------------------------------------------
    // Social login providers
    // -----------------------------------------------------------------------

    fn provider_paths() -> Vec<Vec<&'static str>> {
        PROVIDER_SECRETS.iter().map(|p| p.to_vec()).collect()
    }

    /// The API view of a stored provider: secrets masked.
    fn provider_view(&self, realm: &str, id: &str, stored: &Json, created: i64, updated: i64) -> Json {
        let mut v = stored.clone();
        for path in PROVIDER_SECRETS {
            if let Some(slot) = get_path(&mut v, path) {
                *slot = if slot.as_str().is_some_and(|s| !s.is_empty()) {
                    Json::String(SECRET_MASK.into())
                } else {
                    Json::Null
                };
            }
        }
        if let Some(o) = v.as_object_mut() {
            o.insert("id".into(), json!(id));
            o.insert("realm".into(), json!(realm));
            o.insert("source".into(), json!("admin"));
            o.insert("read_only".into(), json!(false));
            o.insert("created_at".into(), json!(rfc3339(created)));
            o.insert("updated_at".into(), json!(rfc3339(updated)));
        }
        v
    }

    async fn stored_provider(&self, realm: &str, id: &str) -> Result<Option<(Json, i64, i64)>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT config, created_at, updated_at FROM _forgebase_oauth_providers WHERE realm = ?1 AND id = ?2",
            vec![t(realm), t(id)],
        )
        .await?;
        Ok(first(&r).map(|row| {
            (
                serde_json::from_str(&row.str("config")).unwrap_or(Json::Null),
                row.i64("created_at"),
                row.i64("updated_at"),
            )
        }))
    }

    /// Social providers of a realm: the ones created over REST plus the ones
    /// defined in `oauth-providers.json` (read-only). Secrets never leave.
    pub async fn list_realm_providers(&self, caller: &Authenticated, realm: &str) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        let r = read(
            self.data().backend(),
            "SELECT id, config, created_at, updated_at FROM _forgebase_oauth_providers WHERE realm = ?1 ORDER BY id",
            vec![t(realm)],
        )
        .await?;
        let mut items: Vec<Json> = rows(&r)
            .map(|row| {
                let cfg = serde_json::from_str(&row.str("config")).unwrap_or(Json::Null);
                self.provider_view(
                    realm,
                    &row.str("id"),
                    &cfg,
                    row.i64("created_at"),
                    row.i64("updated_at"),
                )
            })
            .collect();
        for p in self.providers().list(realm) {
            if self.providers().is_file_defined(realm, &p.id) {
                items.push(json!({
                    "id": p.id, "realm": realm, "kind": p.kind, "display_name": p.display_name,
                    "source": "file", "read_only": true,
                }));
            }
        }
        items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(json!({"items": items}))
    }

    /// One social provider of a realm (administrators).
    pub async fn get_realm_provider(&self, caller: &Authenticated, realm: &str, id: &str) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        if let Some((cfg, c, u)) = self.stored_provider(realm, id).await? {
            return Ok(self.provider_view(realm, id, &cfg, c, u));
        }
        match self.providers().list(realm).into_iter().find(|p| p.id == id) {
            Some(p) => Ok(json!({
                "id": p.id, "realm": realm, "kind": p.kind, "display_name": p.display_name,
                "source": "file", "read_only": true,
            })),
            None => Err(AuthError::NotFound(format!("provider {id:?} not found"))),
        }
    }

    /// Creates or replaces a social provider (administrators). `body` is a
    /// provider configuration without `id`; secrets follow the write-only
    /// rules. Providers defined in `oauth-providers.json` cannot be changed.
    pub async fn put_realm_provider(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
        id: &str,
        mut body: Json,
    ) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        if self.providers().is_file_defined(realm, id) {
            return Err(AuthError::Conflict(format!(
                "provider {id:?} is defined in oauth-providers.json; edit the file or use another id"
            )));
        }
        let Some(obj) = body.as_object_mut() else {
            return Err(AuthError::invalid("provider body must be an object"));
        };
        match obj.remove("id") {
            None => {}
            Some(Json::String(s)) if s == id => {}
            Some(_) => return Err(AuthError::invalid("body id differs from the URL")),
        }
        for k in ["realm", "source", "read_only", "created_at", "updated_at"] {
            obj.remove(k);
        }
        obj.insert("id".into(), json!(id));
        let old = self.stored_provider(realm, id).await?;
        let paths = Self::provider_paths();
        let a = |p: &str| aad("oauth-provider", realm, id, p);
        self.resolve_secrets(&mut body, old.as_ref().map(|o| &o.0), &paths, &a)?;
        let cfg: ProviderConfig = serde_json::from_value(body.clone())
            .map_err(|e| AuthError::invalid(format!("provider configuration: {e}")))?;
        cfg.validate()?;
        self.seal_secrets(&mut body, &paths, &a)?;
        if let Some(o) = body.as_object_mut() {
            o.remove("id");
        }
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_providers (realm, id, config, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (realm, id) DO UPDATE SET config = excluded.config, updated_at = excluded.updated_at",
            vec![t(realm), t(id), t(body.to_string()), i(now)],
        )
        .await?;
        audit_in(
            &mut tx,
            "realm.provider_saved",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm, "provider": id, "kind": cfg.kind, "created": old.is_none()}),
        )
        .await?;
        tx.commit().await?;
        self.providers().register(realm, cfg)?;
        let (created, _) = old.map_or((now, now), |o| (o.1, now));
        Ok(self.provider_view(realm, id, &body, created, now))
    }

    /// Deletes a provider created over REST (administrators). Linked
    /// identities stay, but nobody can sign in with it any more.
    pub async fn delete_realm_provider(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
        id: &str,
    ) -> Result<(), AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        if self.providers().is_file_defined(realm, id) {
            return Err(AuthError::Conflict(format!(
                "provider {id:?} is defined in oauth-providers.json; remove it there"
            )));
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_oauth_providers WHERE realm = ?1 AND id = ?2",
                vec![t(realm), t(id)],
            )
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound(format!("provider {id:?} not found")));
        }
        audit_in(
            &mut tx,
            "realm.provider_deleted",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm, "provider": id}),
        )
        .await?;
        tx.commit().await?;
        self.providers().remove(realm, id);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // SMS gateway
    // -----------------------------------------------------------------------

    fn sms_paths() -> Vec<Vec<&'static str>> {
        SMS_SECRETS.iter().map(|p| vec![*p]).collect()
    }

    fn sms_view(&self, realm: &str, stored: Option<(&Json, i64, i64)>) -> Json {
        let Some((cfg, created, updated)) = stored else {
            return json!({"realm": realm, "configured": false, "config": null});
        };
        let mut v = cfg.clone();
        if let Some(o) = v.as_object_mut() {
            for k in SMS_SECRETS {
                if let Some(slot) = o.get_mut(*k) {
                    *slot = if slot.as_str().is_some_and(|s| !s.is_empty()) {
                        Json::String(SECRET_MASK.into())
                    } else {
                        Json::Null
                    };
                }
            }
        }
        json!({"realm": realm, "configured": true, "config": v, "created_at": rfc3339(created), "updated_at": rfc3339(updated)})
    }

    async fn stored_sms(&self, realm: &str) -> Result<Option<(Json, i64, i64)>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT config, created_at, updated_at FROM _forgebase_sms_config WHERE realm = ?1",
            vec![t(realm)],
        )
        .await?;
        Ok(first(&r).map(|row| {
            (
                serde_json::from_str(&row.str("config")).unwrap_or(Json::Null),
                row.i64("created_at"),
                row.i64("updated_at"),
            )
        }))
    }

    /// The SMS gateway configuration of a realm, secrets masked
    /// (administrators). `configured: false` means the instance gateway is used.
    pub async fn get_realm_sms(&self, caller: &Authenticated, realm: &str) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        let s = self.stored_sms(realm).await?;
        Ok(self.sms_view(realm, s.as_ref().map(|(c, a, b)| (c, *a, *b))))
    }

    /// Sets the realm's SMS gateway: `{provider: "dev"}`, `{provider:
    /// "twilio", account_sid, auth_token, from | messaging_service_sid,
    /// base_url?}` or `{provider: "webhook", url, bearer_token?}`
    /// (administrators). Takes effect at once.
    pub async fn put_realm_sms(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
        mut body: Json,
    ) -> Result<Json, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        if !body.is_object() {
            return Err(AuthError::invalid("sms configuration must be an object"));
        }
        let old = self.stored_sms(realm).await?;
        let paths = Self::sms_paths();
        let a = |p: &str| aad("sms", realm, "gateway", p);
        // Keeping a secret only makes sense for the same provider type.
        let same = old.as_ref().is_some_and(|o| o.0["provider"] == body["provider"]);
        self.resolve_secrets(&mut body, old.as_ref().filter(|_| same).map(|o| &o.0), &paths, &a)?;
        let provider = self.build_realm_sms(realm, body.clone())?;
        self.seal_secrets(&mut body, &paths, &a)?;
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_sms_config (realm, config, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT (realm) DO UPDATE SET config = excluded.config, updated_at = excluded.updated_at",
            vec![t(realm), t(body.to_string()), i(now)],
        )
        .await?;
        audit_in(
            &mut tx,
            "realm.sms_saved",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm, "provider": body["provider"]}),
        )
        .await?;
        tx.commit().await?;
        self.set_realm_sms(realm, Some(provider));
        let created = old.as_ref().map_or(now, |o| o.1);
        Ok(self.sms_view(realm, Some((&body, created, now))))
    }

    /// Removes the realm's SMS gateway; the instance gateway applies again.
    pub async fn delete_realm_sms(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
    ) -> Result<(), AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec("DELETE FROM _forgebase_sms_config WHERE realm = ?1", vec![t(realm)])
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound("no SMS gateway configured for this realm".into()));
        }
        audit_in(
            &mut tx,
            "realm.sms_deleted",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm}),
        )
        .await?;
        tx.commit().await?;
        self.set_realm_sms(realm, None);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Boot
    // -----------------------------------------------------------------------

    /// Loads REST-managed providers and SMS gateways after the files
    /// (`oauth-providers.json` wins over a stored provider with the same id).
    pub(crate) async fn load_identity_admin_config(&self) -> Result<(), AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT realm, id, config FROM _forgebase_oauth_providers",
            vec![],
        )
        .await?;
        for row in rows(&r) {
            let (realm, id) = (row.str("realm"), row.str("id"));
            if self.providers().is_file_defined(&realm, &id) {
                tracing::warn!(%realm, provider = %id, "oauth-providers.json overrides a stored provider");
                continue;
            }
            let mut cfg: Json = serde_json::from_str(&row.str("config")).unwrap_or(Json::Null);
            let a = |p: &str| aad("oauth-provider", &realm, &id, p);
            self.unseal_secrets(&mut cfg, &Self::provider_paths(), &a)?;
            if let Some(o) = cfg.as_object_mut() {
                o.insert("id".into(), json!(id));
            }
            match serde_json::from_value::<ProviderConfig>(cfg) {
                Ok(c) => {
                    if let Err(e) = self.providers().register(&realm, c) {
                        tracing::warn!(%realm, provider = %id, error = %e, "stored provider rejected");
                    }
                }
                Err(e) => tracing::warn!(%realm, provider = %id, error = %e, "stored provider unreadable"),
            }
        }
        let r = read(
            self.data().backend(),
            "SELECT realm, config FROM _forgebase_sms_config",
            vec![],
        )
        .await?;
        for row in rows(&r) {
            let realm = row.str("realm");
            let mut cfg: Json = serde_json::from_str(&row.str("config")).unwrap_or(Json::Null);
            let a = |p: &str| aad("sms", &realm, "gateway", p);
            self.unseal_secrets(&mut cfg, &Self::sms_paths(), &a)?;
            match self.build_realm_sms(&realm, cfg) {
                Ok(p) => self.set_realm_sms(&realm, Some(p)),
                Err(e) => tracing::warn!(%realm, error = %e, "stored SMS gateway unusable"),
            }
        }
        Ok(())
    }
}
