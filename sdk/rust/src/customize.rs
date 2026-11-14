//! Product customization per realm (`/api/v1/customize`): custom domains, white-label branding,
//! email templates, the mail provider and the delivery log, legal agreements and acceptance, and
//! the public views a hosted sign-in page needs. `client.customize()`.
//!
//! Every admin call acts on the caller's realm; platform administrators pass `realm` to act on
//! another (`*` is the instance configuration for the mail provider).

use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::Result;
use crate::generated::{
    AcceptInput, AcceptItem, Acceptance, Agreement, AgreementInput, AgreementRef, Branding, BrandingInput, Delivery,
    DeliveryStatus, Domain, DomainInput, EmailTemplate, Pending, Preview, PreviewInput, PublicBranding, SendInput,
    TemplateInput, TestMailInput, TestMailResult, VerifyResult,
};
use crate::http::{Req, Transport, seg, wire_str};

const C: &str = "/api/v1/customize";

/// `logo` or `favicon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandingAsset {
    /// The logo.
    Logo,
    /// The favicon.
    Favicon,
}

impl BrandingAsset {
    fn as_str(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::Favicon => "favicon",
        }
    }
}

/// Filters of [`EmailApi::deliveries`].
#[derive(Debug, Clone, Default)]
pub struct EmailDeliveriesQuery {
    /// Platform administrators: the realm to act on.
    pub realm: Option<String>,
    /// Delivery status.
    pub status: Option<DeliveryStatus>,
    /// Recipient address.
    pub to: Option<String>,
    /// Template key.
    pub template: Option<String>,
    /// Default 50, at most 200.
    pub limit: Option<u32>,
    /// Only rows created before this time (epoch ms).
    pub before: Option<i64>,
}

/// The mail provider of a realm, secrets masked (`********`).
#[derive(Debug, Clone, Deserialize)]
pub struct MailProviderState {
    /// Realm (`*`: the instance).
    #[serde(default)]
    pub realm: String,
    /// `false`: the realm uses the instance's provider, which falls back to the log provider.
    #[serde(default)]
    pub configured: bool,
    /// The settings (`provider`, `from_address`, provider fields).
    #[serde(default)]
    pub config: Value,
    /// Creation time (epoch milliseconds).
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Update time (epoch milliseconds).
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// The settings of [`EmailApi::put_provider`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct MailProviderConfig {
    /// `resend`, `postmark`, `sendgrid`, `mailgun`, `ses`, `smtp` or `log`.
    pub provider: String,
    /// Sender address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
    /// Provider settings (`api_key`, `server_token`, `region`, `host`, ...). Secrets are sealed at
    /// rest; an absent field or `"********"` keeps the stored secret of the same provider type.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, Value>,
}

impl MailProviderConfig {
    /// A provider with no settings yet.
    #[must_use]
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            ..Self::default()
        }
    }

    /// Sets the sender address.
    #[must_use]
    pub fn from_address(mut self, a: impl Into<String>) -> Self {
        self.from_address = Some(a.into());
        self
    }

    /// Sets a provider setting.
    #[must_use]
    pub fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.settings.insert(key.to_owned(), value.into());
        self
    }
}

/// Customization of the realm's look, mail and legal text: `client.customize()`.
#[derive(Clone)]
pub struct CustomizeApi {
    pub(crate) t: Arc<Transport>,
}

impl CustomizeApi {
    /// Custom domains.
    #[must_use]
    pub fn domains(&self) -> DomainsApi {
        DomainsApi { t: self.t.clone() }
    }
    /// White-label branding.
    #[must_use]
    pub fn branding(&self) -> BrandingApi {
        BrandingApi { t: self.t.clone() }
    }
    /// Email templates, provider and delivery log.
    #[must_use]
    pub fn email(&self) -> EmailApi {
        EmailApi { t: self.t.clone() }
    }
    /// Legal agreements.
    #[must_use]
    pub fn agreements(&self) -> AgreementsApi {
        AgreementsApi { t: self.t.clone() }
    }
    /// Unauthenticated views for hosted pages.
    #[must_use]
    pub fn hosted(&self) -> PublicCustomizeApi {
        PublicCustomizeApi { t: self.t.clone() }
    }

    /// URL of the branded API documentation page (HTML), for a link or an iframe.
    #[must_use]
    pub fn docs_url(&self, realm: Option<&str>) -> String {
        with_realm(self.t.url(&format!("{C}/docs")), realm)
    }

    /// The branded API documentation page as HTML.
    pub async fn docs(&self, realm: Option<&str>) -> Result<String> {
        self.t
            .text(
                Req::get(format!("{C}/docs"))
                    .q_opt("realm", realm)
                    .anonymous()
                    .header("Accept", "text/html"),
            )
            .await
    }
}

fn with_realm(url: String, realm: Option<&str>) -> String {
    match realm {
        Some(r) => format!("{url}?realm={}", seg(r)),
        None => url,
    }
}

/// Domains routed to a realm by `Host` header, verified with a DNS TXT record: `customize().domains()`.
#[derive(Clone)]
pub struct DomainsApi {
    t: Arc<Transport>,
}

impl DomainsApi {
    /// The realm's domains.
    pub async fn list(&self, realm: Option<&str>) -> Result<Vec<Domain>> {
        self.t
            .items(Req::get(format!("{C}/domains")).q_opt("realm", realm))
            .await
    }

    /// Adds a pending domain; publish the returned `verification` record in DNS, then [`verify`](Self::verify).
    /// `host` has no scheme, port or path.
    pub async fn add(&self, host: &str, realm: Option<&str>) -> Result<Domain> {
        let input = DomainInput {
            host: host.to_owned(),
            realm: realm.map(str::to_owned),
        };
        self.t.json(Req::post(format!("{C}/domains")).json(&input)?).await
    }

    /// One domain.
    pub async fn get(&self, id: &str) -> Result<Domain> {
        self.t.json(Req::get(format!("{C}/domains/{}", seg(id)))).await
    }

    /// Removes a domain.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("{C}/domains/{}", seg(id)))).await
    }

    /// Checks the DNS record. `verified: false` with a reason when it is not there yet; an expired token is a conflict.
    pub async fn verify(&self, id: &str) -> Result<VerifyResult> {
        self.t
            .json(Req::post(format!("{C}/domains/{}/verify", seg(id))).json_raw(json!({})))
            .await
    }

    /// New verification token after the old one expired.
    pub async fn reissue(&self, id: &str) -> Result<Domain> {
        self.t
            .json(Req::post(format!("{C}/domains/{}/reissue", seg(id))).json_raw(json!({})))
            .await
    }
}

/// White-label branding: `customize().branding()`.
#[derive(Clone)]
pub struct BrandingApi {
    t: Arc<Transport>,
}

impl BrandingApi {
    /// The branding.
    pub async fn get(&self, realm: Option<&str>) -> Result<Branding> {
        self.t
            .json(Req::get(format!("{C}/branding")).q_opt("realm", realm))
            .await
    }

    /// Replaces the branding: a field left out is cleared. Images go through [`upload_asset`](Self::upload_asset).
    pub async fn put(&self, input: &BrandingInput, realm: Option<&str>) -> Result<Branding> {
        self.t
            .json(Req::put(format!("{C}/branding")).q_opt("realm", realm).json(input)?)
            .await
    }

    /// Uploads the logo or favicon (an image of at most 256 KiB, e.g. `image/png`).
    pub async fn upload_asset(
        &self,
        kind: BrandingAsset,
        data: impl Into<Bytes>,
        content_type: &str,
        realm: Option<&str>,
    ) -> Result<Branding> {
        self.t
            .json(
                Req::put(format!("{C}/branding/assets/{}", kind.as_str()))
                    .q_opt("realm", realm)
                    .bytes(data.into(), content_type),
            )
            .await
    }

    /// Removes the logo or favicon.
    pub async fn delete_asset(&self, kind: BrandingAsset, realm: Option<&str>) -> Result<Branding> {
        self.t
            .json(Req::delete(format!("{C}/branding/assets/{}", kind.as_str())).q_opt("realm", realm))
            .await
    }
}

/// Transactional email: templates, preview, sending, the provider and the delivery log: `customize().email()`.
#[derive(Clone)]
pub struct EmailApi {
    t: Arc<Transport>,
}

impl EmailApi {
    /// Built-in templates plus the realm's overrides and custom keys.
    pub async fn templates(&self, realm: Option<&str>) -> Result<Vec<EmailTemplate>> {
        self.t
            .items(Req::get(format!("{C}/email/templates")).q_opt("realm", realm))
            .await
    }

    /// One template.
    pub async fn template(&self, key: &str, realm: Option<&str>) -> Result<EmailTemplate> {
        self.t
            .json(Req::get(format!("{C}/email/templates/{}", seg(key))).q_opt("realm", realm))
            .await
    }

    /// Creates or replaces the realm's template for `key` (`verify_email`, `password_reset`,
    /// `magic_link`, `email_otp`, `invitation`, or your own).
    pub async fn put_template(&self, key: &str, input: &TemplateInput, realm: Option<&str>) -> Result<EmailTemplate> {
        self.t
            .json(
                Req::put(format!("{C}/email/templates/{}", seg(key)))
                    .q_opt("realm", realm)
                    .json(input)?,
            )
            .await
    }

    /// Removes the override: a built-in key falls back to the built-in template.
    pub async fn delete_template(&self, key: &str, realm: Option<&str>) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{C}/email/templates/{}", seg(key))).q_opt("realm", realm))
            .await
    }

    /// Renders a saved template (`template`) or a draft with `variables`; `missing` lists the variables it needs and did not get.
    pub async fn preview(&self, input: &PreviewInput, realm: Option<&str>) -> Result<Preview> {
        self.t
            .json(
                Req::post(format!("{C}/email/preview"))
                    .q_opt("realm", realm)
                    .json(input)?
                    .idempotent(),
            )
            .await
    }

    /// Queues a message from a template (delivery is asynchronous, see [`delivery`](Self::delivery)).
    pub async fn send(&self, input: &SendInput, realm: Option<&str>) -> Result<Delivery> {
        self.t
            .json(Req::post(format!("{C}/email/send")).q_opt("realm", realm).json(input)?)
            .await
    }

    /// Delivery log, newest first.
    pub async fn deliveries(&self, q: &EmailDeliveriesQuery) -> Result<Vec<Delivery>> {
        self.t
            .items(
                Req::get(format!("{C}/email/deliveries"))
                    .q_opt("realm", q.realm.as_deref())
                    .q_opt("status", q.status.as_ref().and_then(wire_str))
                    .q_opt("to", q.to.as_deref())
                    .q_opt("template", q.template.as_deref())
                    .q_opt("limit", q.limit)
                    .q_opt("before", q.before),
            )
            .await
    }

    /// One delivery.
    pub async fn delivery(&self, id: &str) -> Result<Delivery> {
        self.t.json(Req::get(format!("{C}/email/deliveries/{}", seg(id)))).await
    }

    /// Queues a failed delivery again.
    pub async fn retry(&self, id: &str) -> Result<Delivery> {
        self.t
            .json(Req::post(format!("{C}/email/deliveries/{}/retry", seg(id))).json_raw(json!({})))
            .await
    }

    /// Which transport sends the realm's email (platform administrators). `realm` `*` is the instance's.
    pub async fn provider(&self, realm: Option<&str>) -> Result<MailProviderState> {
        self.t
            .json(Req::get(format!("{C}/email/provider")).q_opt("realm", realm))
            .await
    }

    /// Sets the realm's mail provider. Nothing is sent; `400 invalid_mail_config` names a bad setting.
    pub async fn put_provider(&self, config: &MailProviderConfig, realm: Option<&str>) -> Result<MailProviderState> {
        self.t
            .json(
                Req::put(format!("{C}/email/provider"))
                    .q_opt("realm", realm)
                    .json(config)?,
            )
            .await
    }

    /// Removes the realm's provider (it uses the instance's again); a missing one is a not-found error.
    pub async fn delete_provider(&self, realm: Option<&str>) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{C}/email/provider")).q_opt("realm", realm))
            .await
    }

    /// Sends one message to `to` now with the configured provider (not queued, not logged). A
    /// failure is `409 mail_test_failed` carrying the provider's reason.
    pub async fn test_provider(&self, to: &str, realm: Option<&str>) -> Result<TestMailResult> {
        let input = TestMailInput { to: to.to_owned() };
        self.t
            .json(
                Req::post(format!("{C}/email/provider/test"))
                    .q_opt("realm", realm)
                    .json(&input)?,
            )
            .await
    }
}

/// Versioned legal agreements and who accepted which version: `customize().agreements()`.
#[derive(Clone)]
pub struct AgreementsApi {
    t: Arc<Transport>,
}

impl AgreementsApi {
    /// Every version (newest first); `kind` narrows to `terms`, `privacy`, ...
    pub async fn list(&self, realm: Option<&str>, kind: Option<&str>) -> Result<Vec<Agreement>> {
        self.t
            .items(
                Req::get(format!("{C}/agreements"))
                    .q_opt("realm", realm)
                    .q_opt("kind", kind),
            )
            .await
    }

    /// Publishes the next version of an agreement (`kind`: lower-case letters, digits, `_`, `-`). Users must accept it again.
    pub async fn publish(&self, input: &AgreementInput, realm: Option<&str>) -> Result<Agreement> {
        self.t
            .json(Req::post(format!("{C}/agreements")).q_opt("realm", realm).json(input)?)
            .await
    }

    /// Current versions the caller has not accepted yet.
    pub async fn pending(&self) -> Result<Vec<AgreementRef>> {
        let p: Pending = self.t.json(Req::get(format!("{C}/agreements/pending"))).await?;
        Ok(p.pending)
    }

    /// Records the caller's acceptance. An outdated version is a conflict (`agreement_outdated`).
    pub async fn accept(&self, items: Vec<AcceptItem>) -> Result<Vec<Acceptance>> {
        let body = AcceptInput { items };
        self.t
            .items(Req::post(format!("{C}/agreements/accept")).json(&body)?)
            .await
    }

    /// The caller's own acceptance records.
    pub async fn accepted(&self) -> Result<Vec<Acceptance>> {
        self.t.items(Req::get(format!("{C}/agreements/accepted"))).await
    }

    /// Acceptance records of the realm (administrators); `principal` is `user:<collection>/<id>`.
    pub async fn acceptances(
        &self,
        realm: Option<&str>,
        principal: Option<&str>,
        kind: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<Acceptance>> {
        self.t
            .items(
                Req::get(format!("{C}/agreements/acceptances"))
                    .q_opt("realm", realm)
                    .q_opt("principal", principal)
                    .q_opt("kind", kind)
                    .q_opt("limit", limit),
            )
            .await
    }
}

/// What a hosted sign-in page or a dashboard shows before anyone signs in. No credentials are
/// sent. On a verified custom domain the realm is the domain's; elsewhere pass `realm`.
#[derive(Clone)]
pub struct PublicCustomizeApi {
    t: Arc<Transport>,
}

impl PublicCustomizeApi {
    /// Public branding.
    pub async fn branding(&self, realm: Option<&str>) -> Result<PublicBranding> {
        self.t
            .json(
                Req::get(format!("{C}/public/branding"))
                    .q_opt("realm", realm)
                    .anonymous(),
            )
            .await
    }

    /// Current agreement versions, with their text.
    pub async fn agreements(&self, realm: Option<&str>) -> Result<Vec<Agreement>> {
        self.t
            .items(
                Req::get(format!("{C}/public/agreements"))
                    .q_opt("realm", realm)
                    .anonymous(),
            )
            .await
    }

    /// URL of the logo or favicon for an `<img src>`; fetching it is a 404 when none is uploaded.
    #[must_use]
    pub fn asset_url(&self, kind: BrandingAsset, realm: Option<&str>) -> String {
        with_realm(self.t.url(&format!("{C}/public/branding/{}", kind.as_str())), realm)
    }

    /// Downloads the logo or favicon.
    pub async fn asset(&self, kind: BrandingAsset, realm: Option<&str>) -> Result<Bytes> {
        self.t
            .raw(
                Req::get(format!("{C}/public/branding/{}", kind.as_str()))
                    .q_opt("realm", realm)
                    .anonymous(),
            )
            .await
    }

    /// URL of the CSS custom properties (`--fb-primary`, ...) of the branding, for a `<link rel="stylesheet">`.
    #[must_use]
    pub fn theme_url(&self, realm: Option<&str>) -> String {
        with_realm(self.t.url(&format!("{C}/public/theme.css")), realm)
    }

    /// The theme CSS.
    pub async fn theme(&self, realm: Option<&str>) -> Result<String> {
        self.t
            .text(
                Req::get(format!("{C}/public/theme.css"))
                    .q_opt("realm", realm)
                    .anonymous()
                    .header("Accept", "text/css"),
            )
            .await
    }
}
