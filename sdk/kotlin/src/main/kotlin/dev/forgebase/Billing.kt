package dev.forgebase

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject

/** A hosted page at the payment provider or store: send the customer to [url]. */
@Serializable
public data class HostedPage(@SerialName("session_id") val sessionId: String = "", val url: String)

/** The billing customer of a subject. */
@Serializable
public data class BillingCustomer(
    val id: String,
    val subject: String = "",
    val email: String? = null,
    val name: String? = null,
    val currency: String = "",
    val region: String? = null,
    val provider: String? = null,
    val balance: Long = 0,
    @SerialName("has_payment_method") val hasPaymentMethod: Boolean = false,
    @SerialName("payment_method_label") val paymentMethodLabel: String? = null,
    @SerialName("tax_exempt") val taxExempt: Boolean = false,
    @SerialName("created_at") val createdAt: String = "",
)

/** What the app received from the store; each store reads its own fields. */
public data class StoreReceipt(
    /** App Store: the StoreKit 2 signed transaction (JWS). */
    val signedTransaction: String? = null,
    /** App Store: the transaction id. */
    val transactionId: String? = null,
    /** Google Play: the purchase token. */
    val purchaseToken: String? = null,
    /** Google Play: the product id. */
    val productId: String? = null,
    /** Google Play: `subscription` (default) or `one_time`. */
    val kind: String? = null,
    /** Creem: the checkout id of the success redirect. */
    val checkoutId: String? = null,
    /** Subject the purchase is for (default: the caller's). */
    val subject: String? = null,
)

/** Merchant-of-record store helpers: `fb.billing.store` (`app_store`, `google_play`, `creem`). */
public class BillingStoreApi internal constructor(private val t: Transport) {
    /** Verifies a purchase server side with the store and applies it. Idempotent; a purchase bound to another account is a [ConflictException]. */
    public suspend fun verify(provider: String, receipt: StoreReceipt): JsonObject = t.request(
        "/api/v1/billing/store/${seg(provider)}/verify", "POST",
        body = obj(
            "signed_transaction" to receipt.signedTransaction, "transaction_id" to receipt.transactionId, "purchase_token" to receipt.purchaseToken,
            "product_id" to receipt.productId, "kind" to receipt.kind, "checkout_id" to receipt.checkoutId, "subject" to receipt.subject,
        ),
    ) as JsonObject

    /** A hosted checkout at a store that sells on the web (Creem): open the URL, then [verify] with the returned `checkout_id`. */
    public suspend fun checkout(provider: String, price: String, successUrl: String, idempotencyKey: String, subject: String? = null): HostedPage =
        t.request(
            "/api/v1/billing/store/${seg(provider)}/checkout", "POST",
            body = obj("price" to price, "success_url" to successUrl, "idempotency_key" to idempotencyKey, "subject" to subject),
        ).decode()

    /** Store products and the prices they are sold as. */
    public suspend fun products(): List<JsonObject> = t.request("/api/v1/billing/store-products").items().map { it as JsonObject }

    /** Store purchases of [subject] (default: the caller). */
    public suspend fun purchases(subject: String? = null): List<JsonObject> =
        t.request("/api/v1/billing/store-purchases", query = mapOf("subject" to subject)).items().map { it as JsonObject }
}

/** The billing customer: `fb.billing.customers`. */
public class BillingCustomersApi internal constructor(private val t: Transport) {
    /** The customer of [subject] (default: the caller's). */
    public suspend fun get(subject: String? = null): BillingCustomer = t.request("/api/v1/billing/customer", query = mapOf("subject" to subject)).decode()

    /** Creates or updates the customer: billing email, name, currency, tax region. */
    public suspend fun put(
        subject: String? = null, email: String? = null, name: String? = null, currency: String? = null, region: String? = null,
        provider: String? = null, taxExempt: Boolean? = null, metadata: Map<String, Any?>? = null,
    ): BillingCustomer = t.request(
        "/api/v1/billing/customer", "PUT",
        body = obj(
            "subject" to subject, "email" to email, "name" to name, "currency" to currency, "region" to region,
            "provider" to provider, "tax_exempt" to taxExempt, "metadata" to metadata,
        ),
    ).decode()
}

/** What a customer sees and does: `fb.billing.portal`. */
public class BillingPortalApi internal constructor(private val t: Transport) {
    /** The customer, live subscriptions and recent invoices, payments and credit notes of [subject]. */
    public suspend fun get(subject: String? = null): JsonObject = t.request("/api/v1/billing/portal", query = mapOf("subject" to subject)) as JsonObject

    /** A provider-hosted page that saves or replaces the payment method: send the customer to its URL. */
    public suspend fun paymentMethod(subject: String? = null, returnUrl: String? = null): HostedPage =
        t.request("/api/v1/billing/portal/payment-method", "POST", body = obj("subject" to subject, "return_url" to returnUrl)).decode()
}

/**
 * Billing from the customer's side: `fb.billing`. The catalog, providers, settings, operator views and the rest of the
 * admin surface (orders, invoices, refunds) are left to the JS and Rust SDKs; use [ForgeBase.request] for a route
 * not covered here.
 */
public class BillingApi internal constructor(private val t: Transport) {
    public val store: BillingStoreApi = BillingStoreApi(t)
    public val customers: BillingCustomersApi = BillingCustomersApi(t)
    public val portal: BillingPortalApi = BillingPortalApi(t)

    /** Subscriptions of [subject] (default: the caller's). */
    public suspend fun subscriptions(subject: String? = null): List<JsonObject> =
        t.request("/api/v1/billing/subscriptions", query = mapOf("subject" to subject)).items().map { it as JsonObject }

    /** Invoices of [subject] (default: the caller's). */
    public suspend fun invoices(subject: String? = null, status: String? = null, limit: Int? = null): List<JsonObject> =
        t.request("/api/v1/billing/invoices", query = mapOf("subject" to subject, "status" to status, "limit" to limit)).items().map { it as JsonObject }
}
