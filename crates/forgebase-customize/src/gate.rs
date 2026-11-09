//! The re-accept gate: a [`RequestGate`] that refuses requests of signed-in
//! users who have not accepted the current version of every agreement of
//! their realm.
//!
//! Exempt: the paths users need to sign in, read and accept the agreements
//! (`/api/auth/`, `/api/v1/customize/`), health and the API description.
//! Administrators, service accounts, API keys of those, the system and
//! impersonation sessions are not gated; anonymous requests have no one to
//! hold to an agreement. The answer is `403 agreement_required` with the
//! pending versions in `details.pending`.

use forgebase_api::ApiError;
use forgebase_api::gate::{GateDecision, RequestGate};
use forgebase_data::{AccessContext, Principal};
use serde_json::json;

use crate::service::CustomizeService;

/// Path prefixes the gate lets through.
pub const EXEMPT_PREFIXES: [&str; 6] = [
    "/api/auth/",
    "/api/v1/customize/",
    "/health",
    "/metrics",
    "/api/openapi.json",
    "/api/system/info",
];

/// The gate. Build with [`AgreementGate::new`].
#[derive(Debug, Clone)]
pub struct AgreementGate {
    svc: CustomizeService,
}

impl AgreementGate {
    /// A gate reading `svc`'s in-memory agreement state.
    pub fn new(svc: CustomizeService) -> Self {
        Self { svc }
    }
}

impl RequestGate for AgreementGate {
    fn admit(&self, ctx: &AccessContext, path: &str) -> GateDecision {
        if !matches!(ctx.principal, Principal::User { .. }) || ctx.impersonator.is_some() {
            return GateDecision::Pass(Vec::new());
        }
        if EXEMPT_PREFIXES.iter().any(|p| path.starts_with(p)) {
            return GateDecision::Pass(Vec::new());
        }
        let pending = self.svc.pending_for(&ctx.realm_id, &ctx.principal_key());
        if pending.is_empty() {
            return GateDecision::Pass(Vec::new());
        }
        let names: Vec<String> = pending.iter().map(|p| format!("{} v{}", p.kind, p.version)).collect();
        GateDecision::Reject(
            ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "agreement_required",
                format!(
                    "accept the current legal agreements to continue ({}): GET /api/v1/customize/agreements/pending, then POST /api/v1/customize/agreements/accept",
                    names.join(", ")
                ),
            )
            .with_details(json!({"pending": pending})),
            Vec::new(),
        )
    }
}
