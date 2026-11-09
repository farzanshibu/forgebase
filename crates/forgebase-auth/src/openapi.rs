//! OpenAPI fragment for the social login and passkey routes (FB-22).
//! Merged into `GET /api/openapi.json` by `forgebase-api` when auth is on.

use utoipa::OpenApi;

/// List the social login providers a login page may offer (no secrets).
#[utoipa::path(get, path = "/api/auth/oauth/providers", tag = "oauth",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_providers() {}

/// Browser entry point: 302 to the provider's authorization endpoint. Query: `collection`, `cookie`, `device_name`.
#[utoipa::path(get, path = "/api/auth/oauth/{provider}/start", tag = "oauth", params(("provider" = String, Path, description = "Provider id")),
    responses((status = 302, description = "Redirect to the provider", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_start_redirect() {}

/// Start a sign-in: returns `authorization_url` and `state` (authorization code + PKCE, state and nonce stored server side, single use, 10 minutes).
#[utoipa::path(post, path = "/api/auth/oauth/{provider}/start", tag = "oauth", params(("provider" = String, Path, description = "Provider id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_start() {}

/// Start linking a provider account to the signed-in user.
#[utoipa::path(post, path = "/api/auth/oauth/{provider}/link", tag = "oauth", params(("provider" = String, Path, description = "Provider id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_link() {}

/// Provider redirect target: `code` + `state` (or `error`). Answers like `/api/auth/login`, or `{linked:true}` for a link flow.
#[utoipa::path(get, path = "/api/auth/oauth/{provider}/callback", tag = "oauth", params(("provider" = String, Path, description = "Provider id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_callback_get() {}

/// Same as GET; JSON `{code,state}` for single-page apps, or a form (Apple `form_post`).
#[utoipa::path(post, path = "/api/auth/oauth/{provider}/callback", tag = "oauth", params(("provider" = String, Path, description = "Provider id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_callback_post() {}

/// The caller's linked provider accounts.
#[utoipa::path(get, path = "/api/auth/oauth/identities", tag = "oauth",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_identities() {}

/// Unlink a provider account (refused when it is the account's only way in).
#[utoipa::path(delete, path = "/api/auth/oauth/identities/{id}", tag = "oauth", params(("id" = String, Path, description = "Identity or credential id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn oauth_unlink() {}

/// Start registering a passkey (signed-in user). Body `{use_as_mfa?}`. Returns `ceremony_id` and `publicKey` (creation options JSON).
#[utoipa::path(post, path = "/api/auth/webauthn/register/options", tag = "webauthn", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_register_options() {}

/// Finish registration: `{ceremony_id, credential, name?}` (RegistrationResponseJSON).
#[utoipa::path(post, path = "/api/auth/webauthn/register/verify", tag = "webauthn", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_register_verify() {}

/// Start a passkey sign-in; optional `{collection?, email?}` (an unknown email gets a stable fake allow list).
#[utoipa::path(post, path = "/api/auth/webauthn/login/options", tag = "webauthn", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_login_options() {}

/// Finish a passkey sign-in (user verification required): `{ceremony_id, credential, cookie?, device_name?}`. Answers like `/api/auth/login`.
#[utoipa::path(post, path = "/api/auth/webauthn/login/verify", tag = "webauthn", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_login_verify() {}

/// Options to answer an MFA challenge with a passkey registered `use_as_mfa`: `{mfa_token}`.
#[utoipa::path(post, path = "/api/auth/webauthn/mfa/options", tag = "webauthn", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_mfa_options() {}

/// Finish an MFA challenge with a passkey: `{mfa_token, ceremony_id, credential}`.
#[utoipa::path(post, path = "/api/auth/webauthn/mfa/verify", tag = "webauthn", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_mfa_verify() {}

/// The caller's passkeys.
#[utoipa::path(get, path = "/api/auth/webauthn/credentials", tag = "webauthn",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_credentials() {}

/// Delete one of the caller's passkeys (refused when it is the account's only way in).
#[utoipa::path(delete, path = "/api/auth/webauthn/credentials/{id}", tag = "webauthn", params(("id" = String, Path, description = "Identity or credential id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Invalid credentials, state or ceremony", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn webauthn_delete() {}

#[derive(OpenApi)]
#[openapi(
    paths(
        oauth_providers, oauth_start_redirect, oauth_start, oauth_link, oauth_callback_get, oauth_callback_post, oauth_identities, oauth_unlink, webauthn_register_options, webauthn_register_verify, webauthn_login_options, webauthn_login_verify, webauthn_mfa_options, webauthn_mfa_verify, webauthn_credentials, webauthn_delete
    ),
    tags(
        (name = "oauth", description = "Social login: OAuth 2.0 / OpenID Connect providers (ADR-0009)"),
        (name = "webauthn", description = "Passkeys: WebAuthn registration and authentication (ADR-0009)"),
    )
)]
struct AuthApiDoc;

/// The OpenAPI fragment for `/api/auth/oauth/*` and `/api/auth/webauthn/*`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    AuthApiDoc::openapi()
}
