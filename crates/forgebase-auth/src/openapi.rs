//! OpenAPI fragment for the social login and passkey routes (FB-22) and the
//! realm, phone, LDAP and account lifecycle routes (FB-25).
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

#[derive(OpenApi)]
#[openapi(
    paths(
        core_routes::auth_signup, core_routes::auth_login, core_routes::auth_refresh, core_routes::auth_logout, core_routes::auth_me, core_routes::auth_verify_email_request, core_routes::auth_verify_email_confirm, core_routes::auth_password_reset_request, core_routes::auth_password_reset_confirm, core_routes::auth_magic_link_request, core_routes::auth_magic_link_confirm, core_routes::auth_otp_request, core_routes::auth_otp_verify, core_routes::auth_mfa_verify, core_routes::auth_mfa_totp_setup, core_routes::auth_mfa_totp_confirm, core_routes::auth_mfa_totp_disable, core_routes::auth_mfa_recovery_codes, core_routes::auth_password_change, core_routes::auth_sessions_list, core_routes::auth_sessions_revoke, core_routes::auth_login_history_list, core_routes::auth_tokens_list, core_routes::auth_tokens_create, core_routes::auth_tokens_revoke, core_routes::auth_admins_login, core_routes::auth_admins_list, core_routes::auth_admins_create, core_routes::auth_service_accounts_list, core_routes::auth_service_accounts_create, core_routes::auth_service_accounts_get, core_routes::auth_service_accounts_delete, core_routes::auth_service_account_keys_list, core_routes::auth_service_account_keys_create, core_routes::auth_api_keys_revoke, core_routes::auth_api_keys_list, core_routes::auth_users_get, core_routes::auth_user_sessions_list, core_routes::auth_user_sessions_revoke, core_routes::auth_user_login_history_list, core_routes::auth_impersonate, core_routes::auth_impersonations_list, core_routes::auth_impersonations_end, core_routes::auth_audit_list, core_routes::auth_keys_rotate,
        oauth_providers, oauth_start_redirect, oauth_start, oauth_link, oauth_callback_get, oauth_callback_post, oauth_identities, oauth_unlink, webauthn_register_options, webauthn_register_verify, webauthn_login_options, webauthn_login_verify, webauthn_mfa_options, webauthn_mfa_verify, webauthn_credentials, webauthn_delete,
        realm_info, realms_list, realm_get, realm_put, phone_request, phone_verify, phone_get, phone_delete, phone_add, phone_confirm, ldap_login, account_deactivate, account_delete, account_export, user_status, user_suspend, user_unsuspend, user_deactivate, user_reactivate, user_anonymise, user_delete, user_export
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
    )
)]
struct AuthApiDoc;

/// The OpenAPI fragment for `/api/auth/oauth/*` and `/api/auth/webauthn/*`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    AuthApiDoc::openapi()
}
