//! Social login: OAuth 2.0 / OpenID Connect providers (FB-22, ADR-0009).
//!
//! * [`registry`]: provider configuration keyed by `(realm, id)`.
//! * [`flow`]: authorization code + PKCE, state and nonce, token exchange,
//!   profile lookup, account linking, sessions via `AuthService::issue_session`.
//! * [`idtoken`]: ID-token validation with a JWKS cache; [`sig`]: RS256/ES256.
//! * [`http_client`]: outbound HTTP behind a trait.

pub mod flow;
pub mod http;
pub mod http_client;
pub mod idtoken;
pub mod registry;
pub mod sig;

use std::sync::{Arc, RwLock};

pub use flow::{IdentityView, OAuthCallback, OAuthOutcome, OAuthStart};
pub use http_client::{HttpClient, HttpRequest, HttpResponse, PlainHttpClient};
pub use registry::{
    AppleKey, DEFAULT_REALM, MSA_TENANT, ProviderConfig, ProviderInfo, ProviderKind, ProviderRegistry, Secret,
    TenantRule,
};

/// Provider state held by the auth service.
#[derive(Debug)]
pub struct Social {
    pub(crate) registry: ProviderRegistry,
    pub(crate) http: RwLock<Arc<dyn HttpClient>>,
    pub(crate) jwks: idtoken::JwksCache,
}

impl Default for Social {
    fn default() -> Self {
        Self {
            registry: ProviderRegistry::default(),
            http: RwLock::new(Arc::new(PlainHttpClient::default())),
            jwks: idtoken::JwksCache::default(),
        }
    }
}

impl Social {
    pub(crate) fn http(&self) -> Arc<dyn HttpClient> {
        self.http.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
