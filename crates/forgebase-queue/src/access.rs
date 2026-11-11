//! Tenancy, resource permissions, subscription owners and record views
//! (FB-141, `rls-coverage.md` G2 and G16).
//!
//! **Names.** A caller with a tenant creates topics, subscriptions and
//! snapshots of that tenant, stored as `<tenant>/<name>`. Queue names never
//! hold `/` and every name is validated before lookup, so a stored name is
//! never addressable: a name resolves to the caller's tenant resource, else
//! to the instance-wide one of that name. Responses show the bare name and
//! the `tenant`.
//!
//! **Permissions.** Every call asks
//! [`Authorizer::authorize_resource`](forgebase_data::Authorizer::authorize_resource)
//! about the topic or subscription it acts on (FB-134):
//! `engine:queue_<action>:<name>` or the plain key, else the resource rule of
//! the operation. A subscription's creator owns it and holds every
//! permission on it (an explicit deny still wins).
//!
//! **Record views.** A message relayed from a record change keeps the
//! changed row ([`source_row`]); its stored `data.record` is null. When it is
//! delivered, the record is evaluated as the subscription's owner (the
//! [`OwnerContext`] recorded when it was created, resolved again at each
//! delivery): the owner's current View rule, tenant and field mask decide
//! what the message carries, and a message whose record the owner cannot see
//! is withheld (its delivery is dropped). Browsing a topic evaluates it as
//! the reader.

use forgebase_data::{
    AccessContext, Action, DataError, Decision, GetOptions, Principal, ResourceKind, ResourceOp, ResourceRef,
    RowSnapshot,
};
use forgebase_db::Statement;
use serde_json::{Value as Json, json};

use crate::error::QueueError;
use crate::row::Row;
use crate::service::{Db, QueueService, Result};

/// `source_row` of record-change messages stored before FB-141: their
/// `data.record` is the full system view, so only privileged owners and
/// readers get them.
pub(crate) const LEGACY_SOURCE: &str = "legacy";

/// The stored name of `name` in `tenant` (`None`: instance-wide).
pub(crate) fn stored(tenant: Option<&str>, name: &str) -> String {
    match tenant {
        Some(t) => format!("{t}/{name}"),
        None => name.to_owned(),
    }
}

/// The name a caller uses for a stored name.
pub(crate) fn bare(stored: &str) -> &str {
    stored.rsplit_once('/').map_or(stored, |(_, n)| n)
}

/// The changed row of a record-change message, as stored in `source_row`.
pub(crate) fn source_row(ev: &forgebase_realtime::Event) -> String {
    json!({
        "collection_id": ev.collection_id,
        "tenant": ev.tenant,
        "snapshot": ev.snapshot.to_json(),
    })
    .to_string()
}

/// The access context a subscription was created with: whose view of
/// record-change messages it delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnerContext {
    /// Principal key (`user:<collection>/<id>`, `service:<id>`, `admin:<id>`, `system`).
    pub(crate) principal: String,
    pub(crate) realm: String,
    pub(crate) org: Option<String>,
    pub(crate) tenant: Option<String>,
    /// Scopes of the API key or token used to create the subscription.
    pub(crate) scopes: Option<Vec<String>>,
}

impl OwnerContext {
    /// The context of `ctx`.
    pub(crate) fn of(ctx: &AccessContext) -> Self {
        Self {
            principal: ctx.principal_key(),
            realm: ctx.realm_id.clone(),
            org: ctx.org_id.clone(),
            tenant: ctx.tenant.clone(),
            scopes: ctx.scopes.clone(),
        }
    }

    /// Stored JSON form.
    pub(crate) fn to_text(&self) -> String {
        json!({
            "principal": self.principal,
            "realm": self.realm,
            "org": self.org,
            "tenant": self.tenant,
            "scopes": self.scopes,
        })
        .to_string()
    }

    /// The stored context, or (subscriptions created before FB-141) the
    /// creator in the default realm.
    pub(crate) fn parse(text: Option<&str>, created_by: &str) -> Result<Self> {
        let Some(text) = text else {
            return Ok(Self {
                principal: created_by.to_owned(),
                realm: forgebase_data::DEFAULT_REALM.to_owned(),
                org: None,
                tenant: None,
                scopes: None,
            });
        };
        let j: Json =
            serde_json::from_str(text).map_err(|e| QueueError::Internal(format!("subscription owner: {e}")))?;
        let s = |k: &str| j.get(k).and_then(Json::as_str).map(str::to_owned);
        Ok(Self {
            principal: s("principal").ok_or_else(|| QueueError::Internal("subscription owner: no principal".into()))?,
            realm: s("realm").unwrap_or_else(|| forgebase_data::DEFAULT_REALM.to_owned()),
            org: s("org"),
            tenant: s("tenant"),
            scopes: j
                .get("scopes")
                .and_then(Json::as_array)
                .map(|a| a.iter().filter_map(Json::as_str).map(str::to_owned).collect()),
        })
    }
}

/// The error for a refused queue call.
pub(crate) fn refusal(ctx: &AccessContext, action: Action, kind: ResourceKind, name: &str) -> QueueError {
    if ctx.principal == Principal::Anonymous {
        return DataError::Unauthenticated.into();
    }
    let what = match kind {
        ResourceKind::Subscription => "subscription",
        _ => "topic",
    };
    DataError::Forbidden(format!("{action:?} on {what} {name:?} is not allowed")).into()
}

impl QueueService {
    /// The resource check (FB-134): `engine:queue_<action>:<name>` or the
    /// plain key, the owner's implicit grant, else the resource rule for
    /// `op`. Queue rules read `@request` only (no item fields), so a filter
    /// is a denial.
    pub(crate) fn check(
        &self,
        ctx: &AccessContext,
        action: Action,
        kind: ResourceKind,
        name: &str,
        op: ResourceOp,
        owner: Option<&str>,
    ) -> Result<()> {
        let mut r = ResourceRef::new(kind, name, op);
        if let Some(o) = owner {
            r = r.with_owner(o);
        }
        match self.inner.data.authorizer().authorize_resource(ctx, action, &r) {
            Decision::Allow => Ok(()),
            _ => Err(refusal(ctx, action, kind, name)),
        }
    }

    /// The owner's access context as it is now: `None` when the owner no
    /// longer exists (a deleted user or administrator, a deleted or disabled
    /// service account), which withholds every record-change message.
    pub(crate) async fn owner_access(&self, db: &mut Db<'_>, oc: &OwnerContext) -> Result<Option<AccessContext>> {
        let p = oc.principal.as_str();
        let base = if p == "system" {
            AccessContext::system()
        } else if let Some(id) = p.strip_prefix("admin:") {
            let rows = db
                .fetch(&Statement::new(
                    "SELECT 1 FROM _forgebase_admins WHERE id = ?1",
                    vec![id.into()],
                ))
                .await?;
            if rows.rows.is_empty() {
                return Ok(None);
            }
            AccessContext::admin(id)
        } else if let Some(id) = p.strip_prefix("service:") {
            let rows = db
                .fetch(&Statement::new(
                    "SELECT role FROM _forgebase_service_accounts WHERE id = ?1 AND disabled_at IS NULL",
                    vec![id.into()],
                ))
                .await?;
            let Some(r) = Row::all(&rows).next() else {
                return Ok(None);
            };
            AccessContext::service(id, r.text("role") == "admin")
        } else if let Some((collection, id)) = p.strip_prefix("user:").and_then(|u| u.split_once('/')) {
            let rec = match self
                .inner
                .data
                .get_record(&AccessContext::system(), collection, id, &GetOptions::default())
                .await
            {
                Ok(rec) => rec,
                Err(DataError::RecordNotFound { .. } | DataError::CollectionNotFound(_)) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            let mut attributes = rec;
            attributes.remove("token_key");
            attributes.remove("_tenant");
            AccessContext::user(collection, id).with_attributes(attributes)
        } else {
            return Ok(None);
        };
        let mut ctx = base.with_realm(oc.realm.clone());
        ctx.org_id.clone_from(&oc.org);
        ctx.tenant.clone_from(&oc.tenant);
        ctx.scopes.clone_from(&oc.scopes);
        Ok(Some(ctx))
    }

    /// Fills `data.record` of a record-change message with what `ctx` may see
    /// of the changed row; `false` when `ctx` may not see it (another tenant,
    /// the View rule, a collection that no longer exists).
    pub(crate) async fn view_record(&self, ctx: &AccessContext, data: &mut Json, source: &str) -> Result<bool> {
        if source == LEGACY_SOURCE {
            return Ok(ctx.is_privileged());
        }
        let src: Json =
            serde_json::from_str(source).map_err(|e| QueueError::Internal(format!("message source row: {e}")))?;
        let (Some(collection), Some(snapshot)) = (
            src.get("collection_id").and_then(Json::as_str),
            src.get("snapshot").and_then(RowSnapshot::from_json),
        ) else {
            return Err(QueueError::Internal("message source row is malformed".into()));
        };
        let tenant = src.get("tenant").and_then(Json::as_str);
        // Defence in depth, as realtime: never cross tenants for non-privileged callers.
        if !ctx.is_privileged() && tenant.is_some() && tenant != ctx.tenant.as_deref() {
            return Ok(false);
        }
        let Some(rec) = self
            .inner
            .data
            .view_snapshot(ctx, Action::View, collection, &snapshot, None)
            .await?
        else {
            return Ok(false);
        };
        if let Some(o) = data.as_object_mut() {
            o.insert("record".into(), Json::Object(rec));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_names_round_trip() {
        assert_eq!(stored(Some("env1"), "orders"), "env1/orders");
        assert_eq!(stored(None, "orders"), "orders");
        assert_eq!(bare("env1/orders"), "orders");
        assert_eq!(bare("orders"), "orders");
    }

    #[test]
    fn owner_context_round_trips() {
        let ctx = AccessContext::user("users", "u1")
            .with_realm("r1")
            .with_org("o1")
            .with_tenant("t1")
            .with_scopes(vec!["queues:consume".into()]);
        let oc = OwnerContext::of(&ctx);
        assert_eq!(OwnerContext::parse(Some(&oc.to_text()), "x").unwrap(), oc);
        let legacy = OwnerContext::parse(None, "admin:root").unwrap();
        assert_eq!(legacy.principal, "admin:root");
        assert_eq!(legacy.tenant, None);
    }
}
