//! Documentation stubs for the core `/api/auth/*` routes of `http.rs` (one
//! per operation, with request and response schemas). Payloads mirror the
//! handlers; nothing here changes behaviour.

use utoipa::ToSchema;

/// Create a user account in an auth collection (`collection` defaults to the default one). Sends a verification email when configured.
#[utoipa::path(post, path = "/api/auth/signup", operation_id = "auth_signup", tag = "auth", request_body = AuthSignupRequest,
    responses((status = 201, description = "Created; `{user}`", body = AuthUserEnvelope)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_signup() {}

/// Password sign-in. Returns a session (tokens, or cookies when `cookie: true`) or, when MFA is enabled, an `mfa_required` challenge for `/api/auth/mfa/verify`.
#[utoipa::path(post, path = "/api/auth/login", operation_id = "auth_login", tag = "auth", request_body = AuthLoginRequest,
    responses((status = 200, description = "Session, or MFA challenge", body = AuthLoginResponse)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_login() {}

/// Rotate the refresh token and get a new access token.
#[utoipa::path(post, path = "/api/auth/refresh", operation_id = "auth_refresh", tag = "auth", request_body = AuthRefreshRequest,
    responses((status = 200, description = "New session tokens", body = AuthSession)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_refresh() {}

/// Revoke the caller's session (clears the session cookies for cookie sessions).
#[utoipa::path(post, path = "/api/auth/logout", operation_id = "auth_logout", tag = "auth",
    responses((status = 204, description = "Signed out")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_logout() {}

/// The authenticated principal (subject, credential kind, scopes, user record).
#[utoipa::path(get, path = "/api/auth/me", operation_id = "auth_me", tag = "auth",
    responses((status = 200, description = "Principal", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_me() {}

/// Send a verification email (always answers 202 so accounts are not revealed).
#[utoipa::path(post, path = "/api/auth/verify-email/request", operation_id = "auth_verify_email_request", tag = "auth", request_body = AuthEmailRequest,
    responses((status = 202, description = "Accepted", body = AuthAccepted)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_verify_email_request() {}

/// Confirm an email address with the emailed token.
#[utoipa::path(post, path = "/api/auth/verify-email/confirm", operation_id = "auth_verify_email_confirm", tag = "auth", request_body = AuthTokenRequest,
    responses((status = 200, description = "Verified; `{user}`", body = AuthUserEnvelope)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_verify_email_confirm() {}

/// Send a password reset email (always 202).
#[utoipa::path(post, path = "/api/auth/password-reset/request", operation_id = "auth_password_reset_request", tag = "auth", request_body = AuthEmailRequest,
    responses((status = 202, description = "Accepted", body = AuthAccepted)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_password_reset_request() {}

/// Set a new password with the emailed token; revokes the user's sessions.
#[utoipa::path(post, path = "/api/auth/password-reset/confirm", operation_id = "auth_password_reset_confirm", tag = "auth", request_body = AuthResetConfirmRequest,
    responses((status = 204, description = "Password changed")))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_password_reset_confirm() {}

/// Send a one-time sign-in link (always 202).
#[utoipa::path(post, path = "/api/auth/magic-link/request", operation_id = "auth_magic_link_request", tag = "auth", request_body = AuthEmailRequest,
    responses((status = 202, description = "Accepted", body = AuthAccepted)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_magic_link_request() {}

/// Sign in with the emailed magic-link token.
#[utoipa::path(post, path = "/api/auth/magic-link/confirm", operation_id = "auth_magic_link_confirm", tag = "auth", request_body = AuthTokenRequest,
    responses((status = 200, description = "Session, or MFA challenge", body = AuthLoginResponse)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_magic_link_confirm() {}

/// Send a one-time email code (always 202).
#[utoipa::path(post, path = "/api/auth/otp/request", operation_id = "auth_otp_request", tag = "auth", request_body = AuthEmailRequest,
    responses((status = 202, description = "Accepted", body = AuthAccepted)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_otp_request() {}

/// Sign in with the emailed one-time code.
#[utoipa::path(post, path = "/api/auth/otp/verify", operation_id = "auth_otp_verify", tag = "auth", request_body = AuthOtpVerifyRequest,
    responses((status = 200, description = "Session, or MFA challenge", body = AuthLoginResponse)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_otp_verify() {}

/// Answer an MFA challenge with a TOTP or recovery code.
#[utoipa::path(post, path = "/api/auth/mfa/verify", operation_id = "auth_mfa_verify", tag = "mfa", request_body = AuthMfaVerifyRequest,
    responses((status = 200, description = "Session", body = AuthLoginResponse)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_mfa_verify() {}

/// Start TOTP enrolment: returns the secret and `otpauth://` URI.
#[utoipa::path(post, path = "/api/auth/mfa/totp/setup", operation_id = "auth_mfa_totp_setup", tag = "mfa",
    responses((status = 200, description = "Pending enrolment", body = AuthTotpSetup)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_mfa_totp_setup() {}

/// Confirm enrolment with a code; returns the recovery codes (shown once).
#[utoipa::path(post, path = "/api/auth/mfa/totp/confirm", operation_id = "auth_mfa_totp_confirm", tag = "mfa", request_body = AuthCodeRequest,
    responses((status = 200, description = "Enabled; recovery codes", body = AuthTotpEnabled)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_mfa_totp_confirm() {}

/// Disable TOTP (needs a current code).
#[utoipa::path(post, path = "/api/auth/mfa/totp/disable", operation_id = "auth_mfa_totp_disable", tag = "mfa", request_body = AuthCodeRequest,
    responses((status = 200, description = "`{enabled:false}`", body = AuthTotpDisabled)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_mfa_totp_disable() {}

/// Regenerate recovery codes (needs a current code); old codes stop working.
#[utoipa::path(post, path = "/api/auth/mfa/recovery-codes", operation_id = "auth_mfa_recovery_codes", tag = "mfa", request_body = AuthCodeRequest,
    responses((status = 200, description = "New recovery codes", body = AuthRecoveryCodes)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_mfa_recovery_codes() {}

/// Change the caller's password.
#[utoipa::path(post, path = "/api/auth/password/change", operation_id = "auth_password_change", tag = "auth", request_body = AuthChangePasswordRequest,
    responses((status = 204, description = "Password changed")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_password_change() {}

/// The caller's active sessions (device list).
#[utoipa::path(get, path = "/api/auth/sessions", operation_id = "auth_sessions_list", tag = "sessions",
    responses((status = 200, description = "Sessions", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_sessions_list() {}

/// Revoke one of the caller's sessions.
#[utoipa::path(delete, path = "/api/auth/sessions/{id}", operation_id = "auth_sessions_revoke", tag = "sessions", params(("id" = String, Path, description = "Session id")),
    responses((status = 204, description = "Revoked")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_sessions_revoke() {}

/// The caller's recent sign-ins.
#[utoipa::path(get, path = "/api/auth/login-history", operation_id = "auth_login_history_list", tag = "sessions",
    responses((status = 200, description = "Login history", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_login_history_list() {}

/// The caller's personal access tokens.
#[utoipa::path(get, path = "/api/auth/tokens", operation_id = "auth_tokens_list", tag = "keys",
    responses((status = 200, description = "Tokens", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_tokens_list() {}

/// Create a personal access token; the secret is shown once.
#[utoipa::path(post, path = "/api/auth/tokens", operation_id = "auth_tokens_create", tag = "keys", request_body = AuthKeyRequest,
    responses((status = 201, description = "Created token (with secret)", body = AuthKeyCreated)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_tokens_create() {}

/// Revoke a personal access token.
#[utoipa::path(delete, path = "/api/auth/tokens/{id}", operation_id = "auth_tokens_revoke", tag = "keys", params(("id" = String, Path, description = "Token id")),
    responses((status = 204, description = "Revoked")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_tokens_revoke() {}

/// Administrator password sign-in.
#[utoipa::path(post, path = "/api/auth/admins/login", operation_id = "auth_admins_login", tag = "admin", request_body = AuthLoginRequest,
    responses((status = 200, description = "Session, or MFA challenge", body = AuthLoginResponse)))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_admins_login() {}

/// Administrators (admin only).
#[utoipa::path(get, path = "/api/auth/admins", operation_id = "auth_admins_list", tag = "admin",
    responses((status = 200, description = "Administrators", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_admins_list() {}

/// Create an administrator (admin only).
#[utoipa::path(post, path = "/api/auth/admins", operation_id = "auth_admins_create", tag = "admin", request_body = AuthAdminCreateRequest,
    responses((status = 201, description = "Created administrator", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_admins_create() {}

/// Service accounts (admin only).
#[utoipa::path(get, path = "/api/auth/service-accounts", operation_id = "auth_service_accounts_list", tag = "keys",
    responses((status = 200, description = "Service accounts", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_accounts_list() {}

/// Create a service account (admin only).
#[utoipa::path(post, path = "/api/auth/service-accounts", operation_id = "auth_service_accounts_create", tag = "keys", request_body = AuthServiceAccountRequest,
    responses((status = 201, description = "Created service account", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_accounts_create() {}

/// One service account (admin only).
#[utoipa::path(get, path = "/api/auth/service-accounts/{id}", operation_id = "auth_service_accounts_get", tag = "keys", params(("id" = String, Path, description = "Service account id")),
    responses((status = 200, description = "Service account", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_accounts_get() {}

/// Delete a service account and its keys (admin only).
#[utoipa::path(delete, path = "/api/auth/service-accounts/{id}", operation_id = "auth_service_accounts_delete", tag = "keys", params(("id" = String, Path, description = "Service account id")),
    responses((status = 204, description = "Deleted")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_accounts_delete() {}

/// API keys of a service account (admin only).
#[utoipa::path(get, path = "/api/auth/service-accounts/{id}/keys", operation_id = "auth_service_account_keys_list", tag = "keys", params(("id" = String, Path, description = "Service account id")),
    responses((status = 200, description = "Keys", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_account_keys_list() {}

/// Create an API key for a service account; the secret is shown once (admin only).
#[utoipa::path(post, path = "/api/auth/service-accounts/{id}/keys", operation_id = "auth_service_account_keys_create", tag = "keys", params(("id" = String, Path, description = "Service account id")), request_body = AuthKeyRequest,
    responses((status = 201, description = "Created key (with secret)", body = AuthKeyCreated)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_service_account_keys_create() {}

/// Revoke an API key (admin only).
#[utoipa::path(delete, path = "/api/auth/api-keys/{id}", operation_id = "auth_api_keys_revoke", tag = "keys", params(("id" = String, Path, description = "Key id")),
    responses((status = 204, description = "Revoked")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_api_keys_revoke() {}

/// All API keys (admin only).
#[utoipa::path(get, path = "/api/auth/api-keys", operation_id = "auth_api_keys_list", tag = "keys",
    responses((status = 200, description = "Keys", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_api_keys_list() {}

/// One user record (admin only).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}", operation_id = "auth_users_get", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "User", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_users_get() {}

/// A user's sessions (admin only).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/sessions", operation_id = "auth_user_sessions_list", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Sessions", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_user_sessions_list() {}

/// Revoke all of a user's sessions (admin only).
#[utoipa::path(post, path = "/api/auth/users/{collection}/{id}/sessions/revoke", operation_id = "auth_user_sessions_revoke", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "`{revoked: n}`", body = AuthRevoked)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_user_sessions_revoke() {}

/// A user's sign-in history; query `limit` (admin only).
#[utoipa::path(get, path = "/api/auth/users/{collection}/{id}/login-history", operation_id = "auth_user_login_history_list", tag = "admin", params(("collection" = String, Path, description = "Auth collection"), ("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Login history", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_user_login_history_list() {}

/// Start an audited impersonation session (admin only).
#[utoipa::path(post, path = "/api/auth/impersonate", operation_id = "auth_impersonate", tag = "admin", request_body = AuthImpersonateRequest,
    responses((status = 200, description = "Impersonation grant (tokens)", body = Object)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_impersonate() {}

/// Impersonation sessions (admin only).
#[utoipa::path(get, path = "/api/auth/impersonations", operation_id = "auth_impersonations_list", tag = "admin",
    responses((status = 200, description = "Impersonations", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_impersonations_list() {}

/// End an impersonation session (admin only).
#[utoipa::path(post, path = "/api/auth/impersonations/{id}/end", operation_id = "auth_impersonations_end", tag = "admin", params(("id" = String, Path, description = "Impersonation id")),
    responses((status = 204, description = "Ended")),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_impersonations_end() {}

/// Security audit log; query `event`, `subject_id`, `before`, `limit` (admin only).
#[utoipa::path(get, path = "/api/auth/audit", operation_id = "auth_audit_list", tag = "admin",
    responses((status = 200, description = "Audit events", body = AuthItems)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_audit_list() {}

/// Rotate the access-token signing key (admin only).
#[utoipa::path(post, path = "/api/auth/keys/rotate", operation_id = "auth_keys_rotate", tag = "admin",
    responses((status = 200, description = "New active key id", body = AuthKeyRotated)),
    security(("bearer" = []), ("api_key" = []), ("session_cookie" = [])))]
#[allow(dead_code, clippy::missing_const_for_fn)]
pub(super) fn auth_keys_rotate() {}

/// `POST /api/auth/signup` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthSignupRequest {
    /// Auth collection (default: the configured default collection).
    pub collection: Option<String>,
    pub email: String,
    pub password: String,
    /// Extra user fields.
    pub data: Option<serde_json::Map<String, serde_json::Value>>,
}

/// `POST /api/auth/login` and `/api/auth/admins/login` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthLoginRequest {
    pub collection: Option<String>,
    pub email: String,
    pub password: String,
    /// Use an HttpOnly session cookie instead of returning tokens.
    pub cookie: Option<bool>,
    /// Name for the device list.
    pub device_name: Option<String>,
}

/// Body of the request-an-email endpoints.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthEmailRequest {
    pub collection: Option<String>,
    pub email: String,
}

/// Body of the confirm-with-token endpoints.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthTokenRequest {
    pub token: String,
    pub cookie: Option<bool>,
    pub device_name: Option<String>,
}

/// `POST /api/auth/password-reset/confirm` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthResetConfirmRequest {
    pub token: String,
    pub password: String,
}

/// `POST /api/auth/otp/verify` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthOtpVerifyRequest {
    pub collection: Option<String>,
    pub email: String,
    pub code: String,
    pub cookie: Option<bool>,
    pub device_name: Option<String>,
}

/// `POST /api/auth/mfa/verify` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthMfaVerifyRequest {
    /// From the `mfa_required` login answer.
    pub mfa_token: String,
    /// TOTP or recovery code.
    pub code: String,
}

/// Body carrying one TOTP or recovery code.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthCodeRequest {
    pub code: String,
}

/// `POST /api/auth/refresh` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthRefreshRequest {
    pub refresh_token: String,
}

/// `POST /api/auth/password/change` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// Body of the token and API key creation endpoints.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthKeyRequest {
    pub name: String,
    /// `<area>:read|write[:<name>]`.
    pub scopes: Vec<String>,
    pub expires_in_days: Option<u32>,
}

/// `POST /api/auth/service-accounts` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthServiceAccountRequest {
    pub name: String,
    pub description: Option<String>,
    /// Defaults to `service`.
    pub role: Option<String>,
}

/// `POST /api/auth/admins` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthAdminCreateRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/impersonate` body.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthImpersonateRequest {
    pub principal: String,
    pub reason: String,
    pub ttl_minutes: Option<u32>,
}

/// Tokens of a new or refreshed session. In cookie mode the tokens are absent and
/// `Set-Cookie` headers carry the session and CSRF cookies.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthSession {
    /// Always `Bearer`.
    pub token_type: String,
    pub access_token: Option<String>,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
    pub session_id: String,
    pub csrf_token: Option<String>,
    /// RFC 3339.
    pub session_expires_at: String,
    /// The signed-in subject.
    pub subject: serde_json::Value,
    /// The user record (user sign-ins only).
    pub user: Option<serde_json::Value>,
}

/// A second factor is required.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthMfaChallenge {
    pub mfa_required: bool,
    pub mfa_token: String,
    pub expires_in: u64,
}

/// A session, or an MFA challenge.
#[derive(ToSchema)]
#[allow(dead_code)]
#[serde(untagged)]
pub enum AuthLoginResponse {
    Session(AuthSession),
    Mfa(AuthMfaChallenge),
}

/// `{user}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthUserEnvelope {
    pub user: serde_json::Value,
}

/// `{status:"accepted", message}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthAccepted {
    pub status: String,
    pub message: String,
}

/// `{items: [...]}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthItems {
    pub items: Vec<serde_json::Value>,
}

/// A pending TOTP enrolment.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthTotpSetup {
    /// Base32 secret.
    pub secret: String,
    /// `otpauth://` URI for QR codes.
    pub otpauth_url: String,
}

/// TOTP enabled.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthTotpEnabled {
    pub enabled: bool,
    pub recovery_codes: Vec<String>,
}

/// TOTP disabled.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthTotpDisabled {
    pub enabled: bool,
}

/// New recovery codes.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthRecoveryCodes {
    pub recovery_codes: Vec<String>,
}

/// A created token or API key; `key` is the secret, shown once, plus the key's metadata.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthKeyCreated {
    pub id: String,
    pub name: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub key: String,
}

/// `{revoked: n}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthRevoked {
    pub revoked: u64,
}

/// `{active_kid}`.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct AuthKeyRotated {
    pub active_kid: String,
}
