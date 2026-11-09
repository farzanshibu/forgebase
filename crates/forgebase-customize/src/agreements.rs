//! Versioned legal agreements and acceptance records.
//!
//! A realm publishes versions of agreements (`terms`, `privacy`, or any key).
//! Versions are immutable (a trigger refuses edits); the highest version of a
//! kind is current. Users accept the current version; the acceptance (who,
//! which version, when, from which address and client) is appended to an
//! immutable log. [`crate::AgreementGate`] blocks requests of users who have
//! not accepted every current version, so publishing a new version makes
//! every user accept again: the gate answers `403 agreement_required` with
//! the pending versions in `details` until they `POST .../accept`.

use forgebase_data::{AccessContext, Principal};
use forgebase_db::Value;

use crate::error::{CustomizeError, Result};
use crate::model::{AcceptInput, Acceptance, Agreement, AgreementInput, AgreementRef};
use crate::service::{CustomizeService, check_key, int, new_id, opt_text, text};

const COLUMNS: &str = "id, realm, kind, version, title, body, url, published_at, published_by";
const ACCEPT_COLUMNS: &str = "id, realm, principal, kind, version, accepted_at, ip, user_agent";

fn agreement_row(r: &[Value]) -> Agreement {
    Agreement {
        id: text(r, 0),
        realm: text(r, 1),
        kind: text(r, 2),
        version: int(r, 3),
        title: text(r, 4),
        body: text(r, 5),
        url: opt_text(r, 6),
        published_at: int(r, 7),
        published_by: text(r, 8),
    }
}

fn acceptance_row(r: &[Value]) -> Acceptance {
    Acceptance {
        id: text(r, 0),
        realm: text(r, 1),
        principal: text(r, 2),
        kind: text(r, 3),
        version: int(r, 4),
        accepted_at: int(r, 5),
        ip: opt_text(r, 6),
        user_agent: opt_text(r, 7),
    }
}

/// Filters of the acceptance log.
#[derive(Debug, Clone, Default)]
pub struct AcceptanceQuery {
    /// Only this principal (`user:<collection>/<id>`).
    pub principal: Option<String>,
    /// Only this agreement kind.
    pub kind: Option<String>,
    /// Page size (default 100, max 500).
    pub limit: Option<i64>,
}

/// Who and from where an acceptance was made.
#[derive(Debug, Clone, Default)]
pub struct AcceptSource {
    /// Client address.
    pub ip: Option<String>,
    /// User-Agent header.
    pub user_agent: Option<String>,
}

fn clip(s: Option<String>, max: usize) -> Option<String> {
    s.map(|v| v.chars().filter(|c| !c.is_control()).take(max).collect::<String>())
        .filter(|v| !v.is_empty())
}

impl CustomizeService {
    /// Reloads the current versions and every acceptance into memory.
    pub async fn reload_agreements(&self) -> Result<()> {
        let cur = self
            .query(
                "SELECT a.realm, a.kind, a.version, a.title FROM _forgebase_agreements a
                 WHERE a.version = (SELECT MAX(version) FROM _forgebase_agreements b WHERE b.realm = a.realm AND b.kind = a.kind)
                 ORDER BY a.realm, a.kind",
                vec![],
            )
            .await?;
        let acc = self
            .query(
                "SELECT realm, principal, kind, MAX(version) FROM _forgebase_agreement_acceptances GROUP BY realm, principal, kind",
                vec![],
            )
            .await?;
        let mut current: std::collections::HashMap<String, Vec<AgreementRef>> = std::collections::HashMap::new();
        for r in &cur.rows {
            current.entry(text(r, 0)).or_default().push(AgreementRef {
                kind: text(r, 1),
                version: int(r, 2),
                title: text(r, 3),
            });
        }
        let mut accepted: std::collections::HashMap<(String, String), std::collections::HashMap<String, i64>> =
            std::collections::HashMap::new();
        for r in &acc.rows {
            accepted
                .entry((text(r, 0), text(r, 1)))
                .or_default()
                .insert(text(r, 2), int(r, 3));
        }
        if let Ok(mut c) = self.inner.cache.write() {
            c.current = current;
            c.accepted = accepted;
        }
        Ok(())
    }

    /// The current versions `principal` has not accepted in `realm` (from
    /// memory: the agreement gate calls this on every request).
    pub fn pending_for(&self, realm: &str, principal: &str) -> Vec<AgreementRef> {
        let Ok(c) = self.inner.cache.read() else {
            return Vec::new();
        };
        let Some(cur) = c.current.get(realm) else {
            return Vec::new();
        };
        let acc = c.accepted.get(&(realm.to_owned(), principal.to_owned()));
        cur.iter()
            .filter(|a| acc.and_then(|m| m.get(&a.kind)).is_none_or(|v| *v < a.version))
            .cloned()
            .collect()
    }

    /// Publishes the next version of an agreement kind.
    pub async fn publish_agreement(
        &self,
        ctx: &AccessContext,
        realm: Option<&str>,
        input: AgreementInput,
    ) -> Result<Agreement> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_write(ctx, &realm)?;
        check_key("kind", &input.kind, false)?;
        self.require_realm(&realm).await?;
        let title = input.title.trim().to_owned();
        if title.is_empty() || title.chars().count() > 200 {
            return Err(CustomizeError::invalid("invalid_agreement", "a title of 1-200 characters is required"));
        }
        if input.body.trim().is_empty() || input.body.len() > 200 * 1024 {
            return Err(CustomizeError::invalid("invalid_agreement", "a body of 1 byte to 200 KiB is required"));
        }
        let url = input.url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        if let Some(u) = &url {
            let l = u.to_ascii_lowercase();
            if !(l.starts_with("https://") || l.starts_with("http://")) || l.chars().any(|c| c.is_control() || c.is_whitespace()) || u.len() > 2000 {
                return Err(CustomizeError::invalid("invalid_agreement", "url must be an http(s) URL"));
            }
        }
        let now = self.now();
        let id = new_id("agr");
        let mut tx = self.begin().await?;
        let v = tx
            .fetch(
                "SELECT COALESCE(MAX(version), 0) FROM _forgebase_agreements WHERE realm = ?1 AND kind = ?2",
                vec![realm.clone().into(), input.kind.clone().into()],
            )
            .await?
            .rows
            .first()
            .map(|r| int(r, 0))
            .unwrap_or(0)
            + 1;
        tx.exec(
            "INSERT INTO _forgebase_agreements (id, realm, kind, version, title, body, url, published_at, published_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            vec![
                id.clone().into(),
                realm.clone().into(),
                input.kind.clone().into(),
                v.into(),
                title.clone().into(),
                input.body.clone().into(),
                url.clone().into(),
                now.into(),
                ctx.principal_key().into(),
            ],
        )
        .await?;
        tx.commit().await?;
        self.reload_agreements().await?;
        Ok(Agreement {
            id,
            realm,
            kind: input.kind,
            version: v,
            title,
            body: input.body,
            url,
            published_at: now,
            published_by: ctx.principal_key(),
        })
    }

    /// Every published version of a realm (newest first), optionally of one kind.
    pub async fn agreements(&self, ctx: &AccessContext, realm: Option<&str>, kind: Option<&str>) -> Result<Vec<Agreement>> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let mut sql = format!("SELECT {COLUMNS} FROM _forgebase_agreements WHERE realm = ?1");
        let mut params: Vec<Value> = vec![realm.into()];
        if let Some(k) = kind {
            params.push(k.into());
            sql.push_str(" AND kind = ?2");
        }
        sql.push_str(" ORDER BY kind, version DESC");
        Ok(self.query(&sql, params).await?.rows.iter().map(|r| agreement_row(r)).collect())
    }

    /// The current version of every agreement kind of a realm, with text (public).
    pub async fn current_agreements(&self, realm: &str) -> Result<Vec<Agreement>> {
        self.require_realm(realm).await?;
        let rows = self
            .query(
                &format!(
                    "SELECT {COLUMNS} FROM _forgebase_agreements a WHERE realm = ?1
                     AND version = (SELECT MAX(version) FROM _forgebase_agreements b WHERE b.realm = a.realm AND b.kind = a.kind)
                     ORDER BY kind"
                ),
                vec![realm.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| agreement_row(r)).collect())
    }

    /// What the caller still has to accept in the realm of the request.
    pub fn pending(&self, ctx: &AccessContext) -> Result<Vec<AgreementRef>> {
        self.require_caller(ctx)?;
        Ok(self.pending_for(&ctx.realm_id, &ctx.principal_key()))
    }

    /// Records that the caller accepted the given versions. Only the current
    /// version of a kind can be accepted (`409 agreement_outdated` otherwise);
    /// accepting again is a no-op. Only user accounts accept: service accounts
    /// and administrators are not bound by the gate.
    pub async fn accept(&self, ctx: &AccessContext, input: AcceptInput, from: AcceptSource) -> Result<Vec<Acceptance>> {
        self.require_caller(ctx)?;
        if !matches!(ctx.principal, Principal::User { .. }) {
            return Err(CustomizeError::Forbidden("only user accounts accept agreements".into()));
        }
        if ctx.impersonator.is_some() {
            return Err(CustomizeError::Forbidden("an impersonating administrator cannot accept on behalf of a user".into()));
        }
        if input.items.is_empty() || input.items.len() > 50 {
            return Err(CustomizeError::invalid("invalid_acceptance", "name 1-50 agreement versions"));
        }
        let realm = ctx.realm_id.clone();
        let principal = ctx.principal_key();
        let ip = clip(from.ip, 64);
        let ua = clip(from.user_agent, 300);
        let now = self.now();
        let mut tx = self.begin().await?;
        let mut out = Vec::new();
        for item in &input.items {
            let cur = tx
                .fetch(
                    "SELECT MAX(version) FROM _forgebase_agreements WHERE realm = ?1 AND kind = ?2",
                    vec![realm.clone().into(), item.kind.clone().into()],
                )
                .await?;
            let Some(current) = cur.rows.first().and_then(|r| r.first()).and_then(Value::as_i64) else {
                tx.rollback().await;
                return Err(CustomizeError::NotFound(format!("agreement {:?}", item.kind)));
            };
            if item.version != current {
                tx.rollback().await;
                return Err(CustomizeError::conflict(
                    "agreement_outdated",
                    format!("{} version {} is not the current version ({current})", item.kind, item.version),
                ));
            }
            tx.exec(
                "INSERT OR IGNORE INTO _forgebase_agreement_acceptances (id, realm, principal, kind, version, accepted_at, ip, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                vec![
                    new_id("acc").into(),
                    realm.clone().into(),
                    principal.clone().into(),
                    item.kind.clone().into(),
                    item.version.into(),
                    now.into(),
                    ip.clone().into(),
                    ua.clone().into(),
                ],
            )
            .await?;
            let row = tx
                .fetch(
                    &format!(
                        "SELECT {ACCEPT_COLUMNS} FROM _forgebase_agreement_acceptances
                         WHERE realm = ?1 AND principal = ?2 AND kind = ?3 AND version = ?4"
                    ),
                    vec![realm.clone().into(), principal.clone().into(), item.kind.clone().into(), item.version.into()],
                )
                .await?;
            if let Some(r) = row.rows.first() {
                out.push(acceptance_row(r));
            }
        }
        tx.commit().await?;
        if let Ok(mut c) = self.inner.cache.write() {
            let m = c.accepted.entry((realm, principal)).or_default();
            for item in &input.items {
                let e = m.entry(item.kind.clone()).or_insert(0);
                *e = (*e).max(item.version);
            }
        }
        Ok(out)
    }

    /// Acceptance records of a realm (administrators).
    pub async fn acceptances(&self, ctx: &AccessContext, realm: Option<&str>, q: AcceptanceQuery) -> Result<Vec<Acceptance>> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let mut sql = format!("SELECT {ACCEPT_COLUMNS} FROM _forgebase_agreement_acceptances WHERE realm = ?1");
        let mut params: Vec<Value> = vec![realm.into()];
        if let Some(p) = q.principal {
            params.push(p.into());
            sql.push_str(&format!(" AND principal = ?{}", params.len()));
        }
        if let Some(k) = q.kind {
            params.push(k.into());
            sql.push_str(&format!(" AND kind = ?{}", params.len()));
        }
        params.push(q.limit.unwrap_or(100).clamp(1, 500).into());
        sql.push_str(&format!(" ORDER BY accepted_at DESC, id DESC LIMIT ?{}", params.len()));
        Ok(self.query(&sql, params).await?.rows.iter().map(|r| acceptance_row(r)).collect())
    }

    /// The caller's own acceptance records.
    pub async fn my_acceptances(&self, ctx: &AccessContext) -> Result<Vec<Acceptance>> {
        self.require_caller(ctx)?;
        let rows = self
            .query(
                &format!(
                    "SELECT {ACCEPT_COLUMNS} FROM _forgebase_agreement_acceptances
                     WHERE realm = ?1 AND principal = ?2 ORDER BY accepted_at DESC, id DESC LIMIT 500"
                ),
                vec![ctx.realm_id.clone().into(), ctx.principal_key().into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| acceptance_row(r)).collect())
    }
}
