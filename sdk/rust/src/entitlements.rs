//! Entitlements and quotas: `has()`, usage, reservations and leases.

use std::future::Future;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{Consumption, Decision, Entitlements, Lease, Reservation, UsageSummary};
use crate::http::{Req, Transport, seg};

/// What to ask [`EntitlementsApi::has`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HasQuery {
    /// Does the subject's plan grant this feature (and is quota left)?
    Feature(String),
    /// Does the caller hold this permission?
    Permission(String),
    /// Is the subject on this plan?
    Plan(String),
}

/// A quota question.
#[derive(Debug, Clone, Default)]
pub struct QuotaRequest {
    /// Feature key.
    pub feature: String,
    /// Units (default 1).
    pub units: Option<i64>,
    /// Counter for per-user / team / resource quotas.
    pub scope_key: Option<String>,
    /// Billed subject (default: the caller's), e.g. `org:<id>`.
    pub subject: Option<String>,
    /// Free-form metadata stored with the usage event.
    pub metadata: Option<Value>,
}

impl QuotaRequest {
    /// One unit of `feature` for the caller.
    #[must_use]
    pub fn new(feature: impl Into<String>) -> Self {
        Self {
            feature: feature.into(),
            ..Self::default()
        }
    }
}

/// A usage-recording request: the idempotency key makes a replay return the first result.
#[derive(Debug, Clone, Default)]
pub struct MeteredRequest {
    /// The quota question.
    pub quota: QuotaRequest,
    /// Required for consume / reserve / lease.
    pub idempotency_key: String,
    /// Seconds a reservation or lease is held (server default when `None`).
    pub ttl_secs: Option<i64>,
}

impl MeteredRequest {
    /// `units` of `feature` under `idempotency_key`.
    #[must_use]
    pub fn new(feature: impl Into<String>, idempotency_key: impl Into<String>) -> Self {
        Self {
            quota: QuotaRequest::new(feature),
            idempotency_key: idempotency_key.into(),
            ttl_secs: None,
        }
    }
}

fn wire(q: &QuotaRequest, key: Option<&str>, ttl: Option<i64>) -> Value {
    json!({"feature": q.feature, "units": q.units, "scope_key": q.scope_key, "subject": q.subject,
        "metadata": q.metadata, "idempotency_key": key, "ttl_secs": ttl})
}

/// Entitlement and quota helpers: `client.entitlements()`.
#[derive(Clone)]
pub struct EntitlementsApi {
    pub(crate) t: Arc<Transport>,
}

impl EntitlementsApi {
    /// The boolean answer to [`HasQuery`].
    pub async fn has(&self, q: HasQuery, subject: Option<&str>) -> Result<bool> {
        Ok(self.has_detailed(q, subject).await?.has)
    }

    /// The answer with the reason when it is no.
    pub async fn has_detailed(&self, q: HasQuery, subject: Option<&str>) -> Result<crate::generated::HasAnswer> {
        let r = Req::get("/api/v1/entitlements/has");
        let r = match q {
            HasQuery::Feature(f) => r.q("feature", f).q_opt("subject", subject),
            HasQuery::Permission(p) => r.q("permission", p),
            HasQuery::Plan(p) => r.q("plan", p).q_opt("subject", subject),
        };
        self.t.json(r).await
    }

    /// Effective plan and grants of the caller (or of `subject`, e.g. `org:<id>`).
    pub async fn get(&self, subject: Option<&str>) -> Result<Entitlements> {
        self.t
            .json(Req::get("/api/v1/entitlements").q_opt("subject", subject))
            .await
    }

    /// Usage of every metered feature.
    pub async fn usage(&self, subject: Option<&str>, scope_key: Option<&str>) -> Result<Vec<UsageSummary>> {
        let v = self
            .t
            .value(
                Req::get("/api/v1/usage")
                    .q_opt("subject", subject)
                    .q_opt("scope_key", scope_key),
            )
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("usage: {e}")))
    }

    /// Would these units be granted right now? No side effects.
    pub async fn check_quota(&self, q: &QuotaRequest) -> Result<Decision> {
        self.t
            .json(
                Req::post("/api/v1/quotas/check")
                    .json_value(wire(q, None, None))
                    .idempotent(),
            )
            .await
    }

    /// Records usage at once. A replayed key returns the first result with `replayed: true`.
    /// Fails with a quota error past a hard limit.
    pub async fn consume(&self, r: &MeteredRequest) -> Result<Consumption> {
        self.t
            .json(
                Req::post("/api/v1/usage/consume")
                    .json_value(wire(&r.quota, Some(&r.idempotency_key), None))
                    .idempotent(),
            )
            .await
    }

    /// Holds units until [`commit_reservation`](Self::commit_reservation), [`release_reservation`](Self::release_reservation) or the TTL.
    pub async fn reserve(&self, r: &MeteredRequest) -> Result<Reservation> {
        self.t
            .json(
                Req::post("/api/v1/usage/reservations")
                    .json_value(wire(&r.quota, Some(&r.idempotency_key), r.ttl_secs))
                    .idempotent(),
            )
            .await
    }

    /// Commits a reservation (all held units, or `units`).
    pub async fn commit_reservation(&self, id: &str, units: Option<i64>) -> Result<Reservation> {
        self.t
            .json(
                Req::post(format!("/api/v1/usage/reservations/{}/commit", seg(id))).json_value(json!({"units": units})),
            )
            .await
    }

    /// Gives the held units back.
    pub async fn release_reservation(&self, id: &str) -> Result<Reservation> {
        self.t
            .json(Req::post(format!("/api/v1/usage/reservations/{}/release", seg(id))).json_value(json!({})))
            .await
    }

    /// Reserves, runs `f`, then commits; releases the hold when `f` fails.
    pub async fn with_reservation<T, F, Fut>(&self, r: &MeteredRequest, f: F) -> Result<T>
    where
        F: FnOnce(Reservation) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let res = self.reserve(r).await?;
        let id = res.id.clone();
        match f(res).await {
            Ok(v) => {
                self.commit_reservation(&id, None).await?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.release_reservation(&id).await;
                Err(e)
            }
        }
    }

    /// Concurrency lease (e.g. "at most 5 running jobs").
    pub async fn acquire_lease(&self, r: &MeteredRequest) -> Result<Lease> {
        self.t
            .json(
                Req::post("/api/v1/usage/leases")
                    .json_value(wire(&r.quota, Some(&r.idempotency_key), r.ttl_secs))
                    .idempotent(),
            )
            .await
    }

    /// Extends a lease.
    pub async fn renew_lease(&self, id: &str, ttl_secs: Option<i64>) -> Result<Lease> {
        self.t
            .json(
                Req::post(format!("/api/v1/usage/leases/{}/renew", seg(id))).json_value(json!({"ttl_secs": ttl_secs})),
            )
            .await
    }

    /// Releases a lease.
    pub async fn release_lease(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/usage/leases/{}", seg(id))))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape() {
        let q = QuotaRequest {
            feature: "ai".into(),
            units: Some(2),
            subject: Some("org:1".into()),
            ..QuotaRequest::default()
        };
        let w = wire(&q, Some("k"), Some(30));
        assert_eq!(w["feature"], "ai");
        assert_eq!(w["units"], 2);
        assert_eq!(w["idempotency_key"], "k");
        assert_eq!(w["ttl_secs"], 30);
        assert!(w["scope_key"].is_null());
    }
}
