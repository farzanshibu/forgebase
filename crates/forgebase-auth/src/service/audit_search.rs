//! Audit investigation (ADR-0036 §4): searching the audit trail by actor,
//! action, resource, outcome, client address, request id, time and free text;
//! cursor paging; related events.
//!
//! These methods do not check who asks: the caller (the administrator-only
//! `/api/v1/insights/audit` routes, or [`AuthService::audit_log`]'s own
//! callers) authorises, and passes `realm` to confine a realm administrator
//! to the rows of their realm. Every value is a bound parameter.

use serde::{Deserialize, Serialize};

use super::{AuditEntry, AuthService, audit_entry};
use forgebase_data::db::Value;

use crate::AuthError;
use crate::store::{self, i, ot, read, rows, t};

/// The hard cap of one page.
pub const AUDIT_PAGE_MAX: u32 = 1000;

/// Which rows of the audit trail to list.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditSearch {
    /// Exactly this actor (`user:users/abc`, `admin:xyz`, `service_account:id`, `anonymous`).
    pub actor: Option<String>,
    /// Actors of this kind: `user`, `admin`, `service_account`, `api_key` or `anonymous`.
    pub actor_type: Option<String>,
    /// Events (actions) starting with this prefix (`login`, `api_key.`).
    pub action: Option<String>,
    /// Kind of the resource concerned (`user`, `admin`, `service_account`, ...).
    pub resource_type: Option<String>,
    /// Collection of the resource concerned.
    pub collection: Option<String>,
    /// Id of the resource concerned.
    pub resource_id: Option<String>,
    /// `success` or `failure`.
    pub outcome: Option<String>,
    /// Client address.
    pub ip: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// At or after (Unix seconds).
    pub from: Option<i64>,
    /// Before (Unix seconds).
    pub to: Option<i64>,
    /// Text contained in the details, event, resource id or user agent
    /// (case-insensitive).
    pub text: Option<String>,
    /// Oldest first (default: newest first).
    #[serde(default)]
    pub ascending: bool,
    /// Continue after this row id in the listing's order.
    pub cursor: Option<i64>,
    /// Page size (default 50, at most [`AUDIT_PAGE_MAX`]).
    pub limit: Option<u32>,
}

/// One page of audit entries.
#[derive(Debug, Clone, Serialize)]
pub struct AuditPage {
    /// The entries in the listing's order.
    pub items: Vec<AuditEntry>,
    /// Row id to pass as `cursor` for the next page; `None` after the last one.
    pub next_cursor: Option<i64>,
}

/// The events around one audit entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRelated {
    /// The entry itself.
    pub event: AuditEntry,
    /// Other entries of the same request, oldest first.
    pub same_request: Vec<AuditEntry>,
    /// Other entries of the same actor within the window, oldest first.
    pub same_actor: Vec<AuditEntry>,
}

/// Counts of an audit time range.
#[derive(Debug, Clone, Serialize)]
pub struct AuditSummary {
    /// Entries in the range.
    pub total: i64,
    /// Entries per outcome.
    pub outcomes: Vec<(String, i64)>,
    /// The most frequent events.
    pub events: Vec<(String, i64)>,
    /// The most active actors.
    pub actors: Vec<(String, i64)>,
    /// The most frequent client addresses.
    pub ips: Vec<(String, i64)>,
}

const COLUMNS: &str = "id, at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, impersonator";

fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl AuditSearch {
    /// `WHERE` conditions and their parameters, numbered from `?1`.
    fn conditions(&self, realm: Option<&str>) -> (Vec<String>, Vec<Value>) {
        let mut c: Vec<String> = Vec::new();
        let mut p: Vec<Value> = Vec::new();
        let mut add = |sql: &str, v: Value| {
            p.push(v);
            c.push(sql.replace('#', &p.len().to_string()));
        };
        if let Some(r) = realm {
            add("realm = ?#", t(r));
        }
        if let Some(a) = &self.actor {
            add("actor = ?#", t(a));
        }
        if let Some(a) = &self.actor_type {
            add("actor LIKE ?# ESCAPE '\\'", t(format!("{}:%", like_escape(a))));
        }
        if let Some(a) = &self.action {
            add("event LIKE ?# ESCAPE '\\'", t(format!("{}%", like_escape(a))));
        }
        if let Some(a) = &self.resource_type {
            add("subject_type = ?#", t(a));
        }
        if let Some(a) = &self.collection {
            add("collection = ?#", t(a));
        }
        if let Some(a) = &self.resource_id {
            add("subject_id = ?#", t(a));
        }
        if let Some(a) = &self.outcome {
            add("outcome = ?#", t(a));
        }
        if let Some(a) = &self.ip {
            add("ip = ?#", t(a));
        }
        if let Some(a) = &self.request_id {
            add("request_id = ?#", t(a));
        }
        if let Some(a) = self.from {
            add("at >= ?#", i(a));
        }
        if let Some(a) = self.to {
            add("at < ?#", i(a));
        }
        if let Some(a) = self.text.as_deref().filter(|s| !s.is_empty()) {
            // One parameter used four times keeps the numbering simple.
            p.push(t(format!("%{}%", like_escape(&a.to_lowercase()))));
            let n = p.len();
            c.push(format!(
                "(lower(CAST(detail AS TEXT)) LIKE ?{n} ESCAPE '\\' OR lower(event) LIKE ?{n} ESCAPE '\\' \
                 OR lower(COALESCE(subject_id, '')) LIKE ?{n} ESCAPE '\\' OR lower(COALESCE(user_agent, '')) LIKE ?{n} ESCAPE '\\')"
            ));
        }
        (c, p)
    }
}

impl AuthService {
    /// Searches the audit trail (see the module docs for who authorises).
    ///
    /// # Errors
    /// A database error.
    pub async fn audit_search(&self, realm: Option<&str>, q: &AuditSearch) -> Result<AuditPage, AuthError> {
        let limit = q.limit.unwrap_or(50).clamp(1, AUDIT_PAGE_MAX);
        let (mut conds, mut params) = q.conditions(realm);
        if let Some(cur) = q.cursor {
            params.push(i(cur));
            conds.push(format!("id {} ?{}", if q.ascending { ">" } else { "<" }, params.len()));
        }
        params.push(i(i64::from(limit) + 1));
        let sql = format!(
            "SELECT {COLUMNS} FROM _forgebase_auth_audit {} ORDER BY id {} LIMIT ?{}",
            if conds.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conds.join(" AND "))
            },
            if q.ascending { "ASC" } else { "DESC" },
            params.len()
        );
        let r = read(self.db(), &sql, params).await?;
        let mut items: Vec<AuditEntry> = rows(&r).map(audit_entry).collect();
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = if more { items.last().map(|e| e.id) } else { None };
        Ok(AuditPage { items, next_cursor })
    }

    /// One audit entry.
    ///
    /// # Errors
    /// A database error.
    pub async fn audit_entry_by_id(&self, realm: Option<&str>, id: i64) -> Result<Option<AuditEntry>, AuthError> {
        let r = read(
            self.db(),
            &format!(
                "SELECT {COLUMNS} FROM _forgebase_auth_audit
                 WHERE id = ?1 AND (CAST(?2 AS TEXT) IS NULL OR realm = ?2)"
            ),
            vec![i(id), ot(realm.map(str::to_owned))],
        )
        .await?;
        Ok(store::first(&r).map(audit_entry))
    }

    /// The entries around entry `id`: those of its request, and those of the
    /// same actor within `window_secs` before and after it (at most `limit`
    /// each).
    ///
    /// # Errors
    /// A database error.
    pub async fn audit_related(
        &self,
        realm: Option<&str>,
        id: i64,
        window_secs: i64,
        limit: u32,
    ) -> Result<Option<AuditRelated>, AuthError> {
        let Some(event) = self.audit_entry_by_id(realm, id).await? else {
            return Ok(None);
        };
        let limit = i64::from(limit.clamp(1, 500));
        let at = time::OffsetDateTime::parse(&event.at, &time::format_description::well_known::Rfc3339)
            .map_or(0, time::OffsetDateTime::unix_timestamp);
        let realm_p = ot(realm.map(str::to_owned));
        let same_request = match &event.request_id {
            Some(rid) => {
                let r = read(
                    self.db(),
                    &format!(
                        "SELECT {COLUMNS} FROM _forgebase_auth_audit
                         WHERE request_id = ?1 AND id <> ?2 AND (CAST(?3 AS TEXT) IS NULL OR realm = ?3)
                         ORDER BY id ASC LIMIT ?4"
                    ),
                    vec![t(rid), i(id), realm_p.clone(), i(limit)],
                )
                .await?;
                rows(&r).map(audit_entry).collect()
            }
            None => Vec::new(),
        };
        let r = read(
            self.db(),
            &format!(
                "SELECT {COLUMNS} FROM _forgebase_auth_audit
                 WHERE actor = ?1 AND id <> ?2 AND at >= ?3 AND at <= ?4
                   AND (CAST(?5 AS TEXT) IS NULL OR request_id IS NULL OR request_id <> ?5)
                   AND (CAST(?6 AS TEXT) IS NULL OR realm = ?6)
                 ORDER BY id ASC LIMIT ?7"
            ),
            vec![
                t(&event.actor),
                i(id),
                i(at - window_secs.max(0)),
                i(at + window_secs.max(0)),
                ot(event.request_id.clone()),
                realm_p,
                i(limit),
            ],
        )
        .await?;
        let same_actor = rows(&r).map(audit_entry).collect();
        Ok(Some(AuditRelated {
            event,
            same_request,
            same_actor,
        }))
    }

    /// Counts of the entries in `[from, to)`: by outcome, event, actor and
    /// client address (the top 10 of each).
    ///
    /// # Errors
    /// A database error.
    pub async fn audit_summary(
        &self,
        realm: Option<&str>,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<AuditSummary, AuthError> {
        let params = || {
            vec![
                from.map_or(Value::Null, i),
                to.map_or(Value::Null, i),
                ot(realm.map(str::to_owned)),
            ]
        };
        const WHERE: &str = "(CAST(?1 AS BIGINT) IS NULL OR at >= ?1) AND (CAST(?2 AS BIGINT) IS NULL OR at < ?2)
             AND (CAST(?3 AS TEXT) IS NULL OR realm = ?3)";
        let r = read(
            self.db(),
            &format!("SELECT COUNT(*) AS n FROM _forgebase_auth_audit WHERE {WHERE}"),
            params(),
        )
        .await?;
        let total = store::first(&r).map_or(0, |r| r.i64("n"));
        let group = |col: &'static str, top: bool| {
            let sql = format!(
                "SELECT {col} AS k, COUNT(*) AS n FROM _forgebase_auth_audit WHERE {WHERE} AND {col} IS NOT NULL
                 GROUP BY {col} ORDER BY n DESC, k ASC LIMIT {}",
                if top { 10 } else { 5 }
            );
            let db = self.db().clone();
            let p = params();
            async move {
                let r = read(&db, &sql, p).await?;
                Ok::<_, AuthError>(rows(&r).map(|r| (r.str("k"), r.i64("n"))).collect::<Vec<_>>())
            }
        };
        Ok(AuditSummary {
            total,
            outcomes: group("outcome", false).await?,
            events: group("event", true).await?,
            actors: group("actor", true).await?,
            ips: group("ip", true).await?,
        })
    }
}
