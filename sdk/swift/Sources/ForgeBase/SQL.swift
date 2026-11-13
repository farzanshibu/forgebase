import Foundation

/// Result of one statement.
public struct SQLResult: Sendable, Equatable {
    /// `read`, `write`, `schema` or `maintenance`.
    public let kind: String
    public let columns: [String]
    /// One array per row, in `columns` order. At most 10 000 rows are returned (`truncated`).
    public let rows: [[JSON]]
    public let rowsAffected: Int
    public let lastInsertId: Int64?
    public let truncated: Bool
    public let durationMs: Double
    /// Secret values in `rows` read `"********"`.
    public let secretsMasked: Bool

    init(_ j: JSON) {
        kind = j["kind"]?.stringValue ?? ""
        columns = j["columns"]?.arrayValue?.compactMap(\.stringValue) ?? []
        rows = j["rows"]?.arrayValue?.map { $0.arrayValue ?? [] } ?? []
        rowsAffected = j["rows_affected"]?.intValue ?? 0
        lastInsertId = j["last_insert_id"]?.int64Value
        truncated = j["truncated"]?.boolValue ?? false
        durationMs = j["duration_ms"]?.doubleValue ?? 0
        secretsMasked = j["secrets_masked"]?.boolValue ?? false
    }

    /// One object per row, keyed by column name (the last of two equal names wins).
    public var objects: [JSONObject] {
        rows.map { row in
            var o: JSONObject = [:]
            for (i, c) in columns.enumerated() where i < row.count { o[c] = row[i] }
            return o
        }
    }
}

/// SQL editor: `fb.sql`. Needs the SQL permission; the server applies the caller's rules.
public struct SQLAPI: Sendable {
    let t: Transport
    private static let base = "/api/v1/sql"

    /// Runs one statement; `params` fill `?` / `$n` placeholders.
    public func execute(_ sql: String, params: [JSON] = []) async throws -> SQLResult {
        SQLResult(try await t.json(Request("POST", Self.base, body: ["sql": .string(sql), "params": .array(params)])))
    }

    /// Runs a read statement and returns one object per row.
    public func query(_ sql: String, params: [JSON] = []) async throws -> [JSONObject] {
        try await execute(sql, params: params).objects
    }

    /// Runs a read statement and decodes each row into `T` (keys = column names).
    public func query<T: Decodable>(_ sql: String, params: [JSON] = [], as type: T.Type) async throws -> [T] {
        try await query(sql, params: params).map { try JSON.object($0).decode(T.self) }
    }

    public func explain(_ sql: String, params: [JSON] = []) async throws -> JSON {
        try await t.json(Request("POST", "\(Self.base)/explain", body: ["sql": .string(sql), "params": .array(params)]))
    }

    public func tables() async throws -> [JSON] {
        try await t.json(Request("GET", "\(Self.base)/tables"))["items"]?.arrayValue ?? []
    }

    public func table(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/tables/\(seg(name))"))
    }
}
