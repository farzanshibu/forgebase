//! Documentation stubs for the FIX-4 realm administration routes in
//! `identity_http.rs`. Nothing here changes behaviour.

/// Current and retired OIDC signing key ids of a realm: `{realm, current, items:[{kid, alg, status: active|retired, created_at, verify_until, verifying}]}` (administrators; no key material).
#[utoipa::path(get, path = "/api/auth/realms/{realm}/keys", operation_id = "identity_realm_keys", tag = "realms", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_realm_keys() {}

/// Remembered consents of a realm, newest first, optionally for one client: `{items:[{id, client_id, client_name, realm, collection, user_id, scopes, created_at, updated_at}], limit, offset}` (administrators).
#[utoipa::path(get, path = "/api/auth/realms/{realm}/consents", operation_id = "identity_realm_consents", tag = "oidc", params(("realm" = String, Path, description = "Realm id"), ("client_id" = Option<String>, Query, description = "Only this client"), ("limit" = Option<i64>, Query, description = "1-500, default 100"), ("offset" = Option<i64>, Query, description = "Default 0")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_realm_consents() {}

/// Revoke any consent of the realm and the token families the client holds for that user (administrators).
#[utoipa::path(delete, path = "/api/auth/realms/{realm}/consents/{id}", operation_id = "identity_realm_consent_revoke", tag = "oidc", params(("realm" = String, Path, description = "Realm id"), ("id" = String, Path, description = "Consent id")),
    responses((status = 204, description = "Done"), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_realm_consent_revoke() {}

/// Social login providers of a realm: `{items}`; secrets show as `********`; `source` is `admin` or `file` (read-only, from oauth-providers.json) (administrators).
#[utoipa::path(get, path = "/api/auth/realms/{realm}/oauth-providers", operation_id = "identity_provider_list", tag = "oauth", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_provider_list() {}

/// One social login provider (administrators).
#[utoipa::path(get, path = "/api/auth/realms/{realm}/oauth-providers/{id}", operation_id = "identity_provider_get", tag = "oauth", params(("realm" = String, Path, description = "Realm id"), ("id" = String, Path, description = "Provider id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_provider_get() {}

/// Create or replace a provider: `{kind, client_id, client_secret?, scopes?, issuer?, authorize_url?, token_url?, userinfo_url?, jwks_url?, emails_url?, redirect_uri?, apple?: {team_id, key_id, private_key_pem?}, allow_signup?, auto_link_verified_email?, display_name?}`. Secrets are write-only and sealed: a string sets, `""` clears, `"********"`/null/absent keep. Providers defined in oauth-providers.json answer 409 (administrators).
#[utoipa::path(put, path = "/api/auth/realms/{realm}/oauth-providers/{id}", operation_id = "identity_provider_put", tag = "oauth", params(("realm" = String, Path, description = "Realm id"), ("id" = String, Path, description = "Provider id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_provider_put() {}

/// Delete a provider created over REST (administrators).
#[utoipa::path(delete, path = "/api/auth/realms/{realm}/oauth-providers/{id}", operation_id = "identity_provider_delete", tag = "oauth", params(("realm" = String, Path, description = "Realm id"), ("id" = String, Path, description = "Provider id")),
    responses((status = 204, description = "Done"), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_provider_delete() {}

/// SMS gateway of a realm, secrets masked: `{realm, configured, config}`; `configured: false` means the instance gateway is used (administrators).
#[utoipa::path(get, path = "/api/auth/realms/{realm}/sms", operation_id = "identity_sms_get", tag = "phone", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_sms_get() {}

/// Set the realm's SMS gateway: `{provider: dev}`, `{provider: twilio, account_sid, auth_token, from?, messaging_service_sid?, base_url?}` or `{provider: webhook, url, bearer_token?}`. Secrets are write-only (as for providers). Takes effect at once (administrators).
#[utoipa::path(put, path = "/api/auth/realms/{realm}/sms", operation_id = "identity_sms_put", tag = "phone", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_sms_put() {}

/// Remove the realm's SMS gateway; the instance gateway applies again (administrators).
#[utoipa::path(delete, path = "/api/auth/realms/{realm}/sms", operation_id = "identity_sms_delete", tag = "phone", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 204, description = "Done"), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 404, description = "Not found", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_sms_delete() {}

/// Create a realm and its identity settings in one call: `{id, name?, collection?, config?}` (same `config` as `PUT /api/auth/realms/{realm}`). Answers the realm identity. Settings are validated before anything is written (administrators).
#[utoipa::path(post, path = "/api/auth/realms", operation_id = "identity_realm_create", tag = "realms", request_body = Object,
    responses((status = 201, description = "Created; the realm identity", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in", body = Object), (status = 403, description = "Administrator access required", body = Object), (status = 409, description = "Realm or collection already exists", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn identity_realm_create() {}
