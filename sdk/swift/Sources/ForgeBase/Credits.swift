import Foundation

/// Prepaid credit balances and ledger: `fb.credits` (reads; posting and rates are administrator operations).
public struct CreditsAPI: Sendable {
    let t: Transport

    /// Balance of `subject` (default: the caller's).
    public func balance(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/credits/balance", query: query(["subject": subject])))
    }

    /// Credit lots; `all` includes exhausted and expired ones.
    public func lots(subject: String? = nil, all: Bool = false) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/credits/lots", query: query(["subject": subject, "all": all ? true : nil])))["items"]?.arrayValue ?? []
    }

    /// One page of the ledger, newest first; continue with the page's `next` value as `after`.
    public func ledger(subject: String? = nil, after: Int64? = nil, limit: Int? = nil, kind: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/credits/ledger", query: query(["subject": subject, "after": after, "limit": limit, "kind": kind])))
    }

    public func entry(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/credits/ledger/\(seg(id))"))
    }

    /// Consistency check of the ledger against the balance.
    public func audit(subject: String? = nil) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/credits/audit", query: query(["subject": subject])))
    }
}
