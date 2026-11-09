//! Manual billing: invoices are paid out of band (bank transfer, cheque)
//! and an operator records the payment (`POST /billing/invoices/{id}/mark-paid`).
//! Charges stay pending, refunds are recorded as done, there are no hosted
//! pages and no webhooks.

use forgebase_db::backend::BoxFuture;

use super::{
    ChargeRequest, ChargeResult, CustomerRef, HostedPaymentRequest, HostedSetupRequest, PaymentProvider, ProviderEvent,
    RefundRequest, RefundResult,
};
use crate::error::{BillingError, Result};
use crate::model::{HostedPage, PaymentStatus, RefundStatus};

/// The manual provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManualProvider;

impl PaymentProvider for ManualProvider {
    fn name(&self) -> &'static str {
        "manual"
    }

    fn create_customer<'a>(&'a self, c: &'a CustomerRef) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(c.customer_id.clone()) })
    }

    fn charge<'a>(&'a self, _r: &'a ChargeRequest) -> BoxFuture<'a, Result<ChargeResult>> {
        Box::pin(async move {
            Ok(ChargeResult {
                status: PaymentStatus::Pending,
                provider_ref: None,
                failure_code: None,
                failure_message: None,
            })
        })
    }

    fn refund<'a>(&'a self, r: &'a RefundRequest) -> BoxFuture<'a, Result<RefundResult>> {
        Box::pin(async move {
            Ok(RefundResult {
                provider_ref: Some(format!("manual:{}", r.idempotency_key)),
                status: RefundStatus::Succeeded,
            })
        })
    }

    fn hosted_payment<'a>(&'a self, _r: &'a HostedPaymentRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            Err(BillingError::invalid(
                "not_supported",
                "manual billing has no hosted payment page",
            ))
        })
    }

    fn hosted_setup<'a>(&'a self, _r: &'a HostedSetupRequest) -> BoxFuture<'a, Result<HostedPage>> {
        Box::pin(async move {
            Err(BillingError::invalid(
                "not_supported",
                "manual billing stores no payment methods",
            ))
        })
    }

    fn verify_webhook(&self, _h: &[(String, String)], _b: &[u8], _n: i64, _t: i64) -> Result<ProviderEvent> {
        Err(BillingError::NotConfigured(
            "manual billing receives no webhooks".into(),
        ))
    }
}
