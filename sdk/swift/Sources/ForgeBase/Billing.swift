import Foundation

public enum StoreProvider: String, Sendable {
    case appStore = "app_store"
    case googlePlay = "google_play"
    case creem
}

/// What the app received from the store; each store reads its own fields.
public struct StoreReceipt: Sendable {
    /// App Store: the StoreKit 2 signed transaction (`Transaction.jsonRepresentation`'s JWS: `VerificationResult.jwsRepresentation`).
    public var signedTransaction: String?
    /// App Store: the transaction id (read back with the App Store Server API).
    public var transactionId: String?
    /// Google Play: the purchase token.
    public var purchaseToken: String?
    public var productId: String?
    /// Google Play: `subscription` (default) or `one_time`.
    public var kind: String?
    /// Creem: the checkout id of the success redirect.
    public var checkoutId: String?
    /// Subject the purchase is for (default: the caller's).
    public var subject: String?

    public init(
        signedTransaction: String? = nil, transactionId: String? = nil, purchaseToken: String? = nil, productId: String? = nil,
        kind: String? = nil, checkoutId: String? = nil, subject: String? = nil
    ) {
        self.signedTransaction = signedTransaction
        self.transactionId = transactionId
        self.purchaseToken = purchaseToken
        self.productId = productId
        self.kind = kind
        self.checkoutId = checkoutId
        self.subject = subject
    }
}

/// A price to buy: `price` is the catalog price key.
public struct BillingItem: Sendable {
    public var price: String
    public var quantity: Int64?
    public init(price: String, quantity: Int64? = nil) {
        self.price = price
        self.quantity = quantity
    }
    fileprivate var json: JSON { wire(["price": price, "quantity": quantity]) }
}

public struct BillingCustomerInput: Sendable {
    public var subject: String?
    public var email: String?
    public var name: String?
    public var currency: String?
    /// Tax region (`US-CA`, `IN`, ...).
    public var region: String?
    public var provider: String?
    public var taxExempt: Bool?
    public var metadata: JSONObject?
    public init(
        subject: String? = nil, email: String? = nil, name: String? = nil, currency: String? = nil, region: String? = nil,
        provider: String? = nil, taxExempt: Bool? = nil, metadata: JSONObject? = nil
    ) {
        self.subject = subject
        self.email = email
        self.name = name
        self.currency = currency
        self.region = region
        self.provider = provider
        self.taxExempt = taxExempt
        self.metadata = metadata
    }
}

/// Customer-facing billing: `fb.billing`. The catalog, providers, settings and operator views (stats, dunning,
/// maintenance) are administrator APIs left to the JavaScript and Rust SDKs.
public struct BillingAPI: Sendable {
    public let customers: BillingCustomersAPI
    /// What a customer sees: invoices, subscriptions and the hosted payment-method page.
    public let portal: BillingPortalAPI
    public let subscriptions: BillingSubscriptionsAPI
    public let invoices: BillingInvoicesAPI
    /// Store purchases (StoreKit 2, Google Play, Creem).
    public let store: BillingStoreAPI

    init(transport t: Transport) {
        customers = BillingCustomersAPI(t: t)
        portal = BillingPortalAPI(t: t)
        subscriptions = BillingSubscriptionsAPI(t: t)
        invoices = BillingInvoicesAPI(t: t)
        store = BillingStoreAPI(t: t)
    }
}

/// The billing customer of a subject.
public struct BillingCustomersAPI: Sendable {
    let t: Transport

    /// The customer of `subject` (default: the caller's).
    public func get(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/billing/customer", query: query(["subject": subject])))
    }

    /// Creates or updates the customer of a subject: billing email, currency, tax region, provider.
    public func put(_ i: BillingCustomerInput) async throws -> JSON {
        try await t.json(
            Request(
                "PUT", "/api/v1/billing/customer",
                body: wire([
                    "subject": i.subject, "email": i.email, "name": i.name, "currency": i.currency, "region": i.region,
                    "provider": i.provider, "tax_exempt": i.taxExempt, "metadata": i.metadata.map(JSON.object),
                ])))
    }
}

public struct BillingPortalAPI: Sendable {
    let t: Transport

    /// The customer, live subscriptions and recent invoices, payments and credit notes of `subject`.
    public func get(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/billing/portal", query: query(["subject": subject])))
    }

    /// A provider-hosted page that saves or replaces the payment method: open `url` in `SFSafariViewController` /
    /// `ASWebAuthenticationSession`. Returns `(url, sessionId)`.
    public func paymentMethod(subject: String? = nil, returnURL: String? = nil) async throws -> (url: URL, sessionId: String) {
        let j = try await t.json(Request("POST", "/api/v1/billing/portal/payment-method", body: wire(["subject": subject, "return_url": returnURL])))
        guard let s = j["url"]?.stringValue, let u = URL(string: s) else { throw ForgeBaseError.invalidResponse("hosted page has no url") }
        return (u, j["session_id"]?.stringValue ?? "")
    }
}

public struct BillingSubscriptionsAPI: Sendable {
    let t: Transport

    /// Subscriptions of `subject` (default: the caller's).
    public func list(subject: String? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/billing/subscriptions", query: query(["subject": subject])))["items"]?.arrayValue ?? []
    }

    public func get(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/billing/subscriptions/\(seg(id))"))
    }

    /// Subscribes: a trial, or the first invoice collected (`payment_url` when the customer must pay). A replayed
    /// `idempotencyKey` returns the first result with `replayed: true`; a second live subscription is a `.conflict` error.
    public func create(
        items: [BillingItem], idempotencyKey: String, subject: String? = nil, trialDays: Int? = nil, coupon: String? = nil,
        returnURL: String? = nil, metadata: JSONObject? = nil
    ) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "/api/v1/billing/subscriptions",
                body: wire([
                    "items": items.map(\.json), "idempotency_key": idempotencyKey, "subject": subject, "trial_days": trialDays,
                    "coupon": coupon, "return_url": returnURL, "metadata": metadata.map(JSON.object),
                ])))
    }

    /// Upgrades or downgrades with proration.
    public func change(_ id: String, price: String, quantity: Int64? = nil, atPeriodEnd: Bool? = nil) async throws -> JSON {
        try await t.json(
            Request("POST", "/api/v1/billing/subscriptions/\(seg(id))/change", body: wire(["price": price, "quantity": quantity, "at_period_end": atPeriodEnd])))
    }

    /// Cancels at the period end (default) or now (`atPeriodEnd: false`).
    public func cancel(_ id: String, atPeriodEnd: Bool? = nil, reason: String? = nil) async throws -> JSON {
        try await t.json(Request("POST", "/api/v1/billing/subscriptions/\(seg(id))/cancel", body: wire(["at_period_end": atPeriodEnd, "reason": reason])))
    }

    /// Resumes a paused subscription or withdraws a scheduled cancellation.
    public func resume(_ id: String) async throws -> JSON {
        try await t.json(Request("POST", "/api/v1/billing/subscriptions/\(seg(id))/resume"))
    }
}

public struct BillingInvoicesAPI: Sendable {
    let t: Transport

    /// Invoices of `subject`, newest first. `limit` is at most 500.
    public func list(subject: String? = nil, status: String? = nil, limit: Int? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/billing/invoices", query: query(["subject": subject, "status": status, "limit": limit])))["items"]?.arrayValue ?? []
    }

    /// One invoice with its lines.
    public func get(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/billing/invoices/\(seg(id))"))
    }

    /// Pays an open invoice now: with the saved method, else the answer carries the hosted page `payment_url`.
    public func pay(_ id: String, returnURL: String? = nil) async throws -> JSON {
        try await t.json(Request("POST", "/api/v1/billing/invoices/\(seg(id))/pay", body: wire(["return_url": returnURL])))
    }
}

/// Merchant-of-record store helpers.
public struct BillingStoreAPI: Sendable {
    let t: Transport

    /// Verifies a purchase server-side with the store and applies it. Idempotent; a purchase bound to another account is a `.conflict`.
    ///
    ///     for await result in Transaction.updates {
    ///         guard case .verified(let tx) = result else { continue }
    ///         _ = try await fb.billing.store.verify(.appStore, StoreReceipt(signedTransaction: result.jwsRepresentation))
    ///         await tx.finish()
    ///     }
    public func verify(_ provider: StoreProvider, _ receipt: StoreReceipt) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "/api/v1/billing/store/\(seg(provider.rawValue))/verify",
                body: wire([
                    "signed_transaction": receipt.signedTransaction, "transaction_id": receipt.transactionId,
                    "purchase_token": receipt.purchaseToken, "product_id": receipt.productId, "kind": receipt.kind,
                    "checkout_id": receipt.checkoutId, "subject": receipt.subject,
                ])))
    }

    /// A hosted checkout at a store that sells on the web (Creem): open `url`, then `verify` with the returned `checkout_id`.
    public func checkout(_ provider: StoreProvider, price: String, successURL: String, idempotencyKey: String, subject: String? = nil) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "/api/v1/billing/store/\(seg(provider.rawValue))/checkout",
                body: wire(["price": price, "success_url": successURL, "idempotency_key": idempotencyKey, "subject": subject])))
    }

    /// Store products and the prices they are sold as.
    public func products() async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/billing/store-products"))["items"]?.arrayValue ?? []
    }

    /// Store purchases of `subject` (default: the caller).
    public func purchases(subject: String? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/billing/store-purchases", query: query(["subject": subject])))["items"]?.arrayValue ?? []
    }
}
