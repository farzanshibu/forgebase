//! Wire models generated from the server's OpenAPI document by `scripts/generate_models.py`.
//!
//! Do not edit by hand: change the script or the OpenAPI source and regenerate.

#![allow(
    missing_docs,
    clippy::large_enum_variant,
    clippy::enum_variant_names,
    clippy::struct_field_names
)]

use serde::{Deserialize, Serialize};

/// Versions to accept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcceptInput {
    /// Agreements and the version accepted.
    #[serde(default)]
    pub items: Vec<AcceptItem>,
}

/// One agreement version accepted.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AcceptItem {
    /// Kind.
    pub kind: String,
    /// Version (must be the current one).
    pub version: i64,
}

/// Acceptance record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Acceptance {
    /// When (epoch ms).
    pub accepted_at: i64,
    /// Id.
    pub id: String,
    /// From which address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Kind.
    pub kind: String,
    /// Who (principal key).
    pub principal: String,
    /// Realm.
    pub realm: String,
    /// With which client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Version.
    pub version: i64,
}

/// The ACL of a bucket (replaced as a whole).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Acl {
    /// Grants.
    #[serde(default)]
    pub entries: Vec<AclEntry>,
}

/// One ACL grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AclEntry {
    /// `user:<collection>/<id>`, `service:<id>`, `role:<name>`,
    /// `authenticated` or `public`.
    pub grantee: String,
    /// Access level.
    pub permission: Permission,
    /// Key prefix the grant covers (`""`: the whole bucket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// An add-on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Addon {
    /// Can be assigned.
    pub active: bool,
    /// Description.
    pub description: String,
    /// Grants.
    #[serde(default)]
    pub features: Vec<AddonFeature>,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
}

/// What one unit of an add-on grants.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonFeature {
    /// Feature key.
    pub feature: String,
    /// Units added per add-on quantity (`null`: unlimited when the add-on is
    /// what enables the feature, no change otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `PUT /addons/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonInput {
    /// Can be assigned (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Grants (replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<AddonFeature>>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Add-on quantity of a subject.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddonQuantity {
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AggregateBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregates: Option<Vec<AggregateSpecBody>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AggregateSpecBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// `count`, `sum`, `avg`, `min`, `max`.
    pub function: String,
}

/// One version of a legal agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Agreement {
    /// Text (plain text or Markdown; escape when rendering).
    pub body: String,
    /// Id.
    pub id: String,
    /// Kind (`terms`, `privacy`, ...).
    pub kind: String,
    /// Published (epoch ms).
    pub published_at: i64,
    /// Who published it.
    pub published_by: String,
    /// Realm.
    pub realm: String,
    /// Title.
    pub title: String,
    /// Canonical URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Version (1, 2, ...).
    pub version: i64,
}

/// Publishes the next version of an agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgreementInput {
    /// Text (max 200 KiB).
    pub body: String,
    /// Kind: lower-case letters, digits, `_`, `-` (max 64).
    pub kind: String,
    /// Title (max 200).
    pub title: String,
    /// Canonical URL (`http(s)://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A version of an agreement without its text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AgreementRef {
    /// Kind.
    pub kind: String,
    /// Title.
    pub title: String,
    /// Version.
    pub version: i64,
}

/// Body of a refund (of a consumption or spend) or a reversal (of a top-up
/// or grant).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AmendInput {
    /// Credits (default: all that is left to refund or reverse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    /// The entry refunded or reversed.
    pub entry_id: String,
    /// External reference (FB-70: refund or credit note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnalyzeBody {
    /// Collection to analyze; every collection when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

/// Consistency of one account: the balance, the ledger and the lots agree.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuditReport {
    /// Balance on the account.
    pub balance: i64,
    /// All of the above agree.
    pub consistent: bool,
    /// Every entry's amount equals the sum of its allocations.
    pub entries_match_allocations: bool,
    /// `balance_after` of the newest entry.
    pub last_balance_after: i64,
    /// Sum of the ledger.
    pub ledger_sum: i64,
    /// Every lot's remaining equals the sum of its allocations.
    pub lots_match_allocations: bool,
    /// Sum of what is left in the lots.
    pub lots_remaining: i64,
    /// Subject.
    pub subject: String,
}

/// `{status:"accepted", message}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthAccepted {
    pub message: String,
    pub status: String,
}

/// `POST /api/auth/admins` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthAdminCreateRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/password/change` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// Body carrying one TOTP or recovery code.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthCodeRequest {
    pub code: String,
}

/// Body of the request-an-email endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthEmailRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    pub email: String,
}

/// `POST /api/auth/impersonate` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthImpersonateRequest {
    pub principal: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_minutes: Option<i64>,
}

/// `{items: [...]}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthItems {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
}

/// A created token or API key; `key` is the secret, shown once, plus the key's metadata.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyCreated {
    pub id: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
}

/// Body of the token and API key creation endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<i64>,
    pub name: String,
    /// `<area>:read|write[:<name>]`.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// `{active_kid}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthKeyRotated {
    pub active_kid: String,
}

/// `POST /api/auth/login` and `/api/auth/admins/login` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthLoginRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Use an HttpOnly session cookie instead of returning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    /// Name for the device list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub email: String,
    pub password: String,
}

/// A session, or an MFA challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuthLoginResponse {
    /// `AuthSession`
    AuthSession(AuthSession),
    /// `AuthMfaChallenge`
    AuthMfaChallenge(AuthMfaChallenge),
}

/// A second factor is required.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthMfaChallenge {
    pub expires_in: i64,
    pub mfa_required: bool,
    pub mfa_token: String,
}

/// `POST /api/auth/mfa/verify` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthMfaVerifyRequest {
    /// TOTP or recovery code.
    pub code: String,
    /// From the `mfa_required` login answer.
    pub mfa_token: String,
}

/// `POST /api/auth/otp/verify` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthOtpVerifyRequest {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub email: String,
}

/// New recovery codes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRecoveryCodes {
    #[serde(default)]
    pub recovery_codes: Vec<String>,
}

/// `POST /api/auth/refresh` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRefreshRequest {
    pub refresh_token: String,
}

/// `POST /api/auth/password-reset/confirm` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthResetConfirmRequest {
    pub password: String,
    pub token: String,
}

/// `{revoked: n}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthRevoked {
    pub revoked: i64,
}

/// `POST /api/auth/service-accounts` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthServiceAccountRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: String,
    /// Defaults to `service`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Tokens of a new or refreshed session. In cookie mode the tokens are absent and
/// `Set-Cookie` headers carry the session and CSRF cookies.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
    pub expires_in: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// RFC 3339.
    pub session_expires_at: String,
    pub session_id: String,
    /// The signed-in subject.
    #[serde(default)]
    pub subject: serde_json::Value,
    /// Always `Bearer`.
    pub token_type: String,
    /// The user record (user sign-ins only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<serde_json::Value>,
}

/// `POST /api/auth/signup` body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthSignupRequest {
    /// Auth collection (default: the configured default collection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Extra user fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    pub email: String,
    pub password: String,
}

/// Body of the confirm-with-token endpoints.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTokenRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub token: String,
}

/// TOTP disabled.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpDisabled {
    pub enabled: bool,
}

/// TOTP enabled.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpEnabled {
    pub enabled: bool,
    #[serde(default)]
    pub recovery_codes: Vec<String>,
}

/// A pending TOTP enrolment.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthTotpSetup {
    /// `otpauth://` URI for QR codes.
    pub otpauth_url: String,
    /// Base32 secret.
    pub secret: String,
}

/// `{user}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AuthUserEnvelope {
    #[serde(default)]
    pub user: serde_json::Value,
}

/// A subject's balance.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Balance {
    /// Credits available.
    pub balance: i64,
    /// Next expiries (soonest first, at most 10).
    #[serde(default)]
    pub expiring: Vec<Expiring>,
    /// Subject.
    pub subject: String,
    /// Last change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Balance over time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceHistory {
    /// Start (inclusive).
    pub from: String,
    /// Bucket width.
    pub interval: Interval,
    /// Balance at `from`.
    pub opening: i64,
    /// Every bucket, oldest first.
    #[serde(default)]
    pub points: Vec<BalancePoint>,
    /// Subject.
    pub subject: String,
    /// End (exclusive).
    pub to: String,
}

/// One bucket of a balance history.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BalancePoint {
    /// Balance at the end of the bucket.
    pub closing: i64,
    /// Credits added in the bucket.
    pub credited: i64,
    /// Credits removed in the bucket.
    pub debited: i64,
    /// Bucket start.
    pub start: String,
}

/// Body of `POST /batch`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BatchBody {
    /// Commands run in order with no other command in between.
    #[serde(default)]
    pub commands: Vec<Vec<Bin>>,
}

/// A binary-safe value: a string, or `{"base64": "..."}` for bytes that are
/// not UTF-8.
pub type Bin = serde_json::Value;

/// White-label branding of a realm.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Branding {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Page background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name shown to users.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// A favicon is uploaded.
    pub has_favicon: bool,
    /// A logo is uploaded.
    pub has_logo: bool,
    /// Primary colour (`#rgb` or `#rrggbb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Realm.
    pub realm: String,
    /// Sign-in page footer (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in page sub-heading (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in page heading (plain text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support contact shown on hosted pages and in email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    /// Last change (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// Replaces the branding of a realm; a field left out is cleared.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BrandingInput {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Page background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name (max 80).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Primary colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Sign-in footer (max 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in sub-heading (max 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in heading (max 120).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

/// A bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bucket {
    /// Allowed content types (`image/*` patterns allowed); `null` allows all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Largest object, in bytes (`null`: the server limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Name.
    pub name: String,
    /// Principal key of the creator.
    pub owner: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
    /// Whether overwrites and deletes keep earlier versions.
    pub versioning: bool,
    /// Default read access.
    pub visibility: Visibility,
}

/// A bucket to create.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BucketInput {
    /// Allowed content types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// Largest object in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Name.
    pub name: String,
    /// Keep versions (default off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<bool>,
    /// Default read access (default private).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// A partial bucket update.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BucketPatch {
    /// New content-type allow list (empty clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_content_types: Option<Vec<String>>,
    /// New size limit (`0` clears it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_bytes: Option<i64>,
    /// Turn versioning on or off (earlier versions are kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versioning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

/// A claim request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClaimRequest {
    /// Only these handlers (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// At most this many jobs (default 1, at most 50).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Only these queues (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<Vec<String>>,
    /// The claiming worker (any id of `[A-Za-z0-9._:-]`, at most 128 characters).
    pub worker_id: String,
}

/// Jobs claimed by a worker.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClaimResponse {
    /// Up to the requested number; empty when nothing is due.
    #[serde(default)]
    pub jobs: Vec<ClaimedJob>,
}

/// A claimed job with the lease that fences it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimedJob {
    /// The job (already `running`).
    pub job: Job,
    /// Secret token: `heartbeat`, `complete` and `fail` must present it.
    pub lease_id: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionList {
    #[serde(default)]
    pub items: Vec<CollectionSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionSchema {
    pub created: String,
    #[serde(default)]
    pub fields: Vec<FieldSchema>,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub options: serde_json::Value,
    /// `strict` or `flexible` (JSON documents with undeclared keys).
    pub schema_mode: String,
    pub system: bool,
    /// `base`, `auth` or `view`.
    pub r#type: String,
    pub updated: String,
    pub version: i64,
    /// Keep every version of every record.
    pub versioned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionSchemaResponse {
    pub collection: CollectionSchema,
    /// JSON Schema (draft 2020-12) of a record of this collection.
    #[serde(default)]
    pub json_schema: serde_json::Value,
}

/// Statistics of one collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CollectionStats {
    /// Committed changes since creation.
    pub change_seq: i64,
    /// Collection.
    pub collection: String,
    /// One entry per vector field.
    #[serde(default)]
    pub indexes: Vec<IndexStatus>,
    /// Items.
    pub items: i64,
    /// Items with keyword-search text.
    pub items_with_text: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ColumnInfoSchema {
    pub declared_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub name: String,
    pub not_null: bool,
    pub primary_key: bool,
}

/// Body of a reservation commit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CommitBody {
    /// Units used (default: all reserved units; the rest is released).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

/// Body of a completion.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompleteBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// Stored as the job's result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// Completes a multipart upload.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompleteInput {
    /// SHA-256 of the whole object (overrides the declared one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Parts to assemble, in order, each optionally with its checksum.
    /// Default: every received part in part-number order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<CompletePart>>,
}

/// A part named in a completion.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompletePart {
    /// Expected checksum of the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Part number.
    pub part_number: i64,
}

/// Result of a consumption.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Consumption {
    /// The decision (of the first request when replayed).
    pub decision: Decision,
    /// Usage event id.
    pub event_id: String,
    /// `true` when this is a replay of an earlier request with the same key.
    pub replayed: bool,
}

/// Credits consumed for one feature in one bucket.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConsumptionRow {
    /// Credits debited (consumption and spend).
    pub debited: i64,
    /// Debit entries.
    pub entries: i64,
    /// Feature (`""`: spends without a feature).
    pub feature: String,
    /// `debited - refunded`.
    pub net: i64,
    /// Credits refunded.
    pub refunded: i64,
    /// Bucket start.
    pub start: String,
    /// Units paid for.
    pub units: i64,
}

/// Credit consumption by feature and period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsumptionStats {
    /// Start (inclusive).
    pub from: String,
    /// Bucket width.
    pub interval: Interval,
    /// Buckets with consumption, oldest first, then by feature.
    #[serde(default)]
    pub rows: Vec<ConsumptionRow>,
    /// Subject (`null`: the whole instance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// End (exclusive).
    pub to: String,
    /// Sum of `net`.
    pub total_net: i64,
}

/// Copy or move target.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CopyBody {
    /// Destination bucket (default: the same).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_bucket: Option<String>,
    /// Destination key.
    pub dest_key: String,
    /// Source key.
    pub key: String,
    /// Source version (copy only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CountResponse {
    pub count: i64,
}

/// Body of a top-up or grant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditInput {
    /// Credits (at least 1).
    pub amount: i64,
    /// When the credits lapse (default: never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Order, payment or invoice reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Idempotency key (unique per subject; FB-70: the payment or event id).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
}

/// Totals by entry kind over a period.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CreditSummary {
    /// Balance now (the subject's, or of every account).
    pub balance: i64,
    /// Credits charged for usage.
    pub consumption: i64,
    /// Credits lapsed.
    pub expiry: i64,
    /// Start (inclusive).
    pub from: String,
    /// Credits granted.
    pub grant: i64,
    /// Net change in the period.
    pub net: i64,
    /// Credits refunded.
    pub refund: i64,
    /// Credits reversed.
    pub reversal: i64,
    /// Usage cost not covered by balances in the period.
    pub shortfall: i64,
    /// Credits spent directly.
    pub spend: i64,
    /// Subject (`null`: the whole instance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// End (exclusive).
    pub to: String,
    /// Credits topped up.
    pub top_up: i64,
}

/// The consumer cursor.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cursor {
    /// Last usage event `seq` read.
    pub seq: i64,
}

/// An enterprise contract limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CustomLimit {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why (contract reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    pub subject: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A quota decision with the counter state after it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Decision {
    /// Allowed.
    pub allowed: bool,
    /// Feature.
    pub feature: String,
    /// Allowance of the window: limit plus rolled-over units (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Units of this request past the limit of a soft quota.
    pub overage_units: i64,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// Units of this request taken from usage packs.
    pub pack_units: i64,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Left in the window plus usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by open reservations.
    pub reserved: i64,
    /// Counter scope key (`""`: the subject's counter).
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// Units asked for.
    pub units: i64,
    /// Committed usage in the window (gauges: the level; concurrency: active leases).
    pub used: i64,
    /// Window end (`null`: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

/// Body of a batch delete.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeleteRequest {
    /// 1 to 1000 ids; unknown ids are ignored.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// Outcome of a delete.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeleteResult {
    /// Items removed.
    pub deleted: i64,
}

/// One row of the delivery log (never the body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// Attempts made.
    pub attempts: i64,
    /// Queued (epoch ms).
    pub created_at: i64,
    /// Id.
    pub id: String,
    /// Queue job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Provider that handled it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Realm.
    pub realm: String,
    /// Handed over (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    /// State.
    pub status: DeliveryStatus,
    /// Subject as sent.
    pub subject: String,
    /// Template key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Recipient.
    pub to: String,
    /// Last change (epoch ms).
    pub updated_at: i64,
}

/// State of a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryStatus {
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "retrying")]
    Retrying,
    #[serde(rename = "sent")]
    Sent,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST /api/v1/functions`: deploys a new version (creating the
/// function on its first deploy) and, unless `activate` is `false`, makes it
/// the active version.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DeployRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<InvokeAccess>,
    /// Make the new version active (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate: Option<bool>,
    /// Granted capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    /// Description (kept from the previous deploy when omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Plain environment values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// Allowed fetch hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    /// Limits (defaults for missing fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsInput>,
    /// Function name.
    pub name: String,
    /// The WebAssembly module (binary format), base64.
    pub wasm: String,
}

/// A deploy result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deployed {
    /// The function after the deploy.
    pub function: FunctionInfo,
    /// The new version.
    pub version: VersionInfo,
}

/// Which way edges are followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    #[serde(rename = "out")]
    Out,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "both")]
    Both,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A custom domain of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    /// Created (epoch ms).
    pub created_at: i64,
    /// Who added it.
    pub created_by: String,
    /// Host name (lower case).
    pub host: String,
    /// Id.
    pub id: String,
    /// Last verification attempt (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    /// Why the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Realm it routes to.
    pub realm: String,
    /// State.
    pub status: DomainStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Verified (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
}

/// Adds a domain.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DomainInput {
    /// Host name, e.g. `login.example.com` (no scheme, port or path).
    pub host: String,
    /// Realm (platform administrators; default: the caller's realm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
}

/// State of a custom domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "verified")]
    Verified,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A typed edge between two nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Edge {
    /// RFC 3339 creation time.
    pub created_at: String,
    /// `false` for an undirected relationship (walked both ways).
    pub directed: bool,
    /// Edge id (unique per tenant).
    pub id: String,
    /// JSON object of properties.
    #[serde(default)]
    pub properties: serde_json::Value,
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    /// Relationship type, e.g. `FOLLOWS`.
    pub r#type: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Body of `PATCH /edges/{id}`: only properties can change.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EdgeChange {
    /// JSON merge patch (RFC 7396; `null` removes a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// A page of edges.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EdgePage {
    #[serde(default)]
    pub items: Vec<Edge>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// An email template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EmailTemplate {
    /// The realm overrides the built-in template.
    pub customized: bool,
    /// HTML body template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Key (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own).
    pub key: String,
    /// Realm.
    pub realm: String,
    /// Subject template.
    pub subject: String,
    /// Plain-text body template.
    pub text: String,
    /// Last change (epoch ms; built-ins: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Variables the template uses.
    #[serde(default)]
    pub variables: Vec<String>,
}

/// Hard or soft limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Enforcement {
    #[serde(rename = "hard")]
    Hard,
    #[serde(rename = "soft")]
    Soft,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Effective entitlements of a subject (and member).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Entitlements {
    /// Whether the subscription entitles (else the default plan applies).
    pub entitled: bool,
    /// Grants by feature key.
    #[serde(default)]
    pub features: std::collections::HashMap<String, Grant>,
    /// Member the grants were resolved for (member overrides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Plan in force (`null`: none, not even a default plan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubscriptionStatus>,
    /// Subject.
    pub subject: String,
    /// Trial end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
}

/// What a ledger entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    #[serde(rename = "top_up")]
    TopUp,
    #[serde(rename = "grant")]
    Grant,
    #[serde(rename = "consumption")]
    Consumption,
    #[serde(rename = "spend")]
    Spend,
    #[serde(rename = "refund")]
    Refund,
    #[serde(rename = "reversal")]
    Reversal,
    #[serde(rename = "expiry")]
    Expiry,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Error body (the standard envelope).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `{code, message, request_id}`.
    pub error: ErrorDetail,
}

/// The `error` object of the envelope.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Stable machine-readable code.
    pub code: String,
    /// Structured context, e.g. per-field validation errors
    /// (`{"fields": {"name": {"code": "required", "message": "..."}}}`).
    /// Additive and optional (ADR-0001 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Human-readable message.
    pub message: String,
    /// Id of the request (also in the `x-request-id` response header).
    pub request_id: String,
}

/// The whole error body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// The error.
    pub error: ErrorDetail,
}

/// History of a job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventList {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<JobEvent>,
}

/// Body of `POST /exec`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecBody {
    /// Command name followed by arguments, e.g. `["INCRBY", "hits", "5"]`.
    #[serde(default)]
    pub command: Vec<Bin>,
}

/// Execution logs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecutionList {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<ExecutionLog>,
}

/// One recorded execution.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExecutionLog {
    /// When it started (RFC 3339).
    pub created_at: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Error message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Fuel consumed.
    pub fuel_used: i64,
    /// Function name.
    pub function: String,
    /// Id (increasing).
    pub id: i64,
    /// Lines the function logged (capped).
    #[serde(default)]
    pub logs: Vec<LogLine>,
    /// Request id of the triggering request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `ok`, `rejected`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout` or `bad_output`.
    pub status: String,
    /// `http`, `hook`, `job` or `test`.
    pub trigger: String,
    /// Version that ran.
    pub version: i64,
}

/// Credits lapsing at one time.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Expiring {
    /// Credits.
    pub amount: i64,
    /// When.
    pub expires_at: String,
}

/// What an expiry run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ExpiryReport {
    /// Credits lapsed.
    pub credits: i64,
    /// Lots emptied.
    pub lots: i64,
}

/// Body of a failure report.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FailBody {
    /// What went wrong.
    pub error: String,
    /// The lease from the claim.
    pub lease_id: String,
    /// `false` sends the job straight to the dead-letter queue (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

/// A feature of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Key (`ai_generation`, `storage`, `api_access`).
    pub key: String,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name.
    pub name: String,
    /// Permission patterns this feature gates (pipeline step 4). A key
    /// `org:<feature>:<permission>` is gated by feature `<feature>` without
    /// being listed.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Unit shown with limits (`requests`, `bytes`).
    pub unit: String,
    /// Last change.
    pub updated_at: String,
}

/// Body of `PUT /features/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureInput {
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name (default: the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Gated permission patterns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// What a feature measures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureKind {
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "metered")]
    Metered,
    #[serde(rename = "gauge")]
    Gauge,
    #[serde(rename = "concurrency")]
    Concurrency,
    #[serde(rename = "rate")]
    Rate,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Vector fields of a collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FieldList {
    /// By name.
    #[serde(default)]
    pub items: Vec<VectorField>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FieldSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    pub hidden: bool,
    /// Stable id; renames keep it.
    pub id: String,
    pub name: String,
    /// Type options sit beside `type`: `min_length`, `max_length`, `min`, `max`,
    /// `integer`, `values`, `max_select`, `collection_id`, `cascade_delete`,
    /// `items`, `min_items`, `max_items`, `dimensions`, `max_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    pub required: bool,
    pub system: bool,
    /// `text`, `number`, `bool`, `date`, `email`, `url`, `select`, `json`,
    /// `array`, `relation`, `file`, `password`, `geo`, `vector`.
    pub r#type: String,
    pub unique: bool,
}

/// Definition of a vector field, as sent when creating it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSpec {
    /// HNSW links per node (default 16, 2 to 128).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<i64>,
    /// Number of components, 1 to 4096.
    pub dimension: i64,
    /// HNSW candidate list size while inserting (default 128).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_add: Option<i64>,
    /// HNSW candidate list size while searching (default 128). Higher means
    /// better recall and slower searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion_search: Option<i64>,
    /// Distance function.
    pub metric: Metric,
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<Quantization>,
}

/// When a metadata filter is applied relative to the vector search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMode {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "pre")]
    Pre,
    #[serde(rename = "post")]
    Post,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionInfo {
    /// HTTP access mode.
    pub access: InvokeAccess,
    /// The version that runs.
    pub active_version: i64,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Description.
    pub description: String,
    /// Id.
    pub id: String,
    /// The newest version.
    pub latest_version: i64,
    /// Name.
    pub name: String,
    /// Identity the function acts as (`service:function.<name>`): grant it
    /// permissions with the policy API.
    pub principal: String,
    /// Last deploy or rollback (RFC 3339).
    pub updated_at: String,
}

/// Functions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionList {
    /// By name.
    #[serde(default)]
    pub items: Vec<FunctionInfo>,
}

/// Body of a gauge change: `delta` (may be negative) or the new `value`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GaugeRequest {
    /// Change of the level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<i64>,
    /// Feature key (a gauge).
    pub feature: String,
    /// Required: a replay returns the first result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Stored with the usage event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Counter scope key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
}

/// A subject's effective grant of one feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    /// Included.
    pub enabled: bool,
    /// Kind of the feature.
    pub kind: FeatureKind,
    /// Limit per window (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Window.
    pub period: Period,
    /// Strongest source applied.
    pub source: GrantSource,
    /// Unit.
    pub unit: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Where a feature's grant comes from (strongest last applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantSource {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "addon")]
    Addon,
    #[serde(rename = "custom")]
    Custom,
    #[serde(rename = "override")]
    Override,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Counts for the admin explorer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GraphStats {
    /// Edge count per relationship type.
    #[serde(default)]
    pub edge_types: Vec<TypeCount>,
    /// Number of edges.
    pub edges: i64,
    /// Number of nodes.
    pub nodes: i64,
}

/// Answer of `has()`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HasAnswer {
    /// The answer.
    pub has: bool,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Body of `POST .../hashes/{key}/delete`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashDeleteBody {
    /// Field names to remove.
    #[serde(default)]
    pub fields: Vec<String>,
}

/// Body of `POST .../hashes/{key}/incr`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashIncrBody {
    /// Amount to add (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
    /// Field to change.
    pub field: String,
}

/// Body of `POST .../hashes/{key}/set`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HashSetBody {
    /// Field names and values.
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Bin>,
}

/// Answer of a heartbeat.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Heartbeat {
    /// The job was cancelled: stop and call `fail`.
    pub cancel_requested: bool,
    /// New lease expiry (RFC 3339).
    pub visibility_until: String,
}

/// A record event a hook function runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    #[serde(rename = "before_create")]
    BeforeCreate,
    #[serde(rename = "after_create")]
    AfterCreate,
    #[serde(rename = "before_update")]
    BeforeUpdate,
    #[serde(rename = "after_update")]
    AfterUpdate,
    #[serde(rename = "before_delete")]
    BeforeDelete,
    #[serde(rename = "after_delete")]
    AfterDelete,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Record events of one collection that run a function.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HookSpec {
    /// Collection name.
    pub collection: String,
    /// Events.
    #[serde(default)]
    pub events: Vec<HookEvent>,
}

/// One hybrid search result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridHit {
    /// Item id.
    pub id: String,
    /// Metadata, unless `include_metadata` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Fused RRF score (higher is better).
    pub score: f64,
    /// Text, when `include_text` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 1-based rank in the keyword ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_rank: Option<i64>,
    /// BM25 relevance in the keyword ranking (higher is better).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_score: Option<f64>,
    /// Vector, when `include_vectors` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
    /// Distance in the vector ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_distance: Option<f64>,
    /// 1-based rank in the vector ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<i64>,
}

/// A hybrid search: keyword (FTS5, BM25) and vector rankings fused with
/// Reciprocal Rank Fusion: `score = Σ weight / (rrf_k + rank)`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridRequest {
    /// Candidates taken from each ranking before fusion (default
    /// `max(5 k, 50)`, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<i64>,
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Metadata filter, applied to both rankings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// When the filter is applied on the vector side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Return each hit's metadata (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
    /// Return each hit's vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// RRF constant (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<f64>,
    /// Keyword query; every word is matched literally (optional:
    /// vector-only without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// How the words combine (default `or`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_operator: Option<TextOperator>,
    /// Query vector (optional: keyword-only without it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<HybridWeights>,
}

/// Hybrid search results.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridResponse {
    /// Best first.
    #[serde(default)]
    pub hits: Vec<HybridHit>,
    /// Candidates from the keyword ranking.
    pub text_candidates: i64,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
    /// Candidates from the vector ranking.
    pub vector_candidates: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_strategy: Option<Strategy>,
}

/// Weights of the two rankings in hybrid search.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HybridWeights {
    /// Weight of the keyword ranking (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<f64>,
    /// Weight of the vector ranking (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ImportResult {
    /// `json`, `ndjson` or `csv`.
    pub format: String,
    /// Records stored.
    pub imported: i64,
}

/// Body of `POST .../strings/{key}/incr`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IncrBody {
    /// Amount to add (default 1; negative to decrement).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexDefinitionSchema {
    pub collection: String,
    #[serde(default)]
    pub keys: Vec<IndexKeySchema>,
    /// `btree` (default) or `fulltext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexInfoSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<IndexDefinitionSchema>,
    pub kind: String,
    pub name: String,
    /// `auto`, `manual`, `sql` or `engine`.
    pub origin: String,
    pub partial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
    pub stats: IndexStatsSchema,
    pub table: String,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexKeySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<bool>,
    /// Field path in filter syntax: `price`, `metadata.brand`, `title:lower`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexList {
    #[serde(default)]
    pub items: Vec<IndexInfoSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexRecommendationSchema {
    pub definition: IndexDefinitionSchema,
    pub max_duration_ms: f64,
    pub occurrences: i64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexRecommendations {
    #[serde(default)]
    pub items: Vec<IndexRecommendationSchema>,
}

/// Life cycle of a derived index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexState {
    #[serde(rename = "unloaded")]
    Unloaded,
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "building")]
    Building,
    #[serde(rename = "failed")]
    Failed,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexStatsSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stat: Option<String>,
}

/// State and metrics of one field's ANN index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStatus {
    /// Searches answered by the ANN index since start.
    pub ann_searches: i64,
    /// Canonical vectors stored for the field.
    pub canonical_vectors: i64,
    /// Reserved slots of the serving index.
    pub capacity: i64,
    /// HNSW links per node.
    pub connectivity: i64,
    /// The index has changes not yet written to its file.
    pub dirty: bool,
    /// ANN engine.
    pub engine: String,
    /// ANN engine version.
    pub engine_version: String,
    /// Searches answered by brute force since start.
    pub exact_searches: i64,
    /// HNSW candidate list size while searching.
    pub expansion_search: i64,
    /// Vector field.
    pub field: String,
    /// Vectors in the serving index.
    pub indexed_vectors: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rebuild: Option<RebuildInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recall: Option<RecallReport>,
    /// When the file was last written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_saved: Option<String>,
    /// Mean search latency since start, in milliseconds.
    pub mean_search_ms: f64,
    /// Memory used by the serving index.
    pub memory_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Element type of the index.
    pub quantization: Quantization,
    /// Why the index is not ready, or why the running rebuild started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Searches since start.
    pub searches: i64,
    /// Size of the index file when saved.
    pub serialized_bytes: i64,
    /// `ann` when searches use an index, `exact` when they scan.
    pub serving: String,
    /// Life cycle state.
    pub state: IndexState,
}

/// Bucket width of statistics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interval {
    #[serde(rename = "hour")]
    Hour,
    #[serde(rename = "day")]
    Day,
    #[serde(rename = "month")]
    Month,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Who may invoke a function over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvokeAccess {
    #[serde(rename = "public")]
    Public,
    #[serde(rename = "authenticated")]
    Authenticated,
    #[serde(rename = "permission")]
    Permission,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A stored item.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Item {
    /// Creation time.
    pub created: String,
    /// Id.
    pub id: String,
    /// Metadata object.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Keyword-search text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Last replace.
    pub updated: String,
    /// Canonical vectors per field.
    #[serde(default)]
    pub vectors: std::collections::HashMap<String, Vec<f64>>,
    /// Incremented by every replace.
    pub version: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAcceptance {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAcceptanceItemsItem>,
}

/// Acceptance record.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAcceptanceItemsItem {
    /// When (epoch ms).
    pub accepted_at: i64,
    /// Id.
    pub id: String,
    /// From which address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Kind.
    pub kind: String,
    /// Who (principal key).
    pub principal: String,
    /// Realm.
    pub realm: String,
    /// With which client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Version.
    pub version: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAddon {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAddonItemsItem>,
}

/// An add-on.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAddonItemsItem {
    /// Can be assigned.
    pub active: bool,
    /// Description.
    pub description: String,
    /// Grants.
    #[serde(default)]
    pub features: Vec<AddonFeature>,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAgreement {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsAgreementItemsItem>,
}

/// One version of a legal agreement.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsAgreementItemsItem {
    /// Text (plain text or Markdown; escape when rendering).
    pub body: String,
    /// Id.
    pub id: String,
    /// Kind (`terms`, `privacy`, ...).
    pub kind: String,
    /// Published (epoch ms).
    pub published_at: i64,
    /// Who published it.
    pub published_by: String,
    /// Realm.
    pub realm: String,
    /// Title.
    pub title: String,
    /// Canonical URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Version (1, 2, ...).
    pub version: i64,
}

/// Buckets.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsBuckets {
    /// Entries.
    #[serde(default)]
    pub items: Vec<Bucket>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomLimit {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsCustomLimitItemsItem>,
}

/// An enterprise contract limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsCustomLimitItemsItem {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why (contract reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    pub subject: String,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsDelivery {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsDeliveryItemsItem>,
}

/// One row of the delivery log (never the body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsDeliveryItemsItem {
    /// Attempts made.
    pub attempts: i64,
    /// Queued (epoch ms).
    pub created_at: i64,
    /// Id.
    pub id: String,
    /// Queue job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Provider that handled it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Realm.
    pub realm: String,
    /// Handed over (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    /// State.
    pub status: DeliveryStatus,
    /// Subject as sent.
    pub subject: String,
    /// Template key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Recipient.
    pub to: String,
    /// Last change (epoch ms).
    pub updated_at: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsDomain {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsDomainItemsItem>,
}

/// A custom domain of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsDomainItemsItem {
    /// Created (epoch ms).
    pub created_at: i64,
    /// Who added it.
    pub created_by: String,
    /// Host name (lower case).
    pub host: String,
    /// Id.
    pub id: String,
    /// Last verification attempt (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    /// Why the last attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Realm it routes to.
    pub realm: String,
    /// State.
    pub status: DomainStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Verified (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsEmailTemplate {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsEmailTemplateItemsItem>,
}

/// An email template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsEmailTemplateItemsItem {
    /// The realm overrides the built-in template.
    pub customized: bool,
    /// HTML body template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Key (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own).
    pub key: String,
    /// Realm.
    pub realm: String,
    /// Subject template.
    pub subject: String,
    /// Plain-text body template.
    pub text: String,
    /// Last change (epoch ms; built-ins: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// Variables the template uses.
    #[serde(default)]
    pub variables: Vec<String>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsFeature {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsFeatureItemsItem>,
}

/// A feature of the catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsFeatureItemsItem {
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Key (`ai_generation`, `storage`, `api_access`).
    pub key: String,
    /// What it measures.
    pub kind: FeatureKind,
    /// Display name.
    pub name: String,
    /// Permission patterns this feature gates (pipeline step 4). A key
    /// `org:<feature>:<permission>` is gated by feature `<feature>` without
    /// being listed.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Unit shown with limits (`requests`, `bytes`).
    pub unit: String,
    /// Last change.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsLot {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsLotItemsItem>,
}

/// What is left of one top-up or grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsLotItemsItem {
    /// Credits it started with.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// The top-up or grant entry.
    pub entry_id: String,
    /// When it lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `top_up` or `grant`.
    pub kind: EntryKind,
    /// Credits left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsOverride {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsOverrideItemsItem>,
}

/// An exception for a subject, a member or a user everywhere.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsOverrideItemsItem {
    /// When.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// `allow` or `deny` (deny always wins).
    pub effect: String,
    /// Ignored after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Replacement limit (allow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key (`null`: the whole subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    pub reason: String,
    /// Subject (`null`: every subject of `member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPackGrant {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPackGrantItemsItem>,
}

/// A usage pack a subject holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPackGrantItemsItem {
    /// Granted.
    pub created_at: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Pack key (`null` for ad-hoc grants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// Units left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
    /// Units granted.
    pub units: i64,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPlan {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsPlanItemsItem>,
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsPlanItemsItem {
    /// Can be assigned.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Features.
    #[serde(default)]
    pub features: Vec<PlanFeature>,
    /// The plan of subjects without an entitled subscription.
    pub is_default: bool,
    /// Key (`free`, `pro`).
    pub key: String,
    /// Free-form data (prices live in FB-70).
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// Trial length offered with this plan.
    pub trial_days: i64,
    /// Last change.
    pub updated_at: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsQuotaPolicy {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsQuotaPolicyItemsItem>,
}

/// Enforcement of a feature's limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsQuotaPolicyItemsItem {
    /// Units one admitted operation needs (pipeline step 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,
    /// Hard or soft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<Enforcement>,
    /// Feature key (set from the path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Default lease length for concurrency features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_secs: Option<i64>,
    /// Soft limits: how far past the limit usage may go, in percent (`null`:
    /// no ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_pct: Option<i64>,
    /// Permission patterns pipeline step 6 admits against this quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Most units that may roll over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_cap: Option<i64>,
    /// Calendar periods: share of the limit that may roll over unused, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_pct: Option<i64>,
    /// Counter scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<QuotaScope>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsRate {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsRateItemsItem>,
}

/// The price of a metered feature in credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsRateItemsItem {
    /// What is charged.
    pub basis: RateBasis,
    /// Created.
    pub created_at: String,
    /// Credits per `per_units` units.
    pub credits_per_unit: i64,
    /// Feature key.
    pub feature: String,
    /// Units the price is for (cost per event: `ceil(units × credits_per_unit / per_units)`).
    pub per_units: i64,
    /// Updated.
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsResponse {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageAlert {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsageAlertItemsItem>,
}

/// A usage alert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageAlertItemsItem {
    /// Raised.
    pub at: String,
    /// Delivered by the alert job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Allowance when raised.
    pub limit: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// 70, 90 or 100.
    pub threshold: i64,
    /// Usage when raised.
    pub used: i64,
    /// Window.
    pub window_start: String,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsagePack {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsagePackItemsItem>,
}

/// A usage pack of the catalog.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsagePackItemsItem {
    /// Can be granted.
    pub active: bool,
    /// Metered feature it adds units to.
    pub feature: String,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Units per pack.
    pub units: i64,
    /// Validity after purchase (`null`: no expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// A list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemsUsageSummary {
    /// Entries.
    #[serde(default)]
    pub items: Vec<ItemsUsageSummaryItemsItem>,
}

/// Current usage of one feature of a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemsUsageSummaryItemsItem {
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Included.
    pub enabled: bool,
    /// Hard or soft.
    pub enforcement: Enforcement,
    /// Feature.
    pub feature: String,
    /// Kind.
    pub kind: FeatureKind,
    /// Allowance (limit plus rolled-over units; `null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// `used` as a percentage of `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Window.
    pub period: Period,
    /// Left including usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by reservations.
    pub reserved: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Committed usage (gauge level, active leases).
    pub used: i64,
    /// Window end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

/// A job. The lease token is never part of it (see \[`ClaimedJob`\]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Claims so far (a lost lease counts).
    pub attempts: i64,
    /// First retry delay in milliseconds; doubles per attempt up to `backoff_max_ms`.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in milliseconds.
    pub backoff_max_ms: i64,
    /// Cancellation was requested while running; workers should stop and `fail`.
    pub cancel_requested: bool,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who enqueued it.
    pub created_by: String,
    /// When it reached a terminal state (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Job id.
    pub id: String,
    /// Deduplication key, unique per queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Handler name.
    pub kind: String,
    /// Message of the last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Attempts before the job is dead-lettered.
    pub max_attempts: i64,
    /// Handler input.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Higher runs first.
    pub priority: i64,
    /// Queue name.
    pub queue: String,
    /// Handler output of a succeeded job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Earliest start (RFC 3339); moves forward while waiting to retry.
    pub run_at: String,
    /// Schedule that enqueued the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    /// Start of the latest attempt (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// State.
    pub status: JobStatus,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Time in milliseconds a worker may hold the job without a heartbeat.
    pub visibility_timeout_ms: i64,
    /// While running: when the lease expires without a heartbeat (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_until: Option<String>,
    /// Worker that holds (or held last) the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// One entry of a job's history.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobEvent {
    /// When (RFC 3339).
    pub at: String,
    /// Attempt number at the time.
    pub attempt: i64,
    /// `enqueued`, `claimed`, `succeeded`, `retry_scheduled`, `dead`,
    /// `cancel_requested`, `cancelled`, `lease_expired`, `released`, `requeued`.
    pub event: String,
    /// Sequence number.
    pub id: i64,
    /// Detail, e.g. the error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Worker involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

/// A page of jobs, newest first.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobPage {
    /// The jobs.
    #[serde(default)]
    pub items: Vec<Job>,
    /// All jobs matching the filter.
    pub total: i64,
}

/// Queue statistics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobStats {
    /// Counts per queue.
    #[serde(default)]
    pub queues: Vec<QueueStats>,
    /// Counts over all queues by state.
    #[serde(default)]
    pub totals: std::collections::HashMap<String, i64>,
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "dead")]
    Dead,
    #[serde(rename = "cancelled")]
    Cancelled,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Answer of `GET .../keys/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KeyValue {
    /// The key.
    pub key: String,
    /// Length: bytes, fields, items, members or entries.
    pub size: i64,
    /// Whether `value` was cut to `limit` items.
    pub truncated: bool,
    /// Milliseconds until expiry; absent when the key does not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
    /// `string`, `hash`, `list`, `set`, `zset` or `stream`.
    pub r#type: String,
    /// The value: string; `{field: value}`; `[items]`; `[{member, score}]`; `[{id, fields}]`.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// Engine-wide statistics.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct KvStats {
    /// Live pub/sub channels.
    pub channels: i64,
    /// Keys removed to stay within the memory budget.
    pub evicted_keys: i64,
    /// Eviction policy: `noeviction`, `allkeys-lru` or `volatile-lru`.
    pub eviction_policy: String,
    /// Keys removed because their TTL passed.
    pub expired_keys: i64,
    /// Reads that found a key.
    pub hits: i64,
    /// Keys of all tenants (expired keys not yet swept included).
    pub keys: i64,
    /// Memory budget in bytes (`0` = unlimited).
    pub max_memory_bytes: i64,
    /// Reads that found nothing.
    pub misses: i64,
    /// Whether changes are written to SQLite.
    pub persistent: bool,
    /// Keys changed in memory and not yet written to SQLite.
    pub unsaved_keys: i64,
    /// Estimated memory in use, bytes.
    pub used_memory_bytes: i64,
}

/// A concurrency lease.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Lease {
    /// Active until (renew to extend).
    pub expires_at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Released.
    pub released: bool,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
}

/// Body of a heartbeat.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LeaseBody {
    /// The lease from the claim.
    pub lease_id: String,
}

/// One immutable ledger row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Who recorded it.
    pub actor: String,
    /// Credits added (positive) or removed (negative).
    pub amount: i64,
    /// When.
    pub at: String,
    /// Balance right after the entry.
    pub balance_after: i64,
    /// Top-up and grant: when the credits lapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Order, payment or invoice reference (FB-70).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Consumption and spend (and their refunds): the feature paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Id.
    pub id: String,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// What it records.
    pub kind: EntryKind,
    /// Top-up and grant: the lot created; reversal and expiry: the lot taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot_id: Option<String>,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Refund: the debit given back; reversal: the top-up or grant taken back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_entry: Option<String>,
    /// Request id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Cursor (increasing in commit order).
    pub seq: i64,
    /// Subject (`org:<id>` or a principal key).
    pub subject: String,
    /// Units paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
    /// Consumption: the FB-24 usage event charged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_event_id: Option<String>,
    /// Consumption: the usage event's `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_seq: Option<i64>,
}

/// A page of ledger entries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LedgerPage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<LedgerEntry>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// Resource limits of one execution.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Limits {
    /// Fuel (roughly: WebAssembly instructions, plus a charge per host call).
    pub fuel: i64,
    /// Executions of this function at the same time; more get `429 function_busy`.
    pub max_concurrency: i64,
    /// Largest linear memory in bytes.
    pub memory_bytes: i64,
    /// Wall-clock limit in milliseconds (including host calls such as fetch).
    pub timeout_ms: i64,
}

/// Limits in a deploy request: every field optional (defaults apply).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LimitsInput {
    /// Fuel per execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<i64>,
    /// Concurrent executions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<i64>,
    /// Memory limit in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<i64>,
    /// Wall-clock limit in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListQueryBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    /// Values of `{:name}` parameters, typed.
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_page: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_total: Option<bool>,
    /// e.g. `-created,name`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ListResponse {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    /// Pass as `cursor` to get the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    pub per_page: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_items: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<i64>,
}

/// A line the function logged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LogLine {
    /// `debug`, `info`, `warn` or `error`.
    pub level: String,
    /// Text.
    pub message: String,
}

/// What is left of one top-up or grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lot {
    /// Credits it started with.
    pub amount: i64,
    /// Created.
    pub created_at: String,
    /// The top-up or grant entry.
    pub entry_id: String,
    /// When it lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Id.
    pub id: String,
    /// `top_up` or `grant`.
    pub kind: EntryKind,
    /// Credits left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
}

/// What a maintenance run did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MaintenanceReport {
    /// Undelivered alerts queued again.
    pub alerts_requeued: i64,
    /// Leases marked expired.
    pub leases_expired: i64,
    /// Reservations released because they expired.
    pub reservations_expired: i64,
    /// Trials that ended without conversion.
    pub trials_expired: i64,
    /// Counter windows opened (with rollover).
    pub windows_opened: i64,
}

/// A member of a channel's presence.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Member {
    /// Who: `user:<collection>/<id>`, `admin:<id>` or `anonymous`.
    pub actor: String,
    /// Connection id (unique per instance).
    pub connection_id: String,
    /// Instance the connection is on.
    pub instance: String,
    /// When the member joined (UTC).
    pub joined_at: String,
    /// Public metadata set by the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ValueModel>,
}

/// Body of set add/remove and sorted-set remove.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MembersBody {
    /// Members.
    #[serde(default)]
    pub members: Vec<Bin>,
}

/// Distance function of a vector field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    #[serde(rename = "cosine")]
    Cosine,
    #[serde(rename = "l2")]
    L2,
    #[serde(rename = "dot")]
    Dot,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST /mget`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MgetBody {
    /// Keys to read.
    #[serde(default)]
    pub keys: Vec<String>,
}

/// Body of `POST /mset`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MsetBody {
    /// Entries to write together.
    #[serde(default)]
    pub entries: Vec<MsetEntry>,
}

/// One entry of `POST /mset`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MsetEntry {
    /// Key.
    pub key: String,
    /// Value.
    pub value: Bin,
}

/// One namespace of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NamespaceInfo {
    /// Keys in it.
    pub keys: i64,
    /// Namespace name.
    pub name: String,
}

/// A neighbour of a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Neighbor {
    /// `out` when the queried node is the edge's source, `in` when it is the
    /// target (also for undirected edges).
    pub direction: Direction,
    /// The connecting edge.
    pub edge: Edge,
    /// The node at the other end.
    pub node: Node,
}

/// A page of neighbours.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NeighborPage {
    #[serde(default)]
    pub items: Vec<Neighbor>,
    /// Pass as `offset` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// A new collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewCollection {
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Searches over at most this many candidate vectors are exact (brute
    /// force) instead of approximate. Default from the server configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_threshold: Option<i64>,
    /// Vector fields (at most 16; more can be added later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldSpec>>,
    /// `[A-Za-z][A-Za-z0-9_]{0,62}`, unique in the tenant.
    pub name: String,
}

/// Body of `POST /edges`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewEdge {
    /// `false` for an undirected relationship (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directed: Option<bool>,
    /// Id; generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Property object (default `{}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
    /// Source node id; the node must exist.
    pub source: String,
    /// Target node id; the node must exist.
    pub target: String,
    /// Relationship type, 1 to 64 of `A-Z a-z 0-9 _ . : -`.
    pub r#type: String,
}

/// A job to enqueue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewJob {
    /// First retry delay in ms (default 1000); doubles per attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms (default 1 hour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Earliest start as a delay from now in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<i64>,
    /// Enqueueing the same key twice in a queue returns the first job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Handler name.
    pub kind: String,
    /// Attempts before dead-lettering (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Higher runs first (default 0, range -1000..=1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Queue (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Earliest start, RFC 3339. Mutually exclusive with `delay_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    /// Time in ms a worker may hold the job without a heartbeat (default 60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout_ms: Option<i64>,
}

/// Body of `POST /nodes`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewNode {
    /// Id; generated when absent. 1 to 128 of `A-Z a-z 0-9 _ . : @ -`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Kind, up to 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Property object (default `{}`), up to 64 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// A schedule to create.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewSchedule {
    /// First retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_base_ms: Option<i64>,
    /// Cap of the retry delay in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff_max_ms: Option<i64>,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Start enabled (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Handler name.
    pub kind: String,
    /// Attempts per job (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    /// Unique name.
    pub name: String,
    /// Handler input (default `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Priority (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// Queue (default `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Visibility timeout in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout_ms: Option<i64>,
}

/// A worker to register.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NewWorker {
    /// Jobs it runs at once (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<i64>,
    /// Id to use; generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Handlers it runs (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Queues it works (empty: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queues: Option<Vec<String>>,
}

/// A graph node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Node {
    /// RFC 3339 creation time.
    pub created_at: String,
    /// Node id (unique per tenant).
    pub id: String,
    /// Optional kind, e.g. `User` or `Product`.
    pub label: String,
    /// JSON object of properties.
    #[serde(default)]
    pub properties: serde_json::Value,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// Body of `PUT` (replace) and `PATCH` (merge) on a node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeChange {
    /// New label (unchanged when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `PUT`: replaces the properties. `PATCH`: JSON merge patch (RFC 7396;
    /// `null` removes a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// Result of deleting a node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeDeleted {
    /// Edges that touched the node and were deleted with it.
    pub deleted_edges: i64,
    /// Deleted node id.
    pub id: String,
}

/// Node filter: label and property equality (all must match).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeFilter {
    /// Only nodes with this label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Property equality, e.g. `{"country": "IN"}`. Values are strings, numbers
    /// or booleans; keys are `A-Z a-z 0-9 _ -`. At most 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,
}

/// A page of nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodePage {
    #[serde(default)]
    pub items: Vec<Node>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// An object version.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObjectInfo {
    /// Bucket.
    pub bucket: String,
    /// SHA-256 of the content (hex); also the ETag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Content type.
    pub content_type: String,
    /// RFC 3339 upload time.
    pub created_at: String,
    /// Principal key of the uploader.
    pub created_by: String,
    /// Whether this version is a delete marker.
    pub delete_marker: bool,
    /// Whether this is the current version.
    pub is_latest: bool,
    /// Key.
    pub key: String,
    /// User metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Size in bytes.
    pub size: i64,
    /// Version id.
    pub version_id: String,
}

/// A page of a listing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ObjectPage {
    /// Pass as `cursor` for the next page; `null` at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Objects (current versions only), in key order.
    #[serde(default)]
    pub objects: Vec<ObjectInfo>,
    /// "Directories" when a delimiter was given.
    #[serde(default)]
    pub prefixes: Vec<String>,
}

/// An exception for a subject, a member or a user everywhere.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Override {
    /// When.
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// `allow` or `deny` (deny always wins).
    pub effect: String,
    /// Ignored after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Replacement limit (allow only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key (`null`: the whole subject).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    pub reason: String,
    /// Subject (`null`: every subject of `member`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `POST /entitlements/overrides`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OverrideInput {
    /// `allow` or `deny`.
    pub effect: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature key.
    pub feature: String,
    /// Replacement limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Member principal key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Rolling window length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// A usage pack a subject holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackGrant {
    /// Granted.
    pub created_at: String,
    /// Expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Pack key (`null` for ad-hoc grants).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// Units left.
    pub remaining: i64,
    /// Subject.
    pub subject: String,
    /// Units granted.
    pub units: i64,
}

/// Body of `POST /entitlements/usage-packs`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackGrantInput {
    /// A replay with the same key returns the first grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Pack key.
    pub pack: String,
    /// Number of packs (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
    /// Subject.
    pub subject: String,
}

/// One received part.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PartInfo {
    /// SHA-256 of the part (hex).
    pub checksum_sha256: String,
    /// Part number (1-10000).
    pub part_number: i64,
    /// Size in bytes.
    pub size: i64,
}

/// Body of `POST /shortest-path`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PathQuery {
    /// Direction to follow (default `out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Edge types to follow; empty or absent = all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    /// Id of the first node.
    pub from: String,
    /// Longest path in hops, 1 to 10 (default 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,
    /// Id of the last node.
    pub to: String,
}

/// Result of a shortest-path search (unweighted: fewest hops).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PathResult {
    /// Edges between consecutive nodes.
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Whether a path within `max_depth` exists.
    pub found: bool,
    /// Number of edges on the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<i64>,
    /// Nodes from `from` to `to`.
    #[serde(default)]
    pub nodes: Vec<Node>,
}

/// What a caller still has to accept.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Pending {
    /// Current versions not yet accepted.
    #[serde(default)]
    pub pending: Vec<AgreementRef>,
}

/// How usage is counted over time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "hour")]
    Hour,
    #[serde(rename = "day")]
    Day,
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "lifetime")]
    Lifetime,
    #[serde(rename = "rolling")]
    Rolling,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A permission level on a bucket or key prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "write")]
    Write,
    #[serde(rename = "admin")]
    Admin,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A plan.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Can be assigned.
    pub active: bool,
    /// Created.
    pub created_at: String,
    /// Description.
    pub description: String,
    /// Features.
    #[serde(default)]
    pub features: Vec<PlanFeature>,
    /// The plan of subjects without an entitled subscription.
    pub is_default: bool,
    /// Key (`free`, `pro`).
    pub key: String,
    /// Free-form data (prices live in FB-70).
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Display name.
    pub name: String,
    /// Trial length offered with this plan.
    pub trial_days: i64,
    /// Last change.
    pub updated_at: String,
}

/// A feature in a plan, an add-on, a custom limit or an override.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanFeature {
    /// Included (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Feature key.
    pub feature: String,
    /// Limit per period; `null` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<Period>,
    /// Allowance while the subscription is trialing (replaces `limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_limit: Option<i64>,
    /// Unit shown with the limit (default: the feature's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Length of a `rolling` window in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_secs: Option<i64>,
}

/// Body of `PUT /plans/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanInput {
    /// Can be assigned (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The plan's features (replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<PlanFeature>>,
    /// Make this the default plan (unsets the previous default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    /// Free-form data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Display name (default: the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Trial length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_days: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanStepSchema {
    pub detail: String,
    pub id: i64,
    pub parent: i64,
}

/// Body of `POST .../lists/{key}/pop`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopBody {
    /// How many to remove (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Head or tail.
    pub side: Side,
}

/// Result of a ledger write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Posted {
    /// The entry.
    pub entry: LedgerEntry,
    /// The idempotency key was seen before: this is the first result.
    pub replayed: bool,
}

/// Members of a channel.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PresenceList {
    /// Channel name.
    pub channel: String,
    /// Members (every instance).
    #[serde(default)]
    pub members: Vec<Member>,
}

/// A rendered message.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Preview {
    /// HTML body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Variables the template uses and the request did not provide.
    #[serde(default)]
    pub missing: Vec<String>,
    /// Subject.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
}

/// Renders a template without sending.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PreviewInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<TemplateInput>,
    /// Render the stored (or built-in) template of this key …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

/// Progress of a rebuild.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Progress {
    /// Vectors added so far.
    pub done: i64,
    /// Vectors to add.
    pub total: i64,
}

/// The public face of a realm: what dashboards and hosted pages need.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PublicBranding {
    /// Accent colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    /// Background colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    /// Product name (the realm id when none is set).
    pub display_name: String,
    /// Verified custom domains of the realm.
    #[serde(default)]
    pub domains: Vec<String>,
    /// URL of the favicon (when uploaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_url: Option<String>,
    /// URL of the logo (when uploaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Primary colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
    /// Realm.
    pub realm: String,
    /// Sign-in footer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_footer: Option<String>,
    /// Sign-in sub-heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_subtitle: Option<String>,
    /// Sign-in heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signin_title: Option<String>,
    /// Support email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    /// Text colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

/// Body of `POST /publish/{channel}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishBody {
    /// The message.
    pub message: Bin,
}

/// Body of `POST .../lists/{key}/push`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushBody {
    /// Head or tail.
    pub side: Side,
    /// Values, pushed one after the other.
    #[serde(default)]
    pub values: Vec<Bin>,
}

/// Element type of the ANN index. Canonical vectors are always stored as
/// float32; a smaller type makes the derived index smaller and faster at some
/// cost in precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quantization {
    #[serde(rename = "f32")]
    F32,
    #[serde(rename = "f16")]
    F16,
    #[serde(rename = "i8")]
    I8,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueryPlanSchema {
    #[serde(default)]
    pub full_scans: Vec<String>,
    #[serde(default)]
    pub indexes_used: Vec<String>,
    pub sql: String,
    #[serde(default)]
    pub steps: Vec<PlanStepSchema>,
}

/// Counts for one queue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QueueStats {
    /// Cancelled (until purged).
    pub cancelled: i64,
    /// In the dead-letter queue.
    pub dead: i64,
    /// Queue name.
    pub queue: String,
    /// Waiting (including delayed and waiting to retry).
    pub queued: i64,
    /// Leased by a worker.
    pub running: i64,
    /// Finished successfully (until purged).
    pub succeeded: i64,
}

/// Enforcement of a feature's limit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct QuotaPolicy {
    /// Units one admitted operation needs (pipeline step 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<i64>,
    /// Hard or soft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<Enforcement>,
    /// Feature key (set from the path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Default lease length for concurrency features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ttl_secs: Option<i64>,
    /// Soft limits: how far past the limit usage may go, in percent (`null`:
    /// no ceiling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overage_pct: Option<i64>,
    /// Permission patterns pipeline step 6 admits against this quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    /// Most units that may roll over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_cap: Option<i64>,
    /// Calendar periods: share of the limit that may roll over unused, 0-100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollover_pct: Option<i64>,
    /// Counter scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<QuotaScope>,
}

/// Which counter a quota uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaScope {
    #[serde(rename = "subject")]
    Subject,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "team")]
    Team,
    #[serde(rename = "resource")]
    Resource,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The price of a metered feature in credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    /// What is charged.
    pub basis: RateBasis,
    /// Created.
    pub created_at: String,
    /// Credits per `per_units` units.
    pub credits_per_unit: i64,
    /// Feature key.
    pub feature: String,
    /// Units the price is for (cost per event: `ceil(units × credits_per_unit / per_units)`).
    pub per_units: i64,
    /// Updated.
    pub updated_at: String,
}

/// What usage of a feature costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateBasis {
    #[serde(rename = "units")]
    Units,
    #[serde(rename = "overage")]
    Overage,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `PUT /credits/rates/{feature}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RateInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<RateBasis>,
    /// Credits per `per_units` units (at least 1).
    pub credits_per_unit: i64,
    /// Units the price is for (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_units: Option<i64>,
}

/// Counters of one instance (also exported as Prometheus metrics).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RealtimeStats {
    /// Open connections.
    pub connections: i64,
    /// Events sent to subscribers (after authorisation).
    pub events_delivered: i64,
    /// Events published to the bus.
    pub events_dispatched: i64,
    /// Events withheld because the subscriber may not see them.
    pub events_withheld: i64,
    /// Newest event cursor published to the bus.
    pub head: i64,
    /// Connections refused by a limit.
    pub rejected: i64,
    /// Connections closed because their credential became invalid.
    pub revoked: i64,
    /// Connections closed as slow consumers.
    pub slow_consumers: i64,
    /// Active subscriptions.
    pub subscriptions: i64,
}

/// The last completed rebuild.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildInfo {
    /// Detail of the reason.
    pub detail: String,
    /// Wall-clock duration.
    pub duration_ms: i64,
    /// Completion time.
    pub finished: String,
    /// Why it ran (`missing`, `corrupt`, `stale`, `version_mismatch`,
    /// `params_changed`, `manual`, `apply_failed`).
    pub reason: String,
    /// Vectors indexed.
    pub vectors: i64,
}

/// Body of a rebuild request.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildRequest {
    /// Only this field (default: every field of the collection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// Indexes being rebuilt.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RebuildResponse {
    /// One entry per field.
    #[serde(default)]
    pub indexes: Vec<IndexStatus>,
}

/// Result of a recall check: ANN results compared with exact results.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecallReport {
    /// Total time of the ANN searches.
    pub ann_ms: f64,
    /// Total time of the exact searches.
    pub exact_ms: f64,
    /// Vector field.
    pub field: String,
    /// Results compared per query.
    pub k: i64,
    /// When it ran.
    pub measured: String,
    /// Mean of |ANN ∩ exact| / k, from 0 to 1.
    pub recall: f64,
    /// Queries run (stored vectors used as queries).
    pub samples: i64,
}

/// Body of a recall check.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecallRequest {
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Results per query (default 10, at most 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Number of queries (default 100, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<i64>,
}

/// What the reconciler did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReconcileReport {
    /// Blobs the metadata names but the driver lacks (reported, not repaired).
    #[serde(default)]
    pub missing_blobs: Vec<String>,
    /// Stored blobs without metadata removed.
    pub orphan_blobs_deleted: i64,
    /// Staged files removed (abandoned parts and single uploads).
    pub staging_deleted: i64,
    /// Unreferenced blobs (no object version uses them) removed.
    pub unreferenced_blobs_deleted: i64,
    /// Expired upload sessions aborted.
    pub uploads_aborted: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordVersionSchema {
    pub actor: String,
    pub changed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// `create`, `update` or `delete`.
    pub op: String,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RecordVersions {
    #[serde(default)]
    pub items: Vec<RecordVersionSchema>,
}

/// Body of a lease renewal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RenewBody {
    /// New hold from now (default: the policy's lease length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
}

/// A usage reservation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    /// Units recorded at commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_units: Option<i64>,
    /// Created.
    pub created_at: String,
    /// Held until.
    pub expires_at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Of which past a soft limit.
    pub overage_units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// `true` when returned for a replayed idempotency key.
    pub replayed: bool,
    /// Counter scope key.
    pub scope_key: String,
    /// State.
    pub status: ReservationStatus,
    /// Subject.
    pub subject: String,
    /// Units held.
    pub units: i64,
}

/// State of a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReservationStatus {
    #[serde(rename = "held")]
    Held,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "released")]
    Released,
    #[serde(rename = "expired")]
    Expired,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Version to restore.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RestoreBody {
    /// Key.
    pub key: String,
    /// Version id.
    pub version_id: String,
}

/// Result of a reversal: takes back what is left of the top-up or grant, up
/// to the amount asked; the rest was already spent (the balance never goes
/// negative).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Reversal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<LedgerEntry>,
    /// The idempotency key was seen before.
    pub replayed: bool,
    /// Credits taken back.
    pub reversed: i64,
    /// Credits asked for but already spent or lapsed.
    pub unrecovered: i64,
}

/// Body of a rollback.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RollbackBody {
    /// Version to activate (default: the newest version before the active one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

/// Answer of `GET /{ns}/keys`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScanResponse {
    /// Pass back as `cursor` for the next page; `"0"` when done.
    pub cursor: String,
    /// Keys of this page.
    #[serde(default)]
    pub keys: Vec<String>,
}

/// A recurring job definition.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Schedule {
    /// First retry delay in ms.
    pub backoff_base_ms: i64,
    /// Cap of the retry delay in ms.
    pub backoff_max_ms: i64,
    /// Creation time (RFC 3339).
    pub created_at: String,
    /// Who created it.
    pub created_by: String,
    /// Cron expression (UTC; 5 fields, or 6 with leading seconds).
    pub cron: String,
    /// Fires only while enabled.
    pub enabled: bool,
    /// Schedule id.
    pub id: String,
    /// Handler name.
    pub kind: String,
    /// Job of the last firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_job_id: Option<String>,
    /// Last firing (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    /// Attempts per job.
    pub max_attempts: i64,
    /// Unique name.
    pub name: String,
    /// Next firing (RFC 3339).
    pub next_run_at: String,
    /// Handler input.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Priority of the jobs.
    pub priority: i64,
    /// Queue of the jobs.
    pub queue: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
    /// Visibility timeout of the jobs in ms.
    pub visibility_timeout_ms: i64,
}

/// Schedules.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ScheduleList {
    /// By name.
    #[serde(default)]
    pub items: Vec<Schedule>,
}

/// Changes to a schedule.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchedulePatch {
    /// New cron expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Enable or disable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// New payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// New priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchemaChangeSchema {
    pub action: String,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<serde_json::Value>,
    pub applied_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    pub id: i64,
    #[serde(default)]
    pub statements: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SchemaHistory {
    #[serde(default)]
    pub items: Vec<SchemaChangeSchema>,
}

/// One scored member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredMember {
    /// Member.
    pub member: Bin,
    /// Score (a finite number, or the strings are not accepted: use `exec` for infinities).
    pub score: f64,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchHit {
    /// Distance in the field's metric (lower is closer).
    pub distance: f64,
    /// Item id.
    pub id: String,
    /// Metadata, unless `include_metadata` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Similarity (higher is closer): cosine similarity, dot product, or
    /// `1 / (1 + euclidean distance)`.
    pub score: f64,
    /// Text, when `include_text` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector, when `include_vectors` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f64>>,
}

/// A similarity search.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    /// Brute force instead of the ANN index (ground truth, small sets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
    /// Vector field; may be omitted when the collection has exactly one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Metadata filter in the filter language, e.g.
    /// `category = 'news' && year >= 2020`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// When the filter is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<FilterMode>,
    /// Return each hit's metadata (default `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_metadata: Option<bool>,
    /// Return each hit's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_text: Option<bool>,
    /// Return each hit's vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    /// Number of results, 1 to 1000 (default 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
    /// Values for `{:name}` parameters of the filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Query vector.
    #[serde(default)]
    pub vector: Vec<f64>,
}

/// Search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Items matching the filter, when the filter was evaluated first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<i64>,
    /// Closest first.
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    /// How the search was answered.
    pub strategy: Strategy,
    /// Server-side time in milliseconds.
    pub took_ms: f64,
}

/// Body of a secret.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretBody {
    /// The value (sealed at rest, never returned).
    pub value: String,
}

/// A secret's metadata (values are never returned).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretInfo {
    /// Created (RFC 3339).
    pub created_at: String,
    /// Name.
    pub name: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// Secrets (names only).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SecretList {
    /// By name.
    #[serde(default)]
    pub items: Vec<SecretInfo>,
}

/// Sends a templated message through the queue.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SendInput {
    /// Template key.
    pub template: String,
    /// Recipient.
    pub to: String,
    /// Variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

/// Body of `PUT .../strings/{key}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetBody {
    /// Keep the key's current TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepttl: Option<bool>,
    /// Only set when the key does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nx: Option<bool>,
    /// Expire after this many milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<i64>,
    /// The value.
    pub value: Bin,
    /// Only set when the key exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xx: Option<bool>,
}

/// Which end of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A request for a signed URL.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SignRequest {
    /// Content type the upload must declare (uploads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// File name to offer on download (`Content-Disposition: attachment`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_as: Option<String>,
    /// Lifetime in seconds (default 900, at most the server limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    /// Object key.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<SignedMethod>,
    /// A specific version (downloads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
}

/// Which HTTP method a signed URL allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignedMethod {
    #[serde(rename = "GET")]
    GET,
    #[serde(rename = "PUT")]
    PUT,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A signed URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedUrl {
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// `GET` or `PUT`.
    pub method: SignedMethod,
    /// Path and query, relative to the server root; usable without credentials
    /// until `expires_at`.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQueryLabelSchema {
    pub collection: String,
    #[serde(default)]
    pub equality_keys: Vec<String>,
    #[serde(default)]
    pub range_keys: Vec<String>,
    #[serde(default)]
    pub sort_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQueryLog {
    #[serde(default)]
    pub items: Vec<SlowQuerySchema>,
    pub threshold_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SlowQuerySchema {
    pub at: String,
    pub duration_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<SlowQueryLabelSchema>,
    pub param_count: i64,
    pub rows: i64,
    /// SQL text; parameter values are never recorded.
    pub sql: String,
}

/// Body of a direct debit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SpendInput {
    /// Credits (at least 1). All or nothing.
    pub amount: i64,
    /// External reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Feature paid for (statistics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<String>,
    /// Idempotency key (unique per subject).
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Subject (default: the caller's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Units paid for (statistics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlBatchBody {
    /// 1 to 1000 statements, run in one all-or-nothing transaction.
    #[serde(default)]
    pub statements: Vec<SqlStatementBody>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlBatchResult {
    #[serde(default)]
    pub items: Vec<SqlResultSchema>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlResultSchema {
    #[serde(default)]
    pub columns: Vec<String>,
    pub duration_ms: f64,
    /// `read`, `write`, `schema` or `maintenance`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_insert_id: Option<i64>,
    #[serde(default)]
    pub rows: Vec<Vec<serde_json::Value>>,
    pub rows_affected: i64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SqlStatementBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<serde_json::Value>>,
    /// One SQL statement; positional parameters `?` / `?1`.
    pub sql: String,
}

/// Instance totals.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StorageStats {
    /// Buckets.
    pub buckets: i64,
    /// Logical bytes of current objects.
    pub logical_bytes: i64,
    /// Current (non-deleted) objects.
    pub objects: i64,
    /// Open upload sessions.
    pub open_uploads: i64,
    /// Physical bytes stored (after deduplication).
    pub stored_bytes: i64,
    /// Object versions, including deleted and older ones.
    pub versions: i64,
}

/// How a search was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    #[serde(rename = "ann")]
    Ann,
    #[serde(rename = "filtered_ann")]
    FilteredAnn,
    #[serde(rename = "post_filter")]
    PostFilter,
    #[serde(rename = "exact")]
    Exact,
    #[serde(rename = "exact_fallback")]
    ExactFallback,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// Body of `POST .../streams/{key}/add`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamAddBody {
    /// Field names and values (sent in name order).
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Bin>,
    /// Entry id (`ms-seq`); generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Keep at most this many entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maxlen: Option<i64>,
}

/// Body of `PUT /entitlements/addons`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubjectAddon {
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
    /// Subject.
    pub subject: String,
}

/// What a subscription listens to.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscribeSpec {
    /// Custom channel name (instead of `collection`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Collection name: changes of its records (with `record`: of one record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Replay every event after this cursor before live events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<i64>,
    /// Filter-language expression the changed record must match
    /// (collection subscriptions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Client-chosen subscription id (1-64 characters of `[A-Za-z0-9_.:-]`),
    /// echoed in every message of this subscription.
    pub id: String,
    /// Values for `{:name}` parameters in `filter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<std::collections::HashMap<String, ValueModel>>,
    /// Record id: only changes of this record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

/// A subject's subscription.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// Add-ons.
    #[serde(default)]
    pub addons: Vec<AddonQuantity>,
    /// Scheduled cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_at: Option<String>,
    /// Current billing period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<String>,
    /// Current billing period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_start: Option<String>,
    /// Monthly quota windows start on this day and time of month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_anchor: Option<String>,
    /// Provider reference (FB-70).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
    /// Last change.
    pub updated_at: String,
}

/// Subscription lifecycle (set by billing, FB-70).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "trialing")]
    Trialing,
    #[serde(rename = "past_due")]
    PastDue,
    #[serde(rename = "paused")]
    Paused,
    #[serde(rename = "canceled")]
    Canceled,
    #[serde(rename = "expired")]
    Expired,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// The billing sync seam's input: the full state of a subject's subscription
/// (FB-70 calls \[`crate::BillingSync::sync_subscription`\]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionSync {
    /// Add-ons (`null`: unchanged; a list replaces the set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addons: Option<Vec<AddonQuantity>>,
    /// Scheduled cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_at: Option<String>,
    /// Current billing period end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<String>,
    /// Current billing period start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_period_start: Option<String>,
    /// Cycle anchor of monthly windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_anchor: Option<String>,
    /// Provider event time: an event older than the last applied one is
    /// ignored (out-of-order webhooks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_at: Option<String>,
    /// Provider event id: a replay of an applied event changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// Provider reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    /// Plan key.
    pub plan: String,
    /// Lifecycle state.
    pub status: SubscriptionStatus,
    /// `org:<id>` or a principal key.
    pub subject: String,
    /// End of the trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<String>,
}

/// Result of a billing sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOutcome {
    /// Plan after the sync.
    pub plan: String,
    /// What happened: `applied`, `replayed` (same event id) or `stale`
    /// (older than the last applied event).
    pub result: String,
    /// Status after the sync.
    pub status: SubscriptionStatus,
    /// Subject.
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TableInfoSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default)]
    pub columns: Vec<ColumnInfoSchema>,
    #[serde(default)]
    pub indexes: Vec<String>,
    /// `table` or `view`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TableList {
    #[serde(default)]
    pub items: Vec<TableInfoSchema>,
}

/// Saves a template.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TemplateInput {
    /// HTML body template (max 128 KiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// Subject template (max 300).
    pub subject: String,
    /// Plain-text body template (max 64 KiB).
    pub text: String,
}

/// How the words of a keyword query combine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextOperator {
    #[serde(rename = "or")]
    Or,
    #[serde(rename = "and")]
    And,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThresholdBody {
    /// Statements slower than this are logged; `0` logs every statement.
    pub threshold_ms: i64,
}

/// Body of `POST /traverse`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraverseQuery {
    /// Direction to follow (default `out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Edge types to follow; empty or absent = all (at most 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<NodeFilter>,
    /// Return the edges between the returned nodes (and the start node).
    /// Default `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_edges: Option<bool>,
    /// Return the start node too (depth 0). Default `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_start: Option<bool>,
    /// Most nodes returned, 1 to 1000 (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Hops, 1 to 10 (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,
    /// Id of the start node.
    pub start: String,
}

/// Result of a traversal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraverseResult {
    /// Edges between the returned nodes (and the start node).
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Reached nodes, nearest first, then by id.
    #[serde(default)]
    pub nodes: Vec<TraversedNode>,
    /// Start node id.
    pub start: String,
    /// More nodes or edges matched than were returned.
    pub truncated: bool,
}

/// A node reached by a traversal.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TraversedNode {
    /// Fewest hops from the start node.
    pub depth: i64,
    /// The node.
    pub node: Node,
}

/// Body of `POST /entitlements/trials`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TrialInput {
    /// Trial length (default: the plan's `trial_days`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<i64>,
    /// Plan key.
    pub plan: String,
    /// Subject.
    pub subject: String,
}

/// Body of `PUT .../keys/{key}/ttl`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TtlBody {
    /// Milliseconds from now until the key expires (positive).
    pub ttl_ms: i64,
}

/// Edges of one type.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TypeCount {
    /// Number of edges.
    pub count: i64,
    /// Relationship type.
    pub r#type: String,
}

/// A multipart or resumable upload session.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Upload {
    /// Bucket.
    pub bucket: String,
    /// Content type of the finished object.
    pub content_type: String,
    /// Declared SHA-256 of the whole object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_sha256: Option<String>,
    /// Declared total size (resumable uploads complete when it is reached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_size: Option<i64>,
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// Session id.
    pub id: String,
    /// Target key.
    pub key: String,
    /// `multipart` (numbered parts, any order) or `resumable` (sequential
    /// offsets).
    pub kind: String,
    /// Received parts.
    #[serde(default)]
    pub parts: Vec<PartInfo>,
    /// Bytes received so far.
    pub received_bytes: i64,
}

/// A new upload session.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UploadInput {
    /// SHA-256 of the whole object, verified on completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Content type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Target key.
    pub key: String,
    /// `multipart` (default) or `resumable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// User metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<std::collections::HashMap<String, String>>,
    /// Total size, when known (required to auto-complete a resumable upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// One item to insert or replace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertItem {
    /// 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item
    /// with this id is replaced completely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// JSON object, filterable with the filter language (`category = 'news'`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Text for keyword search (the full-text side of hybrid search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Vector per field name. Fields left out have no vector for this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectors: Option<std::collections::HashMap<String, Vec<f64>>>,
}

/// Body of an upsert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertRequest {
    /// 1 to 1000 items, written in one transaction.
    #[serde(default)]
    pub items: Vec<UpsertItem>,
}

/// Outcome of an upsert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UpsertResult {
    /// Items that did not exist before.
    pub created: i64,
    /// Ids in request order.
    #[serde(default)]
    pub ids: Vec<String>,
    /// Items replaced.
    pub updated: i64,
}

/// A usage alert.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageAlert {
    /// Raised.
    pub at: String,
    /// Delivered by the alert job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Allowance when raised.
    pub limit: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Subject.
    pub subject: String,
    /// 70, 90 or 100.
    pub threshold: i64,
    /// Usage when raised.
    pub used: i64,
    /// Window.
    pub window_start: String,
}

/// One FB-24 usage event charged in credits.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageCharge {
    /// When it was charged.
    pub at: String,
    /// Credits debited.
    pub charged: i64,
    /// Credits the usage cost.
    pub cost: i64,
    /// The consumption entry (`null` when nothing could be charged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    /// When the usage happened.
    pub event_at: String,
    /// Usage event id.
    pub event_id: String,
    /// Usage event `seq`.
    pub event_seq: i64,
    /// Feature.
    pub feature: String,
    /// Units the rate is for.
    pub per_units: i64,
    /// Credits per `per_units` units applied.
    pub rate: i64,
    /// Credits the balance could not cover (FB-70 may bill them).
    pub shortfall: i64,
    /// Subject.
    pub subject: String,
    /// Units priced.
    pub units: i64,
}

/// A page of usage charges.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageChargePage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<UsageCharge>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// An immutable usage record (FB-70 bills from these).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageEvent {
    /// Who recorded it.
    pub actor: String,
    /// When.
    pub at: String,
    /// Feature.
    pub feature: String,
    /// Id.
    pub id: String,
    /// Idempotency key.
    pub idempotency_key: String,
    /// Caller data.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Of which past a soft limit (billable overage).
    pub overage_units: i64,
    /// Of which from usage packs.
    pub pack_units: i64,
    /// Request id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Reservation committed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    /// Counter scope key.
    pub scope_key: String,
    /// Cursor (increasing).
    pub seq: i64,
    /// `consume`, `reservation` or `gauge`.
    pub source: String,
    /// Subject billed.
    pub subject: String,
    /// Units (in-window plus pack units; gauges: the change).
    pub units: i64,
    /// Window the usage counts in.
    pub window_start: String,
}

/// A page of usage events.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageEventPage {
    /// Oldest first.
    #[serde(default)]
    pub items: Vec<UsageEvent>,
    /// Pass as `after` for the next page (`null`: no more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// A usage pack of the catalog.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsagePack {
    /// Can be granted.
    pub active: bool,
    /// Metered feature it adds units to.
    pub feature: String,
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Units per pack.
    pub units: i64,
    /// Validity after purchase (`null`: no expiry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// Body of `PUT /usage-packs/{key}`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsagePackInput {
    /// Can be granted (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Metered feature.
    pub feature: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Units per pack.
    pub units: i64,
    /// Validity in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_days: Option<i64>,
}

/// A quota check, consumption or reservation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRequest {
    /// Feature key.
    pub feature: String,
    /// Required for consume and reserve: a replay returns the first result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Stored with the usage event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Counter of a `user`, `team` or `resource` quota (default for `user`:
    /// the caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_key: Option<String>,
    /// Subject billed (default: the caller's subject; another subject needs
    /// privileges or membership).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Reservations: hold time in seconds (default 300, at most 86400).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
    /// Units (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<i64>,
}

/// What one run of the usage consumer did.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageRun {
    /// Credits debited.
    pub credits_charged: i64,
    /// Cursor after the run (last usage event `seq` read).
    pub cursor: i64,
    /// Events charged (priced and not charged before).
    pub events_charged: i64,
    /// Usage events read.
    pub events_read: i64,
    /// Credits not covered by balances.
    pub shortfall: i64,
}

/// Current usage of one feature of a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Rolled over from the previous window.
    pub carried_in: i64,
    /// Included.
    pub enabled: bool,
    /// Hard or soft.
    pub enforcement: Enforcement,
    /// Feature.
    pub feature: String,
    /// Kind.
    pub kind: FeatureKind,
    /// Allowance (limit plus rolled-over units; `null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Usage-pack units left.
    pub pack_remaining: i64,
    /// `used` as a percentage of `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Window.
    pub period: Period,
    /// Left including usage packs (`null`: unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<i64>,
    /// Held by reservations.
    pub reserved: i64,
    /// Counter scope key.
    pub scope_key: String,
    /// Committed usage (gauge level, active leases).
    pub used: i64,
    /// Window end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<String>,
    /// Window start.
    pub window_start: String,
}

pub type ValueModel = serde_json::Value;

/// A vector collection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorCollection {
    /// Creation time.
    pub created: String,
    /// Free text.
    pub description: String,
    /// See \[`NewCollection::exact_threshold`\].
    pub exact_threshold: i64,
    /// Vector fields.
    #[serde(default)]
    pub fields: Vec<VectorField>,
    /// Number of items.
    pub items: i64,
    /// Name.
    pub name: String,
    /// Last change of the definition or the data.
    pub updated: String,
}

/// Collections of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorCollectionList {
    /// By name.
    #[serde(default)]
    pub items: Vec<VectorCollection>,
}

/// A vector field of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorField {
    /// HNSW links per node.
    pub connectivity: i64,
    /// Creation time.
    pub created: String,
    /// Number of components.
    pub dimension: i64,
    /// HNSW candidate list size while inserting.
    pub expansion_add: i64,
    /// HNSW candidate list size while searching.
    pub expansion_search: i64,
    /// Distance function.
    pub metric: Metric,
    /// Name.
    pub name: String,
    /// Element type of the ANN index.
    pub quantization: Quantization,
}

/// Statistics of the caller's tenant.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VectorStats {
    /// Indexes currently being rebuilt.
    pub building: i64,
    /// Collections.
    pub collections: i64,
    /// Vectors in loaded indexes.
    pub indexed_vectors: i64,
    /// Vector fields (one ANN index each).
    pub indexes: i64,
    /// Memory used by loaded indexes.
    pub memory_bytes: i64,
}

/// The DNS record that proves ownership.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Verification {
    /// The token stops being accepted at this time (epoch ms).
    pub expires_at: i64,
    /// Record name (`_forgebase-verify.<host>`).
    pub record_name: String,
    /// Always `TXT`.
    pub record_type: String,
    /// Record value (`forgebase-verify=<token>`).
    pub record_value: String,
}

/// Result of a verification attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyResult {
    /// The domain after the attempt.
    pub domain: Domain,
    /// Why not (when not verified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The record was found.
    pub verified: bool,
}

/// Everything about one version except the module itself. Stored as JSON in
/// `_forgebase_function_versions.config`; a rollback restores all of it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionConfig {
    /// Granted capabilities (see \[`CAPABILITIES`\]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Plain (non-secret) environment values, readable with the `env` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// With `fetch`: hosts the function may call (`api.example.com`, or
    /// `*.example.com` for subdomains). Empty = any public host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    /// Limits.
    pub limits: Limits,
}

/// One version of a function.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Granted capabilities (see \[`CAPABILITIES`\]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Plain (non-secret) environment values, readable with the `env` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    /// With `fetch`: hosts the function may call (`api.example.com`, or
    /// `*.example.com` for subdomains). Empty = any public host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hosts: Option<Vec<String>>,
    /// Record hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<HookSpec>>,
    /// Limits.
    pub limits: Limits,
    /// Whether this is the active version.
    pub active: bool,
    /// When it was deployed (RFC 3339).
    pub created_at: String,
    /// Who deployed it.
    pub created_by: String,
    /// SHA-256 of the module, hex.
    pub sha256: String,
    /// Module size in bytes.
    pub size: i64,
    /// Version number (1, 2, ...).
    pub version: i64,
}

/// Versions.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VersionList {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<VersionInfo>,
}

/// Who may read a bucket without an explicit grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    #[serde(rename = "private")]
    Private,
    #[serde(rename = "authenticated")]
    Authenticated,
    #[serde(rename = "public")]
    Public,
    /// A value this SDK version does not know (kept so newer servers do not break old clients).
    #[serde(other)]
    Unknown,
}

/// A worker that registered or claimed jobs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Worker {
    /// Jobs it runs at once.
    pub concurrency: i64,
    /// Worker id.
    pub id: String,
    /// Handlers it runs (empty: all).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Last claim, heartbeat or registration (RFC 3339).
    pub last_seen_at: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Seen in the last minute.
    pub online: bool,
    /// Queues it works (empty: all).
    #[serde(default)]
    pub queues: Vec<String>,
    /// First seen (RFC 3339).
    pub registered_at: String,
}

/// Workers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkerList {
    /// Most recently seen first.
    #[serde(default)]
    pub items: Vec<Worker>,
}

/// Body of `POST .../zsets/{key}/add`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ZAddBody {
    /// Members to add or update.
    #[serde(default)]
    pub members: Vec<ScoredMember>,
}
