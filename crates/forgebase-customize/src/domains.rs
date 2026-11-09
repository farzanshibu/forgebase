//! Custom domains: claim, DNS TXT verification, routing table.
//!
//! A realm admin claims `login.example.com`; the service answers with a TXT
//! record to publish at `_forgebase-verify.login.example.com`
//! (`forgebase-verify=<token>`). `verify` looks the record up through the
//! configured [`crate::DnsResolver`]; the token is accepted until it expires
//! (`verify_ttl_ms`), after which it must be re-issued. Once verified, the
//! Host router ([`crate::host`]) sends requests for that host to the realm.
//!
//! A host belongs to one realm. An unverified claim whose token expired can be
//! taken over by another realm, so a squatter cannot block the real owner
//! forever; a verified host cannot be claimed again until it is removed.
//!
//! TLS is not terminated here: put the custom hosts behind the reverse proxy
//! that holds their certificates (see ADR-0020).

use forgebase_data::AccessContext;
use forgebase_db::Value;

use crate::error::{CustomizeError, Result};
use crate::model::{Domain, DomainInput, DomainStatus, VERIFY_LABEL, VERIFY_PREFIX, Verification, VerifyResult};
use crate::service::{CustomizeService, int, new_id, opt_int, opt_text, text};

const COLUMNS: &str =
    "id, realm, host, token, status, created_at, expires_at, verified_at, last_checked, last_error, created_by";

fn domain_row(r: &[Value]) -> Domain {
    let status = DomainStatus::parse(&text(r, 4));
    let host = text(r, 2);
    Domain {
        id: text(r, 0),
        realm: text(r, 1),
        verification: (status == DomainStatus::Pending).then(|| Verification {
            record_name: format!("{VERIFY_LABEL}.{host}"),
            record_type: "TXT".into(),
            record_value: format!("{VERIFY_PREFIX}{}", text(r, 3)),
            expires_at: int(r, 6),
        }),
        host,
        status,
        created_at: int(r, 5),
        verified_at: opt_int(r, 7),
        last_checked: opt_int(r, 8),
        last_error: opt_text(r, 9),
        created_by: text(r, 10),
    }
}

/// Normalizes a host name taken from input: trims, lower-cases, strips one
/// trailing dot. Does not accept ports, schemes, paths, wildcards or IPs.
pub fn normalize_host(h: &str) -> Result<String> {
    let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
    let bad = |m: &str| Err(CustomizeError::invalid("invalid_host", format!("{h:?}: {m}")));
    if h.len() < 3 || h.len() > 253 {
        return bad("a host name has 3 to 253 characters");
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return bad("needs at least two labels (example.com)");
    }
    for l in &labels {
        let ok = !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok {
            return bad(
                "labels are 1-63 characters of a-z, 0-9 and `-`, not starting or ending with `-` (no scheme, port, path or wildcard)",
            );
        }
    }
    // An IPv4 literal is not a domain.
    if labels.iter().all(|l| l.chars().all(|c| c.is_ascii_digit())) {
        return bad("an IP address is not a domain");
    }
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".invalid") || h.ends_with(".local") {
        return bad("reserved name");
    }
    Ok(h)
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

impl CustomizeService {
    /// Reloads the routing table (verified domains) from the database.
    pub async fn reload_hosts(&self) -> Result<()> {
        let rows = self
            .query(
                "SELECT host, realm FROM _forgebase_domains WHERE status = 'verified'",
                vec![],
            )
            .await?;
        let map = rows.rows.iter().map(|r| (text(r, 0), text(r, 1))).collect();
        if let Ok(mut c) = self.inner.cache.write() {
            c.hosts = map;
        }
        Ok(())
    }

    /// The realm a verified custom host routes to.
    pub fn realm_for_host(&self, host: &str) -> Option<String> {
        self.inner.cache.read().ok().and_then(|c| c.hosts.get(host).cloned())
    }

    /// Claims a host for a realm (pending until its TXT record is verified).
    pub async fn add_domain(&self, ctx: &AccessContext, input: DomainInput) -> Result<Domain> {
        let realm = self.realm_for(ctx, input.realm.as_deref())?;
        self.can_write(ctx, &realm)?;
        let host = normalize_host(&input.host)?;
        self.require_realm(&realm).await?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let existing = tx
                .fetch(
                    &format!("SELECT {COLUMNS} FROM _forgebase_domains WHERE host = ?1"),
                    vec![host.clone().into()],
                )
                .await?;
            if let Some(row) = existing.rows.first() {
                let d = domain_row(row);
                let stale = d.status == DomainStatus::Pending && int(row, 6) <= now;
                if !stale {
                    return Err(CustomizeError::conflict(
                        "domain_taken",
                        if d.realm == realm {
                            format!("{host} is already added to this realm")
                        } else {
                            format!("{host} belongs to another realm")
                        },
                    ));
                }
                tx.exec("DELETE FROM _forgebase_domains WHERE id = ?1", vec![d.id.into()])
                    .await?;
            }
            let id = new_id("dom");
            tx.exec(
                "INSERT INTO _forgebase_domains (id, realm, host, token, status, created_at, expires_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
                vec![
                    id.clone().into(),
                    realm.clone().into(),
                    host.into(),
                    new_token().into(),
                    now.into(),
                    (now + self.inner.cfg.verify_ttl_ms).into(),
                    ctx.principal_key().into(),
                ],
            )
            .await?;
            Ok(id)
        }
        .await;
        match res {
            Ok(id) => {
                tx.commit().await?;
                self.domain(ctx, &id).await
            }
            Err(e) => {
                tx.rollback().await;
                Err(e)
            }
        }
    }

    async fn domain_unchecked(&self, id: &str) -> Result<Domain> {
        let rows = self
            .query(
                &format!("SELECT {COLUMNS} FROM _forgebase_domains WHERE id = ?1"),
                vec![id.into()],
            )
            .await?;
        rows.rows
            .first()
            .map(|r| domain_row(r))
            .ok_or_else(|| CustomizeError::NotFound("domain".into()))
    }

    /// One domain (administrators of its realm).
    pub async fn domain(&self, ctx: &AccessContext, id: &str) -> Result<Domain> {
        self.require_caller(ctx)?;
        let d = self.domain_unchecked(id).await?;
        self.can_read(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("domain".into()))?;
        Ok(d)
    }

    /// The domains of a realm.
    pub async fn domains(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<Vec<Domain>> {
        let realm = self.realm_for(ctx, realm)?;
        self.can_read(ctx, &realm)?;
        let rows = self
            .query(
                &format!("SELECT {COLUMNS} FROM _forgebase_domains WHERE realm = ?1 ORDER BY host"),
                vec![realm.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| domain_row(r)).collect())
    }

    /// Issues a new token for a pending domain (after the old one expired).
    pub async fn reissue_domain(&self, ctx: &AccessContext, id: &str) -> Result<Domain> {
        let d = self.domain_unchecked(id).await?;
        self.can_write(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("domain".into()))?;
        if d.status == DomainStatus::Verified {
            return Err(CustomizeError::conflict(
                "already_verified",
                "the domain is already verified",
            ));
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        tx.exec(
            "UPDATE _forgebase_domains SET token = ?2, created_at = ?3, expires_at = ?4, last_error = NULL WHERE id = ?1",
            vec![id.into(), new_token().into(), now.into(), (now + self.inner.cfg.verify_ttl_ms).into()],
        )
        .await?;
        tx.commit().await?;
        self.domain_unchecked(id).await
    }

    /// Looks the TXT record up and marks the domain verified when it is there.
    /// An expired token is `409 token_expired`; a missing or wrong record is
    /// `verified: false` with the reason.
    pub async fn verify_domain(&self, ctx: &AccessContext, id: &str) -> Result<VerifyResult> {
        let d = self.domain_unchecked(id).await?;
        self.can_write(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("domain".into()))?;
        if d.status == DomainStatus::Verified {
            return Ok(VerifyResult {
                verified: true,
                reason: None,
                domain: d,
            });
        }
        let now = self.now();
        let Some(v) = d.verification.clone() else {
            return Err(CustomizeError::Internal("pending domain without a token".into()));
        };
        if v.expires_at <= now {
            return Err(CustomizeError::conflict(
                "token_expired",
                "the verification token expired; re-issue it and publish the new record",
            ));
        }
        let (verified, reason) = match self.dns().txt(&v.record_name).await {
            Err(e) => (false, Some(format!("DNS lookup failed: {e}"))),
            Ok(records) if records.iter().any(|r| r.trim() == v.record_value) => (true, None),
            Ok(records) if records.is_empty() => (false, Some(format!("no TXT record found at {}", v.record_name))),
            Ok(_) => (
                false,
                Some(format!(
                    "TXT records at {} do not contain the expected value",
                    v.record_name
                )),
            ),
        };
        let mut tx = self.begin().await?;
        if verified {
            // A concurrent claim cannot exist (host is unique), but the row may
            // have been removed meanwhile.
            let n = tx
                .exec(
                    "UPDATE _forgebase_domains SET status = 'verified', verified_at = ?2, last_checked = ?2, last_error = NULL
                     WHERE id = ?1 AND status = 'pending'",
                    vec![id.into(), now.into()],
                )
                .await?;
            if n == 0 {
                tx.rollback().await;
                return Err(CustomizeError::NotFound("domain".into()));
            }
        } else {
            tx.exec(
                "UPDATE _forgebase_domains SET last_checked = ?2, last_error = ?3 WHERE id = ?1",
                vec![id.into(), now.into(), reason.clone().into()],
            )
            .await?;
        }
        tx.commit().await?;
        if verified {
            self.reload_hosts().await?;
        }
        Ok(VerifyResult {
            verified,
            reason,
            domain: self.domain_unchecked(id).await?,
        })
    }

    /// Removes a domain; requests for its host stop routing to the realm.
    pub async fn delete_domain(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let d = self.domain_unchecked(id).await?;
        self.can_write(ctx, &d.realm)
            .map_err(|_| CustomizeError::NotFound("domain".into()))?;
        let mut tx = self.begin().await?;
        tx.exec("DELETE FROM _forgebase_domains WHERE id = ?1", vec![id.into()])
            .await?;
        tx.commit().await?;
        self.reload_hosts().await
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_host;

    #[test]
    fn host_validation() {
        assert_eq!(
            normalize_host(" Login.Example.COM. ").unwrap_or_default(),
            "login.example.com"
        );
        for bad in [
            "localhost",
            "example",
            "a.b..c",
            "-a.example.com",
            "a_b.example.com",
            "http://a.example.com",
            "a.example.com:8080",
            "*.example.com",
            "10.0.0.1",
            "a.example.com/x",
            "x.local",
            "a b.example.com",
        ] {
            assert!(normalize_host(bad).is_err(), "{bad}");
        }
    }
}
