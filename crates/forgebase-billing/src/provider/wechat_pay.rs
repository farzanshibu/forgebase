//! WeChat Pay API v3 (FB-72), against a sandbox or mock endpoint only.
//!
//! - **Requests** are signed: `Authorization: WECHATPAY2-SHA256-RSA2048
//!   mchid=…,nonce_str=…,signature=…,timestamp=…,serial_no=…`, the signature
//!   being RSA-SHA256 (PKCS#1 v1.5) of `METHOD\nPATH\nTIMESTAMP\nNONCE\nBODY\n`
//!   under the merchant private key whose certificate serial is `serial_no`.
//! - **Responses and notifications** are verified with the WeChat Pay
//!   platform key named by `Wechatpay-Serial` (a platform certificate or a
//!   platform public key, several during a rotation; unknown serials and
//!   expired certificates are refused), over `TIMESTAMP\nNONCE\nBODY\n`, with
//!   the timestamp inside the webhook tolerance.
//! - **Notification resources** are AEAD_AES_256_GCM encrypted under the
//!   merchant's APIv3 key and decrypted here; the decrypted `mchid` must be
//!   ours. Notifications are deduplicated by their `id`.
//! - Customers pay invoices on a Native (QR code) payment; refunds go
//!   through `/v3/refund/domestic/refunds`. WeChat Pay has no test keys, so
//!   the production API host is refused (test mode only, like the other
//!   drivers).

use std::sync::Arc;

use base64::Engine as _;
use forgebase_auth::social::http_client::{HttpClient, HttpRequest, HttpResponse};
use forgebase_db::backend::BoxFuture;
use forgebase_entitlements::Clock;
use forgebase_entitlements::model::parse_iso;
use ring::aead;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use super::{
    ChargeRequest, ChargeResult, CustomerRef, EventAction, HostedPaymentRequest, HostedSetupRequest, PaymentProvider,
    ProviderEvent, RefundRequest, RefundResult, der, header, json_body, jws, parse_ok, str_at,
};
use crate::error::{BillingError, Result};
use crate::model::{HostedPage, RefundStatus};
use crate::money::hex;

/// Production API hosts (refused).
pub const PRODUCTION_HOSTS: [&str; 2] = ["api.mch.weixin.qq.com", "api2.mch.weixin.qq.com"];

/// Account settings.
#[derive(Debug, Clone, Default)]
pub struct WechatPayConfig {
    /// Merchant id.
    pub mchid: String,
    /// App id the payments are made in.
    pub appid: String,
    /// Serial of the merchant API certificate.
    pub merchant_serial: String,
    /// Merchant API private key (PEM PKCS#8 RSA).
    pub private_key: String,
    /// APIv3 key (32 bytes) that decrypts notifications.
    pub api_v3_key: String,
    /// Platform keys: `(serial, PEM certificate or public key)`.
    pub platform_keys: Vec<(String, String)>,
    /// Public URL of `POST /api/v1/billing/webhooks/wechat_pay`.
    pub notify_url: String,
}

impl WechatPayConfig {
    /// From the provider account: `key_id` is the merchant id, `api_key` the
    /// APIv3 key, and `config` holds `appid`, `merchant_serial`,
    /// `private_key`, `platform_keys` (`[{"serial", "pem"}]`) and `notify_url`.
    pub fn from_json(mchid: &str, api_v3_key: &str, v: &Json) -> Self {
        let s = |k: &str| v.get(k).and_then(Json::as_str).unwrap_or_default().trim().to_owned();
        Self {
            mchid: mchid.trim().to_owned(),
            appid: s("appid"),
            merchant_serial: s("merchant_serial"),
            private_key: s("private_key"),
            api_v3_key: api_v3_key.trim().to_owned(),
            platform_keys: v
                .get("platform_keys")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
                .filter_map(|k| Some((str_at(k, &["serial"])?.to_owned(), str_at(k, &["pem"])?.to_owned())))
                .collect(),
            notify_url: s("notify_url"),
        }
    }
}

#[derive(Debug, Clone)]
struct PlatformKey {
    serial: String,
    rsa: Vec<u8>,
    validity: Option<(i64, i64)>,
}

/// The WeChat Pay driver.
#[derive(Clone)]
pub struct WechatPayProvider {
    cfg: WechatPayConfig,
    key: Vec<u8>,
    platform: Vec<PlatformKey>,
    api_base: String,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for WechatPayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WechatPayProvider")
            .field("mchid", &self.cfg.mchid)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

fn forged(msg: &str) -> BillingError {
    BillingError::webhook("invalid_signature", msg.to_owned())
}

/// RSA-SHA256 of a WeChat Pay message, base64 (merchant requests; tests sign
/// platform messages with it).
pub fn sign_message(pkcs8_pem: &str, msg: &str) -> Result<String> {
    let key = der::pem_der(pkcs8_pem, "PRIVATE KEY")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(jws::rsa_sign(&key, msg.as_bytes())?))
}

/// Encrypts a notification resource like WeChat Pay (tests): base64 of the
/// AES-256-GCM ciphertext and tag.
pub fn encrypt_resource(api_v3_key: &str, nonce: &str, aad: &str, plain: &[u8]) -> Result<String> {
    let (key, nonce) = aead_parts(api_v3_key, nonce)?;
    let mut buf = plain.to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::from(aad.as_bytes()), &mut buf)
        .map_err(|_| BillingError::Internal("encryption failed".into()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf))
}

fn aead_parts(api_v3_key: &str, nonce: &str) -> Result<(aead::LessSafeKey, aead::Nonce)> {
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, api_v3_key.as_bytes())
        .map_err(|_| BillingError::invalid("invalid_key", "the APIv3 key must be 32 bytes"))?;
    let nonce: [u8; 12] = nonce
        .as_bytes()
        .try_into()
        .map_err(|_| forged("the resource nonce must be 12 bytes"))?;
    Ok((aead::LessSafeKey::new(key), aead::Nonce::assume_unique_for_key(nonce)))
}

/// Decrypts a notification resource.
pub fn decrypt_resource(api_v3_key: &str, resource: &Json) -> Result<Json> {
    if str_at(resource, &["algorithm"]) != Some("AEAD_AES_256_GCM") {
        return Err(forged("unsupported resource algorithm"));
    }
    let (key, nonce) = aead_parts(api_v3_key, str_at(resource, &["nonce"]).unwrap_or_default())?;
    let aad = resource
        .get("associated_data")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let mut buf = base64::engine::general_purpose::STANDARD
        .decode(str_at(resource, &["ciphertext"]).unwrap_or_default())
        .map_err(|_| forged("the resource ciphertext is not base64"))?;
    let plain = key
        .open_in_place(nonce, aead::Aad::from(aad.as_bytes()), &mut buf)
        .map_err(|_| forged("the resource does not decrypt with the APIv3 key"))?;
    serde_json::from_slice(plain).map_err(|_| forged("the decrypted resource is not JSON"))
}

/// A WeChat Pay order number (6 to 32 characters) for an idempotency key.
fn trade_no(key: &str) -> String {
    hex(&Sha256::digest(key.as_bytes()))[..32].to_owned()
}

impl WechatPayProvider {
    /// A driver for a sandbox or mock endpoint (`api_base` is required).
    pub fn new(
        cfg: WechatPayConfig,
        api_base: Option<&str>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let base = api_base.map(|b| b.trim_end_matches('/').to_owned()).ok_or_else(|| {
            BillingError::invalid(
                "live_mode_refused",
                "WeChat Pay has no test keys: set api_base to a sandbox or mock endpoint",
            )
        })?;
        let host = url::Url::parse(&base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        if PRODUCTION_HOSTS.contains(&host.as_str()) {
            return Err(BillingError::invalid(
                "live_mode_refused",
                "the production WeChat Pay API is refused",
            ));
        }
        if cfg.mchid.is_empty() || !cfg.mchid.bytes().all(|b| b.is_ascii_digit()) {
            return Err(BillingError::invalid(
                "invalid_config",
                "key_id must be the merchant id (digits)",
            ));
        }
        for (v, name) in [
            (&cfg.appid, "appid"),
            (&cfg.merchant_serial, "merchant_serial"),
            (&cfg.notify_url, "notify_url"),
        ] {
            if v.is_empty() {
                return Err(BillingError::invalid("invalid_config", format!("{name} is required")));
            }
        }
        if cfg.api_v3_key.len() != 32 {
            return Err(BillingError::invalid(
                "invalid_key",
                "the APIv3 key (api_key) must be 32 bytes",
            ));
        }
        let key = der::pem_der(&cfg.private_key, "PRIVATE KEY")?;
        jws::rsa_public(&key)?;
        let mut platform = Vec::new();
        for (serial, pem) in &cfg.platform_keys {
            let blocks = der::pem_blocks(pem);
            let (label, der_bytes) = blocks
                .first()
                .ok_or_else(|| BillingError::invalid("invalid_config", "a platform key is not PEM"))?;
            let (rsa, validity, cert_serial) = match label.as_str() {
                "CERTIFICATE" => {
                    let info = der::cert_info(der_bytes)?;
                    (
                        der::spki_key(info.spki)?.to_vec(),
                        Some((info.not_before, info.not_after)),
                        Some(info.serial_hex),
                    )
                }
                "PUBLIC KEY" => (der::spki_key(der_bytes)?.to_vec(), None, None),
                _ => {
                    return Err(BillingError::invalid(
                        "invalid_config",
                        "a platform key must be a certificate or a public key",
                    ));
                }
            };
            der::rsa_components(&rsa)?;
            if let Some(cs) = cert_serial
                && !cs.eq_ignore_ascii_case(serial)
            {
                return Err(BillingError::invalid(
                    "invalid_config",
                    format!("platform certificate serial {cs} does not match {serial}"),
                ));
            }
            platform.push(PlatformKey {
                serial: serial.to_ascii_uppercase(),
                rsa,
                validity,
            });
        }
        if platform.is_empty() {
            return Err(BillingError::invalid("invalid_config", "platform_keys is required"));
        }
        Ok(Self {
            cfg,
            key,
            platform,
            api_base: base,
            http,
            clock,
        })
    }

    /// Verifies a platform signature over `ts\nnonce\nbody\n`.
    fn verify(&self, headers: &[(String, String)], body: &[u8], now_ms: i64, tolerance_secs: i64) -> Result<()> {
        let get = |n: &str| header(headers, n).ok_or_else(|| forged("missing Wechatpay signature headers"));
        let (ts, nonce, sig, serial) = (
            get("wechatpay-timestamp")?,
            get("wechatpay-nonce")?,
            get("wechatpay-signature")?,
            get("wechatpay-serial")?,
        );
        let key = self
            .platform
            .iter()
            .find(|k| k.serial.eq_ignore_ascii_case(serial.trim()))
            .ok_or_else(|| forged("unknown Wechatpay-Serial (platform key not configured)"))?;
        if key.validity.is_some_and(|(nb, na)| now_ms < nb || na < now_ms) {
            return Err(forged("the platform certificate is expired or not yet valid"));
        }
        let ts_secs: i64 = ts.trim().parse().map_err(|_| forged("bad Wechatpay-Timestamp"))?;
        if (now_ms / 1000 - ts_secs).abs() > tolerance_secs {
            return Err(BillingError::webhook(
                "stale_signature",
                "the Wechatpay-Timestamp is outside the tolerance",
            ));
        }
        let sig = base64::engine::general_purpose::STANDARD
            .decode(sig.trim())
            .map_err(|_| forged("the Wechatpay-Signature is not base64"))?;
        let mut msg = format!("{}\n{}\n", ts.trim(), nonce.trim()).into_bytes();
        msg.extend_from_slice(body);
        msg.push(b'\n');
        if !jws::rsa_verify(&key.rsa, &msg, &sig) {
            return Err(forged("the Wechatpay-Signature does not verify"));
        }
        Ok(())
    }

    async fn send(&self, method: &'static str, path_query: &str, body: Option<&Json>) -> Result<Json> {
        let ts = self.clock.now_ms() / 1000;
        let nonce = uuid::Uuid::new_v4().simple().to_string().to_ascii_uppercase();
        let body_text = body.map(Json::to_string).unwrap_or_default();
        let sig = base64::engine::general_purpose::STANDARD.encode(jws::rsa_sign(
            &self.key,
            format!("{method}\n{path_query}\n{ts}\n{nonce}\n{body_text}\n").as_bytes(),
        )?);
        let mut req = HttpRequest::get(format!("{}{path_query}", self.api_base))
            .header(
                "authorization",
                format!(
                    "WECHATPAY2-SHA256-RSA2048 mchid=\"{}\",nonce_str=\"{nonce}\",signature=\"{sig}\",timestamp=\"{ts}\",serial_no=\"{}\"",
                    self.cfg.mchid, self.cfg.merchant_serial
                ),
            )
            .header("accept", "application/json");
        if method == "POST" {
            req.method = "POST";
            req.body = body_text.into_bytes();
            req = req.header("content-type", "application/json");
        }
        let res: HttpResponse = self.http.send(req).await?;
        if (200..300).contains(&res.status) {
            self.verify(&res.headers, &res.body, self.clock.now_ms(), 300)
                .map_err(|e| BillingError::Provider(format!("unverified WeChat Pay response: {e}")))?;
        } else {
            let v: Json = serde_json::from_slice(&res.body).unwrap_or(Json::Null);
            let msg = str_at(&v, &["message"]).unwrap_or("request refused");
            return Err(BillingError::Provider(format!("HTTP {}: {msg}", res.status)));
        }
        parse_ok(&res)
    }
}

impl PaymentProvider for WechatPayProvider {
    fn name(&self) -> &'static str {
        "wechat_pay"
    }

    fn account(&self) -> Option<String> {
        Some(self.cfg.mchid.clone()).filter(|a| !a.is_empty())
    }

    fn create_customer<'a>(&'a self, c: &'a CustomerRef) -> BoxFuture<'a, Result<String>> {
        // WeChat Pay has no customer object; the payer is an openid per payment.
        Box::pin(async move { Ok(c.customer_id.clone()) })
    }

    fn charge<'a>(&'a self, _r: &'a ChargeRequest) -> BoxFuture<'a, Result<ChargeResult>> {
        Box::pin(async move {
            Ok(ChargeResult::failed(
                "not_supported",
                "WeChat Pay has no off-session charges; the customer pays the invoice by QR code",
            ))
        })
    }

    fn refund<'a>(&'a self, r: &'a RefundRequest) -> BoxFuture<'a, Result<RefundResult>> {
        Box::pin(async move {
            let enc: String = url::form_urlencoded::byte_serialize(r.payment_ref.as_bytes()).collect();
            let tx = self
                .send(
                    "GET",
                    &format!("/v3/pay/transactions/id/{enc}?mchid={}", self.cfg.mchid),
                    None,
                )
                .await?;
            let total = tx
                .get("amount")
                .and_then(|a| a.get("total"))
                .and_then(Json::as_i64)
                .ok_or_else(|| BillingError::Provider("WeChat Pay returned no transaction amount".into()))?;
            let mut body = json!({
                "transaction_id": r.payment_ref,
                "out_refund_no": trade_no(&r.idempotency_key),
                "notify_url": self.cfg.notify_url,
                "amount": {"refund": r.amount, "total": total, "currency": r.currency.to_ascii_uppercase()},
            });
            if let Some(reason) = &r.reason {
                body["reason"] = json!(reason.chars().take(80).collect::<String>());
            }
            let v = self.send("POST", "/v3/refund/domestic/refunds", Some(&body)).await?;
            Ok(RefundResult {
                provider_ref: str_at(&v, &["refund_id"]).map(str::to_owned),
                status: match str_at(&v, &["status"]) {
                    Some("SUCCESS") => RefundStatus::Succeeded,
                    Some("PROCESSING") | None => RefundStatus::Pending,
                    _ => RefundStatus::Failed,
                },
            })
        })
    }

    fn hosted_payment<'a>(&'a self, r: &'a HostedPaymentRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            if !r.currency.eq_ignore_ascii_case("CNY") {
                return Err(BillingError::invalid(
                    "currency_not_supported",
                    "WeChat Pay Native payments are in CNY",
                ));
            }
            let mut description = String::new();
            for c in r.description.chars() {
                if description.len() + c.len_utf8() > 127 {
                    break;
                }
                description.push(c);
            }
            let out_trade_no = trade_no(&r.idempotency_key);
            let body = json!({
                "appid": self.cfg.appid,
                "mchid": self.cfg.mchid,
                "description": if description.is_empty() { "ForgeBase invoice".to_owned() } else { description },
                "out_trade_no": out_trade_no,
                "notify_url": self.cfg.notify_url,
                "attach": r.invoice_id,
                "amount": {"total": r.amount, "currency": "CNY"},
            });
            let v = self.send("POST", "/v3/pay/transactions/native", Some(&body)).await?;
            Ok(HostedPage {
                url: str_at(&v, &["code_url"])
                    .ok_or_else(|| BillingError::Provider("WeChat Pay returned no code_url".into()))?
                    .to_owned(),
                session_id: out_trade_no,
            })
        })
    }

    fn hosted_setup<'a>(&'a self, _r: &'a HostedSetupRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            Err(BillingError::invalid(
                "not_supported",
                "WeChat Pay stores no payment methods",
            ))
        })
    }

    fn verify_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
        now_ms: i64,
        tolerance_secs: i64,
    ) -> Result<ProviderEvent> {
        self.verify(headers, body, now_ms, tolerance_secs)?;
        let v = json_body(body)?;
        let id = str_at(&v, &["id"])
            .ok_or_else(|| BillingError::webhook("invalid_payload", "no notification id"))?
            .to_owned();
        let event_type = str_at(&v, &["event_type"]).unwrap_or_default().to_owned();
        let created_at = str_at(&v, &["create_time"]).and_then(parse_iso).unwrap_or(now_ms);
        let res = decrypt_resource(&self.cfg.api_v3_key, v.get("resource").unwrap_or(&Json::Null))?;
        if str_at(&res, &["mchid"]) != Some(self.cfg.mchid.as_str()) {
            return Err(forged("the notification is for another merchant"));
        }
        let txn = str_at(&res, &["transaction_id"]).map(str::to_owned);
        let action = if event_type.starts_with("TRANSACTION.") {
            let payment_ref = txn
                .clone()
                .or_else(|| str_at(&res, &["out_trade_no"]).map(str::to_owned))
                .unwrap_or_default();
            let invoice_id = str_at(&res, &["attach"]).map(str::to_owned);
            match str_at(&res, &["trade_state"]) {
                Some("SUCCESS") => EventAction::PaymentSucceeded {
                    invoice_id,
                    payment_ref,
                    amount: res
                        .get("amount")
                        .and_then(|a| a.get("total"))
                        .and_then(Json::as_i64)
                        .unwrap_or(0),
                    currency: res
                        .get("amount")
                        .and_then(|a| str_at(a, &["currency"]))
                        .unwrap_or("CNY")
                        .to_ascii_uppercase(),
                    provider_customer: res.get("payer").and_then(|p| str_at(p, &["openid"])).map(str::to_owned),
                    method: None,
                },
                Some(state @ ("CLOSED" | "PAYERROR" | "REVOKED")) => EventAction::PaymentFailed {
                    invoice_id,
                    payment_ref,
                    code: Some(state.to_ascii_lowercase()),
                    message: str_at(&res, &["trade_state_desc"]).map(str::to_owned),
                },
                _ => EventAction::Other,
            }
        } else if event_type.starts_with("REFUND.") {
            EventAction::RefundUpdated {
                refund_ref: str_at(&res, &["refund_id"]).unwrap_or_default().to_owned(),
                payment_ref: txn,
                status: match str_at(&res, &["refund_status"]) {
                    Some("SUCCESS") => RefundStatus::Succeeded,
                    Some("PROCESSING") => RefundStatus::Pending,
                    _ => RefundStatus::Failed,
                },
            }
        } else {
            EventAction::Other
        };
        Ok(ProviderEvent {
            id,
            event_type,
            created_at,
            action,
        })
    }
}
