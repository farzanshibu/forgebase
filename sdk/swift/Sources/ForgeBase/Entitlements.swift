import Foundation

/// The answer to "would these units be granted?".
public struct Decision: Codable, Sendable, Equatable {
    public let allowed: Bool
    public let feature: String
    public let limit: Int64?
    public let remaining: Int64?
    public let used: Int64
    public let reserved: Int64
    public let units: Int64
    public let overageUnits: Int64
    public let packRemaining: Int64
    public let reason: String?
    public let subject: String
    public let scopeKey: String
    public let windowStart: String
    public let windowEnd: String?

    enum CodingKeys: String, CodingKey {
        case allowed, feature, limit, remaining, used, reserved, units, reason, subject
        case overageUnits = "overage_units", packRemaining = "pack_remaining", scopeKey = "scope_key"
        case windowStart = "window_start", windowEnd = "window_end"
    }
}

public struct Consumption: Codable, Sendable, Equatable {
    public let eventId: String
    /// A replayed idempotency key returns the first result.
    public let replayed: Bool
    public let decision: Decision
    enum CodingKeys: String, CodingKey { case eventId = "event_id", replayed, decision }
}

public struct Reservation: Codable, Sendable, Equatable {
    public let id: String
    public let feature: String
    public let subject: String
    public let scopeKey: String
    public let units: Int64
    public let status: String
    public let expiresAt: String
    public let committedUnits: Int64?
    public let replayed: Bool

    enum CodingKeys: String, CodingKey {
        case id, feature, subject, units, status, replayed
        case scopeKey = "scope_key", expiresAt = "expires_at", committedUnits = "committed_units"
    }
}

public struct Lease: Codable, Sendable, Equatable {
    public let id: String
    public let feature: String
    public let subject: String
    public let scopeKey: String
    public let expiresAt: String
    public let released: Bool
    public let replayed: Bool

    enum CodingKeys: String, CodingKey {
        case id, feature, subject, released, replayed
        case scopeKey = "scope_key", expiresAt = "expires_at"
    }
}

/// A feature check: `.feature("export")`, `.permission("billing:read")` or `.plan("pro")`.
public enum EntitlementQuery: Sendable {
    case feature(String, subject: String? = nil)
    case permission(String)
    case plan(String, subject: String? = nil)

    var items: [URLQueryItem] {
        switch self {
        case .feature(let f, let s): return query(["feature": f, "subject": s])
        case .permission(let p): return query(["permission": p])
        case .plan(let p, let s): return query(["plan": p, "subject": s])
        }
    }
}

public struct QuotaRequest: Sendable {
    public var feature: String
    /// Units (default 1).
    public var units: Int64?
    /// Counter for per-user / team / resource quotas.
    public var scopeKey: String?
    /// Billed subject (default: the caller's).
    public var subject: String?
    public var metadata: JSONObject?
    /// Required for consume / reserve / lease: a replay returns the first result.
    public var idempotencyKey: String?
    public var ttlSeconds: Int?

    public init(
        feature: String, units: Int64? = nil, scopeKey: String? = nil, subject: String? = nil, metadata: JSONObject? = nil,
        idempotencyKey: String? = nil, ttlSeconds: Int? = nil
    ) {
        self.feature = feature
        self.units = units
        self.scopeKey = scopeKey
        self.subject = subject
        self.metadata = metadata
        self.idempotencyKey = idempotencyKey
        self.ttlSeconds = ttlSeconds
    }

    fileprivate var body: JSON {
        wire([
            "feature": feature, "units": units, "scope_key": scopeKey, "subject": subject,
            "metadata": metadata.map(JSON.object), "idempotency_key": idempotencyKey, "ttl_secs": ttlSeconds,
        ])
    }
}

/// Entitlement checks and quota helpers: `fb.entitlements`. Plan, feature and add-on administration lives in the
/// JavaScript and Rust SDKs.
public struct EntitlementsAPI: Sendable {
    let t: Transport

    /// `has(.feature("export"))`: the boolean answer.
    public func has(_ q: EntitlementQuery) async throws -> Bool { try await hasDetailed(q).has }

    /// Same as `has` with the reason when the answer is no.
    public func hasDetailed(_ q: EntitlementQuery) async throws -> (has: Bool, reason: String?) {
        let j = try await t.json(Request("GET", "/api/v1/entitlements/has", query: q.items))
        return (j["has"]?.boolValue ?? false, j["reason"]?.stringValue)
    }

    /// Effective plan and grants of the caller (or of `subject`, e.g. `org:<id>`).
    public func get(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/entitlements", query: query(["subject": subject])))
    }

    /// Usage of every metered feature.
    public func usage(subject: String? = nil, scopeKey: String? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/usage", query: query(["subject": subject, "scope_key": scopeKey])))["items"]?.arrayValue ?? []
    }

    /// Would these units be granted right now? No side effects.
    public func checkQuota(_ r: QuotaRequest) async throws -> Decision {
        try await t.decode(Request("POST", "/api/v1/quotas/check", body: r.body))
    }

    /// Records usage at once. A replayed key returns the first result with `replayed: true`. Throws a
    /// `.quotaExceeded` error past a hard limit.
    public func consume(_ r: QuotaRequest) async throws -> Consumption {
        try await t.decode(Request("POST", "/api/v1/usage/consume", body: try need(r).body))
    }

    /// Holds units until `commitReservation` or `releaseReservation` (or `ttlSeconds`).
    public func reserve(_ r: QuotaRequest) async throws -> Reservation {
        try await t.decode(Request("POST", "/api/v1/usage/reservations", body: try need(r).body))
    }

    public func commitReservation(_ id: String, units: Int64? = nil) async throws -> Reservation {
        try await t.decode(Request("POST", "/api/v1/usage/reservations/\(seg(id))/commit", body: wire(["units": units])))
    }

    public func releaseReservation(_ id: String) async throws -> Reservation {
        try await t.decode(Request("POST", "/api/v1/usage/reservations/\(seg(id))/release", body: .object([:])))
    }

    /// Reserve, run `body`, then commit; release the hold when `body` throws.
    public func withReservation<R: Sendable>(_ r: QuotaRequest, _ body: @Sendable (Reservation) async throws -> R) async throws -> R {
        let res = try await reserve(r)
        do {
            let out = try await body(res)
            _ = try await commitReservation(res.id)
            return out
        } catch {
            _ = try? await releaseReservation(res.id)
            throw error
        }
    }

    /// Concurrency lease (e.g. "at most 5 running exports").
    public func acquireLease(_ r: QuotaRequest) async throws -> Lease {
        try await t.decode(Request("POST", "/api/v1/usage/leases", body: try need(r).body))
    }

    public func renewLease(_ id: String, ttlSeconds: Int? = nil) async throws -> Lease {
        try await t.decode(Request("POST", "/api/v1/usage/leases/\(seg(id))/renew", body: wire(["ttl_secs": ttlSeconds])))
    }

    public func releaseLease(_ id: String) async throws {
        try await t.perform(Request("DELETE", "/api/v1/usage/leases/\(seg(id))"))
    }

    /// The subscription of `subject` (default: the caller's). No subscription is a `.notFound` error.
    public func subscription(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/entitlements/subscription", query: query(["subject": subject])))
    }

    /// Usage alerts (70 / 90 / 100 %) of `subject` (default: the caller's), newest first.
    public func usageAlerts(subject: String? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/usage/alerts", query: query(["subject": subject])))["items"]?.arrayValue ?? []
    }

    private func need(_ r: QuotaRequest) throws -> QuotaRequest {
        guard r.idempotencyKey != nil else {
            throw ForgeBaseError.invalidRequest("consume, reserve and lease need an idempotencyKey: a replay returns the first result")
        }
        return r
    }
}
