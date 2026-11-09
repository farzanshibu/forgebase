//! OpenAPI fragment for every `/api/auth/*` route: the auth core (FB-20),
//! social login and passkeys (FB-22), realm, phone, LDAP and account
//! lifecycle (FB-25), and the identity provider (FB-26).
//! Merged into `GET /api/openapi.json` by `forgebase-api` when auth is on.

use utoipa::OpenApi;

mod core_routes;

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

/// Public sign-in options of the realm selected by `X-ForgeBase-Realm`: methods, signup, Turnstile site key, providers.
#[utoipa::path(get, path = "/api/auth/realm", tag = "realms",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn realm_info() {}

/// Every realm identity configuration (administrators; secrets shown as `********`).
#[utoipa::path(get, path = "/api/auth/realms", tag = "realms",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn realms_list() {}

/// One realm identity configuration (administrators).
#[utoipa::path(get, path = "/api/auth/realms/{realm}", tag = "realms", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn realm_get() {}

/// Create or replace a realm's identity: `{collection?, config: {signup, methods, require_verified_email, phone, ldap, turnstile}}` (administrators; the realm must exist).
#[utoipa::path(put, path = "/api/auth/realms/{realm}", tag = "realms", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn realm_put() {}

/// Send a sign-in code by SMS: `{phone, collection?}` (same answer whether or not the number can sign in; Turnstile action `otp`).
#[utoipa::path(post, path = "/api/auth/phone/request", tag = "phone", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_request() {}

/// Sign in with `{phone, code, cookie?, device_name?}`; first use creates the account when the realm allows phone signup.
#[utoipa::path(post, path = "/api/auth/phone/verify", tag = "phone", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_verify() {}

/// The caller's phone number.
#[utoipa::path(get, path = "/api/auth/phone", tag = "phone",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_get() {}

/// Remove the caller's phone number (refused when it is the only way in).
#[utoipa::path(delete, path = "/api/auth/phone", tag = "phone",
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_delete() {}

/// Send a code confirming `{phone}` for the signed-in caller.
#[utoipa::path(post, path = "/api/auth/phone/add", tag = "phone", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_add() {}

/// Confirm `{phone, code}` and set it as the caller's number.
#[utoipa::path(post, path = "/api/auth/phone/confirm", tag = "phone", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn phone_confirm() {}

/// Sign in with a directory account: `{username, password, cookie?, device_name?}` (Turnstile action `login`).
#[utoipa::path(post, path = "/api/auth/ldap/login", tag = "ldap", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn ldap_login() {}

/// Deactivate the caller's account: `{current_password?}` (or a sign-in in the last 10 minutes).
#[utoipa::path(post, path = "/api/auth/account/deactivate", tag = "account", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn account_deactivate() {}

/// Delete the caller's account, or anonymise it with `{anonymise: true}`; `{current_password?}`.
#[utoipa::path(post, path = "/api/auth/account/delete", tag = "account", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn account_delete() {}

/// The caller's data export (JSON).
#[utoipa::path(get, path = "/api/auth/account/export", tag = "account",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn account_export() {}

/// Lifecycle state of a user (administrators).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/status", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_status() {}

/// Suspend `{reason, until?}` (Unix seconds); revokes sessions (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/suspend", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_suspend() {}

/// Lift a suspension (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/unsuspend", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_unsuspend() {}

/// Deactivate `{reason?}` (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/deactivate", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_deactivate() {}

/// Reactivate a deactivated user (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/reactivate", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_reactivate() {}

/// Anonymise: personal data replaced, credentials removed, id kept (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/anonymise", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_anonymise() {}

/// Delete the user and every auth row (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/delete", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_delete() {}

/// A user's data export (administrators).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/export", tag = "account", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden (method disabled, Turnstile, suspended, not an administrator)", body = Object), (status = 409, description = "Conflict", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn user_export() {}

/// Create an account in an auth collection: `{email, password, collection?}`; sends a verification email.
#[utoipa::path(post, path = "/api/auth/signup", tag = "auth", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_signup() {}

/// Sign in with email and password: `{email, password, collection?, cookie?, device_name?}`. Returns tokens, or `{mfa_required, mfa_token}`; `cookie: true` sets an HttpOnly session cookie instead.
#[utoipa::path(post, path = "/api/auth/login", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_login() {}

/// Exchange a refresh token for new tokens (rotation; reuse revokes the session): `{refresh_token}`.
#[utoipa::path(post, path = "/api/auth/refresh", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_refresh() {}

/// End the current session.
#[utoipa::path(post, path = "/api/auth/logout", tag = "auth",
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_logout() {}

/// The signed-in caller.
#[utoipa::path(get, path = "/api/auth/me", tag = "auth",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_me() {}

/// Send a verification email: `{email, collection?}` (always 202).
#[utoipa::path(post, path = "/api/auth/verify-email/request", tag = "auth", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_verify_request() {}

/// Confirm an email address: `{token}`.
#[utoipa::path(post, path = "/api/auth/verify-email/confirm", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_verify_confirm() {}

/// Send a password-reset email: `{email, collection?}` (always 202).
#[utoipa::path(post, path = "/api/auth/password-reset/request", tag = "auth", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_reset_request() {}

/// Set a new password with a reset token: `{token, password}`; ends every session.
#[utoipa::path(post, path = "/api/auth/password-reset/confirm", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_reset_confirm() {}

/// Send a sign-in link: `{email, collection?}` (always 202).
#[utoipa::path(post, path = "/api/auth/magic-link/request", tag = "auth", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_magic_request() {}

/// Sign in with a magic link token: `{token, cookie?, device_name?}`.
#[utoipa::path(post, path = "/api/auth/magic-link/confirm", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_magic_confirm() {}

/// Send a one-time sign-in code by email: `{email, collection?}` (always 202).
#[utoipa::path(post, path = "/api/auth/otp/request", tag = "auth", request_body = Object,
    responses((status = 202, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_otp_request() {}

/// Sign in with an email code: `{email, code, collection?, cookie?}`.
#[utoipa::path(post, path = "/api/auth/otp/verify", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_otp_verify() {}

/// Finish an MFA challenge: `{mfa_token, code}` (TOTP or recovery code).
#[utoipa::path(post, path = "/api/auth/mfa/verify", tag = "mfa", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_mfa_verify() {}

/// Start TOTP enrolment: returns the secret and `otpauth://` URL.
#[utoipa::path(post, path = "/api/auth/mfa/totp/setup", tag = "mfa",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_totp_setup() {}

/// Enable TOTP with a first code: `{code}`; returns recovery codes.
#[utoipa::path(post, path = "/api/auth/mfa/totp/confirm", tag = "mfa", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_totp_confirm() {}

/// Disable TOTP with a valid second factor: `{code}`.
#[utoipa::path(post, path = "/api/auth/mfa/totp/disable", tag = "mfa", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_totp_disable() {}

/// Replace the recovery codes: `{code}`.
#[utoipa::path(post, path = "/api/auth/mfa/recovery-codes", tag = "mfa", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_recovery_codes() {}

/// Change the password: `{current_password, new_password}`; ends other sessions.
#[utoipa::path(post, path = "/api/auth/password/change", tag = "auth", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_change_password() {}

/// The caller's sessions (device list).
#[utoipa::path(get, path = "/api/auth/sessions", tag = "sessions",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sessions() {}

/// Revoke a session.
#[utoipa::path(delete, path = "/api/auth/sessions/{id}", tag = "sessions", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_session_revoke() {}

/// The caller's sign-in history.
#[utoipa::path(get, path = "/api/auth/login-history", tag = "sessions",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_login_history() {}

/// The caller's personal access tokens.
#[utoipa::path(get, path = "/api/auth/tokens", tag = "keys",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_pats() {}

/// Create a personal access token: `{name, scopes, expires_in_days?}` (shown once).
#[utoipa::path(post, path = "/api/auth/tokens", tag = "keys", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_pat_create() {}

/// Revoke a personal access token.
#[utoipa::path(delete, path = "/api/auth/tokens/{id}", tag = "keys", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_pat_revoke() {}

/// Administrator sign-in: `{email, password}`.
#[utoipa::path(post, path = "/api/auth/admins/login", tag = "admin", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_login() {}

/// List administrators (administrators).
#[utoipa::path(get, path = "/api/auth/admins", tag = "admin",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admins() {}

/// Create an administrator (administrators): `{email, password}`.
#[utoipa::path(post, path = "/api/auth/admins", tag = "admin", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_create() {}

/// List service accounts (administrators).
#[utoipa::path(get, path = "/api/auth/service-accounts", tag = "keys",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sas() {}

/// Create a service account (administrators): `{name, role?}`.
#[utoipa::path(post, path = "/api/auth/service-accounts", tag = "keys", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sa_create() {}

/// A service account (administrators).
#[utoipa::path(get, path = "/api/auth/service-accounts/{id}", tag = "keys", params(("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sa_get() {}

/// Delete a service account and its keys (administrators).
#[utoipa::path(delete, path = "/api/auth/service-accounts/{id}", tag = "keys", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sa_delete() {}

/// API keys of a service account (administrators).
#[utoipa::path(get, path = "/api/auth/service-accounts/{id}/keys", tag = "keys", params(("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sa_keys() {}

/// Create an API key (administrators): `{name, scopes, expires_in_days?}` (shown once).
#[utoipa::path(post, path = "/api/auth/service-accounts/{id}/keys", tag = "keys", params(("id" = String, Path, description = "Id")), request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_sa_key_create() {}

/// Every API key and PAT (administrators).
#[utoipa::path(get, path = "/api/auth/api-keys", tag = "keys",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_api_keys() {}

/// Revoke an API key (administrators).
#[utoipa::path(delete, path = "/api/auth/api-keys/{id}", tag = "keys", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_api_key_revoke() {}

/// A user with status and sign-in details (administrators).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_user() {}

/// A user's sessions (administrators).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/sessions", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_user_sessions() {}

/// Revoke every session of a user (administrators).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/sessions/revoke", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_user_sessions_revoke() {}

/// A user's sign-in history (administrators).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/login-history", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_admin_user_history() {}

/// Start an impersonation session (administrators, ADR-0014): `{principal, reason, ttl_minutes?}`.
#[utoipa::path(post, path = "/api/auth/impersonate", tag = "admin", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_impersonate() {}

/// Impersonation sessions (administrators).
#[utoipa::path(get, path = "/api/auth/impersonations", tag = "admin",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_impersonations() {}

/// End an impersonation session.
#[utoipa::path(post, path = "/api/auth/impersonations/{id}/end", tag = "admin", params(("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_impersonation_end() {}

/// Auth audit log (administrators). Query: `event`, `subject_id`, `before`, `limit`.
#[utoipa::path(get, path = "/api/auth/audit", tag = "admin",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_audit() {}

/// Rotate the HS256 session-token signing key (administrators).
#[utoipa::path(post, path = "/api/auth/keys/rotate", tag = "admin",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn core_keys_rotate() {}

/// OpenID Provider metadata of a realm (OIDC Discovery 1.0).
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/.well-known/openid-configuration", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_discovery() {}

/// JSON Web Key Set of the realm (ES256 keys: active and still-verifying retired ones).
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/jwks", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_jwks() {}

/// Authorization endpoint (code + PKCE S256). Query: `response_type=code`, `client_id`, `redirect_uri` (exact match), `scope`, `state`, `nonce`, `code_challenge`, `code_challenge_method=S256`, `prompt` (none, login, consent), `max_age`, or `request_id` to resume. Redirects to the client, to the login page, or shows the consent screen (JSON with `Accept: application/json`).
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/authorize", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 302, description = "Redirect"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_authorize_get() {}

/// Authorization endpoint with form parameters (same as GET).
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/authorize", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 302, description = "Redirect"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_authorize_post() {}

/// Answer the consent screen (browser session): form or JSON `request_id`, `decision` (allow, deny), `csrf_token`. Refused in an impersonated session.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/consent", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 303, description = "Redirect"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_consent() {}

/// Token endpoint (form): `grant_type` authorization_code (`code`, `redirect_uri`, `code_verifier`), refresh_token (`refresh_token`, `scope?`), client_credentials (`scope?`), or urn:ietf:params:oauth:grant-type:device_code (`device_code`). Client authentication: Basic, or `client_id` + `client_secret`, or `client_id` alone for public clients.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/token", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_token() {}

/// UserInfo: claims of the access token's scopes (`Authorization: Bearer`).
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/userinfo", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_userinfo_get() {}

/// UserInfo with the access token in the header or as form field `access_token`.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/userinfo", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_userinfo_post() {}

/// Token introspection (RFC 7662; confidential clients of the realm): form `token`, `token_type_hint?`.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/introspect", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_introspect() {}

/// Token revocation (RFC 7009): form `token`, `token_type_hint?`. A refresh token revokes its family. Always 200.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/revoke", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_revoke() {}

/// Device authorization (RFC 8628): form `client_id`, `scope`. Returns `device_code`, `user_code`, `verification_uri`, `verification_uri_complete`, `expires_in`, `interval`.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/device_authorization", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_device_authorization() {}

/// Device verification page (browser session; login redirect otherwise). Query `user_code?`.
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/device", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_device_get() {}

/// Approve or deny a device: form or JSON `user_code`, `decision` (allow, deny), `csrf_token`.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/device", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_device_post() {}

/// RP-initiated logout: `id_token_hint`, `client_id`, `post_logout_redirect_uri` (exact match), `state`. Ends the realm's browser session (confirmation page without a matching hint).
#[utoipa::path(get, path = "/api/auth/oidc/{realm}/logout", tag = "oidc", params(("realm" = String, Path, description = "Realm id")),
    responses((status = 302, description = "Redirect"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_logout_get() {}

/// Confirmed logout: the same parameters plus `csrf_token` and `confirm=yes`.
#[utoipa::path(post, path = "/api/auth/oidc/{realm}/logout", tag = "oidc", params(("realm" = String, Path, description = "Realm id")), request_body = Object,
    responses((status = 303, description = "Redirect"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_logout_post() {}

/// Client Apps (administrators). Query `realm?`.
#[utoipa::path(get, path = "/api/auth/clients", tag = "oidc",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_clients() {}

/// Register a Client App (administrators): `{realm?, name, type (confidential, public), redirect_uris, post_logout_redirect_uris?, grant_types?, scopes?, skip_consent?, access_token_ttl?, refresh_token_ttl?}`. The secret is returned once.
#[utoipa::path(post, path = "/api/auth/clients", tag = "oidc", request_body = Object,
    responses((status = 201, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_client_create() {}

/// A Client App (administrators; never the secret).
#[utoipa::path(get, path = "/api/auth/clients/{id}", tag = "oidc", params(("id" = String, Path, description = "Id")),
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_client_get() {}

/// Change a Client App (administrators); `disabled: true` refuses new authorizations and tokens.
#[utoipa::path(patch, path = "/api/auth/clients/{id}", tag = "oidc", params(("id" = String, Path, description = "Id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_client_update() {}

/// Delete a Client App with its consents, codes and token families (administrators).
#[utoipa::path(delete, path = "/api/auth/clients/{id}", tag = "oidc", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_client_delete() {}

/// Rotate a confidential client's secret (administrators): `{grace_seconds?}` keeps the previous secret valid that long.
#[utoipa::path(post, path = "/api/auth/clients/{id}/secret", tag = "oidc", params(("id" = String, Path, description = "Id")), request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_client_secret() {}

/// The caller's remembered consents.
#[utoipa::path(get, path = "/api/auth/consents", tag = "oidc",
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_consents() {}

/// Withdraw a consent; revokes the client's tokens for the caller.
#[utoipa::path(delete, path = "/api/auth/consents/{id}", tag = "oidc", params(("id" = String, Path, description = "Id")),
    responses((status = 204, description = "Done"), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_consent_revoke() {}

/// Rotate a realm's OIDC signing key (administrators): `{realm?}`.
#[utoipa::path(post, path = "/api/auth/keys/oidc/rotate", tag = "oidc", request_body = Object,
    responses((status = 200, description = "Success", body = Object), (status = 400, description = "Invalid input", body = Object), (status = 401, description = "Not signed in or invalid credentials", body = Object), (status = 403, description = "Forbidden", body = Object)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
fn idp_keys_rotate() {}

#[derive(OpenApi)]
#[openapi(
    paths(
        core_routes::auth_signup, core_routes::auth_login, core_routes::auth_refresh, core_routes::auth_logout, core_routes::auth_me, core_routes::auth_verify_email_request, core_routes::auth_verify_email_confirm, core_routes::auth_password_reset_request, core_routes::auth_password_reset_confirm, core_routes::auth_magic_link_request, core_routes::auth_magic_link_confirm, core_routes::auth_otp_request, core_routes::auth_otp_verify, core_routes::auth_mfa_verify, core_routes::auth_mfa_totp_setup, core_routes::auth_mfa_totp_confirm, core_routes::auth_mfa_totp_disable, core_routes::auth_mfa_recovery_codes, core_routes::auth_password_change, core_routes::auth_sessions_list, core_routes::auth_sessions_revoke, core_routes::auth_login_history_list, core_routes::auth_tokens_list, core_routes::auth_tokens_create, core_routes::auth_tokens_revoke, core_routes::auth_admins_login, core_routes::auth_admins_list, core_routes::auth_admins_create, core_routes::auth_service_accounts_list, core_routes::auth_service_accounts_create, core_routes::auth_service_accounts_get, core_routes::auth_service_accounts_delete, core_routes::auth_service_account_keys_list, core_routes::auth_service_account_keys_create, core_routes::auth_api_keys_revoke, core_routes::auth_api_keys_list, core_routes::auth_users_get, core_routes::auth_user_sessions_list, core_routes::auth_user_sessions_revoke, core_routes::auth_user_login_history_list, core_routes::auth_impersonate, core_routes::auth_impersonations_list, core_routes::auth_impersonations_end, core_routes::auth_audit_list, core_routes::auth_keys_rotate,
        oauth_providers, oauth_start_redirect, oauth_start, oauth_link, oauth_callback_get, oauth_callback_post, oauth_identities, oauth_unlink, webauthn_register_options, webauthn_register_verify, webauthn_login_options, webauthn_login_verify, webauthn_mfa_options, webauthn_mfa_verify, webauthn_credentials, webauthn_delete,
        realm_info, realms_list, realm_get, realm_put, phone_request, phone_verify, phone_get, phone_delete, phone_add, phone_confirm, ldap_login, account_deactivate, account_delete, account_export, user_status, user_suspend, user_unsuspend, user_deactivate, user_reactivate, user_anonymise, user_delete, user_export,
        core_signup, core_login, core_refresh, core_logout, core_me, core_verify_request, core_verify_confirm, core_reset_request, core_reset_confirm,
        core_magic_request, core_magic_confirm, core_otp_request, core_otp_verify, core_mfa_verify, core_totp_setup, core_totp_confirm, core_totp_disable,
        core_recovery_codes, core_change_password, core_sessions, core_session_revoke, core_login_history, core_pats, core_pat_create, core_pat_revoke,
        core_admin_login, core_admins, core_admin_create, core_sas, core_sa_create, core_sa_get, core_sa_delete, core_sa_keys, core_sa_key_create,
        core_api_keys, core_api_key_revoke, core_admin_user, core_admin_user_sessions, core_admin_user_sessions_revoke, core_admin_user_history,
        core_impersonate, core_impersonations, core_impersonation_end, core_audit, core_keys_rotate, idp_discovery, idp_jwks, idp_authorize_get,
        idp_authorize_post, idp_consent, idp_token, idp_userinfo_get, idp_userinfo_post, idp_introspect, idp_revoke, idp_device_authorization, idp_device_get,
        idp_device_post, idp_logout_get, idp_logout_post, idp_clients, idp_client_create, idp_client_get, idp_client_update, idp_client_delete,
        idp_client_secret, idp_consents, idp_consent_revoke, idp_keys_rotate
    ),
    components(schemas(
        core_routes::AuthSignupRequest,
        core_routes::AuthLoginRequest,
        core_routes::AuthEmailRequest,
        core_routes::AuthTokenRequest,
        core_routes::AuthResetConfirmRequest,
        core_routes::AuthOtpVerifyRequest,
        core_routes::AuthMfaVerifyRequest,
        core_routes::AuthCodeRequest,
        core_routes::AuthRefreshRequest,
        core_routes::AuthChangePasswordRequest,
        core_routes::AuthKeyRequest,
        core_routes::AuthServiceAccountRequest,
        core_routes::AuthAdminCreateRequest,
        core_routes::AuthImpersonateRequest,
        core_routes::AuthSession,
        core_routes::AuthMfaChallenge,
        core_routes::AuthLoginResponse,
        core_routes::AuthUserEnvelope,
        core_routes::AuthAccepted,
        core_routes::AuthItems,
        core_routes::AuthTotpSetup,
        core_routes::AuthTotpEnabled,
        core_routes::AuthTotpDisabled,
        core_routes::AuthRecoveryCodes,
        core_routes::AuthKeyCreated,
        core_routes::AuthRevoked,
        core_routes::AuthKeyRotated,
    )),
    tags(
        (name = "auth", description = "Sign-up, sign-in, tokens and password flows (ADR-0004)"),
        (name = "mfa", description = "Second factor: TOTP and recovery codes"),
        (name = "sessions", description = "The caller's sessions and sign-in history"),
        (name = "tokens", description = "Personal access tokens"),
        (name = "admin-auth", description = "Administrators, service accounts, API keys, user sessions, impersonation, audit, key rotation"),
        (name = "oauth", description = "Social login: OAuth 2.0 / OpenID Connect providers (ADR-0009)"),
        (name = "webauthn", description = "Passkeys: WebAuthn registration and authentication (ADR-0009)"),
        (name = "realms", description = "Realm-scoped identity configuration (ADR-0015)"),
        (name = "phone", description = "Phone-number sign-in with SMS codes (ADR-0015)"),
        (name = "ldap", description = "LDAP directory sign-in (ADR-0015)"),
        (name = "account", description = "Account lifecycle: suspend, deactivate, delete, anonymise, export (ADR-0015)"),
        (name = "auth", description = "Sign-up, sign-in, tokens and email flows (ADR-0004)"),
        (name = "mfa", description = "TOTP and recovery codes (ADR-0004)"),
        (name = "sessions", description = "Sessions, devices and sign-in history (ADR-0004)"),
        (name = "keys", description = "Personal access tokens, service accounts and API keys (ADR-0004)"),
        (name = "admin", description = "Administrators, user inspection, impersonation, audit, key rotation (ADR-0004, ADR-0014)"),
        (name = "oidc", description = "ForgeBase as identity provider: OAuth 2.0 / OpenID Connect, Client Apps, consent, device flow (ADR-0019)"),
    )
)]
struct AuthApiDoc;

/// The OpenAPI fragment for `/api/auth/*`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    AuthApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// Every `.route(...)` of the auth routers, as `(METHOD, /api/auth/path)`.
    fn routed() -> BTreeSet<(String, String)> {
        let sources = [
            include_str!("http.rs"),
            include_str!("identity_http.rs"),
            include_str!("social/http.rs"),
            include_str!("webauthn/http.rs"),
            include_str!("idp/http.rs"),
        ];
        let mut out = BTreeSet::new();
        for src in sources {
            let mut rest = src;
            while let Some(i) = rest.find(".route(") {
                rest = &rest[i + 7..];
                let mut depth = 1;
                let mut end = 0;
                for (j, c) in rest.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = j;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let call = &rest[..end];
                let Some(q) = call.find('"') else { continue };
                let path = &call[q + 1..];
                let path = &path[..path.find('"').unwrap_or(0)];
                let after = &call[q + path.len() + 2..];
                for m in ["get", "post", "put", "patch", "delete"] {
                    let needle = format!("{m}(");
                    let hit = after.match_indices(&needle).any(|(k, _)| {
                        k == 0 || !after.as_bytes()[k - 1].is_ascii_alphanumeric() && after.as_bytes()[k - 1] != b'_'
                    });
                    if hit {
                        out.insert((m.to_uppercase(), format!("/api/auth{path}")));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn every_auth_route_is_documented() {
        let doc = serde_json::to_value(super::openapi()).unwrap();
        let mut documented = BTreeSet::new();
        for (path, item) in doc["paths"].as_object().unwrap() {
            for m in item.as_object().unwrap().keys() {
                documented.insert((m.to_uppercase(), path.clone()));
            }
        }
        let routes = routed();
        assert!(routes.len() > 100, "parsed {} routes", routes.len());
        let missing: Vec<_> = routes.difference(&documented).collect();
        assert!(missing.is_empty(), "undocumented routes: {missing:?}");
        let stale: Vec<_> = documented.difference(&routes).collect();
        assert!(stale.is_empty(), "documented but not routed: {stale:?}");
    }
}
