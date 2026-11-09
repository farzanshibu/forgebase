//! Request and response types. Times are epoch milliseconds.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

/// DNS label prefix of the verification record.
pub const VERIFY_LABEL: &str = "_forgebase-verify";
/// Prefix of the verification record's value.
pub const VERIFY_PREFIX: &str = "forgebase-verify=";

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

/// State of a custom domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainStatus {
    /// Waiting for the DNS TXT record.
    Pending,
    /// Proven; requests with this `Host` reach the realm.
    Verified,
}

impl DomainStatus {
    pub(crate) fn parse(s: &str) -> Self {
        if s == "verified" { Self::Verified } else { Self::Pending }
    }
}

/// The DNS record that proves ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Verification {
    /// Record name (`_forgebase-verify.<host>`).
    pub record_name: String,
    /// Always `TXT`.
    pub record_type: String,
    /// Record value (`forgebase-verify=<token>`).
    pub record_value: String,
    /// The token stops being accepted at this time (epoch ms).
    pub expires_at: i64,
}

/// A custom domain of a realm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Domain {
    /// Id.
    pub id: String,
    /// Realm it routes to.
    pub realm: String,
    /// Host name (lower case).
    pub host: String,
    /// State.
    pub status: DomainStatus,
    /// What to publish in DNS (while pending).
    pub verification: Option<Verification>,
    /// Created (epoch ms).
    pub created_at: i64,
    /// Verified (epoch ms).
    pub verified_at: Option<i64>,
    /// Last verification attempt (epoch ms).
    pub last_checked: Option<i64>,
    /// Why the last attempt failed.
    pub last_error: Option<String>,
    /// Who added it.
    pub created_by: String,
}

/// Adds a domain.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DomainInput {
    /// Host name, e.g. `login.example.com` (no scheme, port or path).
    pub host: String,
    /// Realm (platform administrators; default: the caller's realm).
    #[serde(default)]
    pub realm: Option<String>,
}

/// Result of a verification attempt.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerifyResult {
    /// The record was found.
    pub verified: bool,
    /// Why not (when not verified).
    pub reason: Option<String>,
    /// The domain after the attempt.
    pub domain: Domain,
}

// ---------------------------------------------------------------------------
// Branding
// ---------------------------------------------------------------------------

/// White-label branding of a realm.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Branding {
    /// Realm.
    pub realm: String,
    /// Product name shown to users.
    pub display_name: Option<String>,
    /// Primary colour (`#rgb` or `#rrggbb`).
    pub primary_color: Option<String>,
    /// Page background.
    pub background_color: Option<String>,
    /// Text colour.
    pub text_color: Option<String>,
    /// Accent colour.
    pub accent_color: Option<String>,
    /// Sign-in page heading (plain text).
    pub signin_title: Option<String>,
    /// Sign-in page sub-heading (plain text).
    pub signin_subtitle: Option<String>,
    /// Sign-in page footer (plain text).
    pub signin_footer: Option<String>,
    /// Support contact shown on hosted pages and in email.
    pub support_email: Option<String>,
    /// A logo is uploaded.
    pub has_logo: bool,
    /// A favicon is uploaded.
    pub has_favicon: bool,
    /// Last change (epoch ms).
    pub updated_at: Option<i64>,
}

/// Replaces the branding of a realm; a field left out is cleared.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct BrandingInput {
    /// Product name (max 80).
    #[serde(default)]
    pub display_name: Option<String>,
    /// Primary colour.
    #[serde(default)]
    pub primary_color: Option<String>,
    /// Page background.
    #[serde(default)]
    pub background_color: Option<String>,
    /// Text colour.
    #[serde(default)]
    pub text_color: Option<String>,
    /// Accent colour.
    #[serde(default)]
    pub accent_color: Option<String>,
    /// Sign-in heading (max 120).
    #[serde(default)]
    pub signin_title: Option<String>,
    /// Sign-in sub-heading (max 300).
    #[serde(default)]
    pub signin_subtitle: Option<String>,
    /// Sign-in footer (max 300).
    #[serde(default)]
    pub signin_footer: Option<String>,
    /// Support email.
    #[serde(default)]
    pub support_email: Option<String>,
}

/// The public face of a realm: what dashboards and hosted pages need.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicBranding {
    /// Realm.
    pub realm: String,
    /// Product name (the realm id when none is set).
    pub display_name: String,
    /// Primary colour.
    pub primary_color: Option<String>,
    /// Background colour.
    pub background_color: Option<String>,
    /// Text colour.
    pub text_color: Option<String>,
    /// Accent colour.
    pub accent_color: Option<String>,
    /// Sign-in heading.
    pub signin_title: Option<String>,
    /// Sign-in sub-heading.
    pub signin_subtitle: Option<String>,
    /// Sign-in footer.
    pub signin_footer: Option<String>,
    /// Support email.
    pub support_email: Option<String>,
    /// URL of the logo (when uploaded).
    pub logo_url: Option<String>,
    /// URL of the favicon (when uploaded).
    pub favicon_url: Option<String>,
    /// Verified custom domains of the realm.
    pub domains: Vec<String>,
}

/// Which image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    /// The logo.
    Logo,
    /// The favicon.
    Favicon,
}

impl AssetKind {
    /// Parses the path segment.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "logo" => Some(Self::Logo),
            "favicon" => Some(Self::Favicon),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::Favicon => "favicon",
        }
    }
}

/// A stored image.
#[derive(Debug, Clone)]
pub struct Asset {
    /// Media type.
    pub content_type: String,
    /// Bytes.
    pub bytes: Vec<u8>,
    /// Entity tag.
    pub etag: String,
}

// ---------------------------------------------------------------------------
// Email
// ---------------------------------------------------------------------------

/// An email template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailTemplate {
    /// Realm.
    pub realm: String,
    /// Key (`verify_email`, `password_reset`, `magic_link`, `email_otp`, `invitation`, or your own).
    pub key: String,
    /// Subject template.
    pub subject: String,
    /// Plain-text body template.
    pub text: String,
    /// HTML body template.
    pub html: Option<String>,
    /// The realm overrides the built-in template.
    pub customized: bool,
    /// Variables the template uses.
    pub variables: Vec<String>,
    /// Last change (epoch ms; built-ins: none).
    pub updated_at: Option<i64>,
}

/// Saves a template.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TemplateInput {
    /// Subject template (max 300).
    pub subject: String,
    /// Plain-text body template (max 64 KiB).
    pub text: String,
    /// HTML body template (max 128 KiB).
    #[serde(default)]
    pub html: Option<String>,
}

/// Renders a template without sending.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PreviewInput {
    /// Render the stored (or built-in) template of this key …
    #[serde(default)]
    pub template: Option<String>,
    /// … or this draft.
    #[serde(default)]
    pub draft: Option<TemplateInput>,
    /// Variables.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub variables: Json,
}

/// A rendered message.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Preview {
    /// Subject.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
    /// HTML body.
    pub html: Option<String>,
    /// Variables the template uses and the request did not provide.
    pub missing: Vec<String>,
}

/// Sends a templated message through the queue.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SendInput {
    /// Template key.
    pub template: String,
    /// Recipient.
    pub to: String,
    /// Variables.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub variables: Json,
}

/// State of a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Waiting for the first attempt.
    Queued,
    /// An attempt failed; another is scheduled.
    Retrying,
    /// Handed to the provider.
    Sent,
    /// Gave up.
    Failed,
}

impl DeliveryStatus {
    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "retrying" => Self::Retrying,
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            _ => Self::Queued,
        }
    }

    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Retrying => "retrying",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }
}

/// One row of the delivery log (never the body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Delivery {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm: String,
    /// Template key.
    pub template: Option<String>,
    /// Recipient.
    pub to: String,
    /// Subject as sent.
    pub subject: String,
    /// State.
    pub status: DeliveryStatus,
    /// Attempts made.
    pub attempts: i64,
    /// Last failure.
    pub last_error: Option<String>,
    /// Provider that handled it.
    pub provider: Option<String>,
    /// Queue job.
    pub job_id: Option<String>,
    /// Queued (epoch ms).
    pub created_at: i64,
    /// Last change (epoch ms).
    pub updated_at: i64,
    /// Handed over (epoch ms).
    pub sent_at: Option<i64>,
}

/// Outcome of one delivery attempt (what the job handler reports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// Sent (or already sent earlier).
    Sent,
    /// Failed; the queue should try again.
    Retry(String),
    /// Failed for good.
    Failed(String),
}

// ---------------------------------------------------------------------------
// Agreements
// ---------------------------------------------------------------------------

/// One version of a legal agreement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Agreement {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm: String,
    /// Kind (`terms`, `privacy`, ...).
    pub kind: String,
    /// Version (1, 2, ...).
    pub version: i64,
    /// Title.
    pub title: String,
    /// Text (plain text or Markdown; escape when rendering).
    pub body: String,
    /// Canonical URL.
    pub url: Option<String>,
    /// Published (epoch ms).
    pub published_at: i64,
    /// Who published it.
    pub published_by: String,
}

/// Publishes the next version of an agreement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AgreementInput {
    /// Kind: lower-case letters, digits, `_`, `-` (max 64).
    pub kind: String,
    /// Title (max 200).
    pub title: String,
    /// Text (max 200 KiB).
    pub body: String,
    /// Canonical URL (`http(s)://`).
    #[serde(default)]
    pub url: Option<String>,
}

/// A version of an agreement without its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgreementRef {
    /// Kind.
    pub kind: String,
    /// Version.
    pub version: i64,
    /// Title.
    pub title: String,
}

/// Versions to accept.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AcceptInput {
    /// Agreements and the version accepted.
    pub items: Vec<AcceptItem>,
}

/// One agreement version accepted.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AcceptItem {
    /// Kind.
    pub kind: String,
    /// Version (must be the current one).
    pub version: i64,
}

/// Acceptance record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Acceptance {
    /// Id.
    pub id: String,
    /// Realm.
    pub realm: String,
    /// Who (principal key).
    pub principal: String,
    /// Kind.
    pub kind: String,
    /// Version.
    pub version: i64,
    /// When (epoch ms).
    pub accepted_at: i64,
    /// From which address.
    pub ip: Option<String>,
    /// With which client.
    pub user_agent: Option<String>,
}

/// What a caller still has to accept.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Pending {
    /// Current versions not yet accepted.
    pub pending: Vec<AgreementRef>,
}
