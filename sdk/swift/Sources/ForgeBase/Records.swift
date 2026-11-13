import Foundation

public struct ListOptions: Sendable {
    /// Filter-language expression, e.g. `price > {:min} && status = "active"`.
    public var filter: String?
    /// Typed values for `{:name}` placeholders in `filter` (sent with the JSON query endpoint).
    public var params: JSONObject?
    /// `["-created", "name"]`.
    public var sort: [String]
    /// Relations to expand inline.
    public var expand: [String]
    /// Fields to return.
    public var fields: [String]
    /// Full-text search term.
    public var search: String?
    /// 1-based page (offset pagination).
    public var page: Int?
    /// Page size (server maximum 500).
    public var perPage: Int?
    /// Cursor from a previous page's `nextCursor`.
    public var cursor: String?
    /// Skip the total count (cheaper on large collections).
    public var skipTotal: Bool?
    /// `.strong` reads from the primary even when replicas serve reads.
    public var consistency: Consistency?

    public init(
        filter: String? = nil, params: JSONObject? = nil, sort: [String] = [], expand: [String] = [],
        fields: [String] = [], search: String? = nil, page: Int? = nil, perPage: Int? = nil,
        cursor: String? = nil, skipTotal: Bool? = nil, consistency: Consistency? = nil
    ) {
        self.filter = filter
        self.params = params
        self.sort = sort
        self.expand = expand
        self.fields = fields
        self.search = search
        self.page = page
        self.perPage = perPage
        self.cursor = cursor
        self.skipTotal = skipTotal
        self.consistency = consistency
    }
}

/// One page of records.
public struct ListResult<T: Sendable>: Sendable {
    public var items: [T]
    public var page: Int?
    public var perPage: Int
    public var totalItems: Int?
    public var totalPages: Int?
    /// Pass as `cursor` for the next page; `nil` on the last page.
    public var nextCursor: String?
}

private struct WireList<T: Decodable>: Decodable {
    let items: [T]
    let page: Int?
    let perPage: Int
    let totalItems: Int?
    let totalPages: Int?
    let nextCursor: String?

    enum CodingKeys: String, CodingKey {
        case items, page, perPage = "per_page", totalItems = "total_items", totalPages = "total_pages", nextCursor = "next_cursor"
    }
}

public enum TransferFormat: String, Sendable {
    case json, ndjson, csv
}

/// Records of one collection: `fb.collection("posts")`.
///
/// `T` is the record type: a `Codable` struct whose keys match the collection's fields, or `JSONObject`.
public struct RecordsAPI<T: Codable & Sendable>: Sendable {
    let t: Transport
    public let name: String
    private var base: String { "/api/v1/collections/\(seg(name))" }

    init(transport: Transport, name: String) {
        t = transport
        self.name = name
    }

    /// One page. With `params` the typed JSON query endpoint is used, otherwise a plain GET.
    public func list(_ o: ListOptions = .init()) async throws -> ListResult<T> {
        let sort = o.sort.isEmpty ? nil : o.sort.joined(separator: ",")
        let result: WireList<T>
        if let params = o.params {
            var r = Request(
                "POST", "\(base)/records/query",
                body: wire(
                    ["filter": o.filter, "sort": sort, "search": o.search, "page": o.page, "per_page": o.perPage,
                     "cursor": o.cursor, "skip_total": o.skipTotal, "params": JSON.object(params),
                     "expand": o.expand.isEmpty ? nil : o.expand, "fields": o.fields.isEmpty ? nil : o.fields]))
            r.consistency = o.consistency
            result = try await t.decode(r)
        } else {
            var r = Request(
                "GET", "\(base)/records",
                query: query([
                    "filter": o.filter, "sort": sort, "search": o.search, "page": o.page, "per_page": o.perPage,
                    "cursor": o.cursor, "skip_total": o.skipTotal,
                    "expand": o.expand.isEmpty ? nil : o.expand.joined(separator: ","),
                    "fields": o.fields.isEmpty ? nil : o.fields.joined(separator: ","),
                ]))
            r.consistency = o.consistency
            result = try await t.decode(r)
        }
        return ListResult(
            items: result.items, page: result.page, perPage: result.perPage, totalItems: result.totalItems,
            totalPages: result.totalPages, nextCursor: result.nextCursor)
    }

    /// Every page, lazily: `for try await page in posts.pages(.init(perPage: 100))`. Follows `nextCursor`, else page numbers.
    public func pages(_ o: ListOptions = .init()) -> AsyncThrowingStream<ListResult<T>, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var cursor = o.cursor
                    var page = o.page ?? 1
                    while true {
                        var opts = o
                        opts.cursor = cursor
                        opts.page = cursor == nil ? page : nil
                        let res = try await list(opts)
                        continuation.yield(res)
                        if let next = res.nextCursor {
                            cursor = next
                        } else if cursor == nil, let p = res.page, let total = res.totalPages, p < total {
                            page = p + 1
                        } else {
                            break
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Every record, lazily: `for try await post in posts.iterate(.init(filter: "published = true"))`.
    public func iterate(_ o: ListOptions = .init()) -> AsyncThrowingStream<T, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    for try await p in pages(o) { for item in p.items { continuation.yield(item) } }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Collects every record (mind the memory use on big collections).
    public func all(_ o: ListOptions = .init()) async throws -> [T] {
        var out: [T] = []
        for try await r in iterate(o) { out.append(r) }
        return out
    }

    /// First record matching `filter`, or `nil`.
    public func first(filter: String, params: JSONObject? = nil, sort: [String] = []) async throws -> T? {
        try await list(ListOptions(filter: filter, params: params, sort: sort, perPage: 1, skipTotal: true)).items.first
    }

    public func get(_ id: String, expand: [String] = [], fields: [String] = [], consistency: Consistency? = nil) async throws -> T {
        var r = Request(
            "GET", "\(base)/records/\(seg(id))",
            query: query([
                "expand": expand.isEmpty ? nil : expand.joined(separator: ","),
                "fields": fields.isEmpty ? nil : fields.joined(separator: ","),
            ]))
        r.consistency = consistency
        return try await t.decode(r)
    }

    public func create<B: Encodable & Sendable>(_ data: B) async throws -> T {
        try await t.decode(Request("POST", "\(base)/records", body: try JSON(encoding: data)))
    }

    /// Partial update (PATCH). With `version` (the record's `_version`) a stale write throws a `.conflict` error.
    public func update<B: Encodable & Sendable>(_ id: String, _ patch: B, version: Int? = nil) async throws -> T {
        var r = Request("PATCH", "\(base)/records/\(seg(id))", body: try JSON(encoding: patch))
        r.headers = Self.ifMatch(version)
        return try await t.decode(r)
    }

    /// Full replace (PUT).
    public func replace<B: Encodable & Sendable>(_ id: String, _ data: B, version: Int? = nil) async throws -> T {
        var r = Request("PUT", "\(base)/records/\(seg(id))", body: try JSON(encoding: data))
        r.headers = Self.ifMatch(version)
        return try await t.decode(r)
    }

    public func delete(_ id: String, version: Int? = nil) async throws {
        var r = Request("DELETE", "\(base)/records/\(seg(id))")
        r.headers = Self.ifMatch(version)
        try await t.perform(r)
    }

    public func count(filter: String? = nil) async throws -> Int {
        try await t.json(Request("GET", "\(base)/records/count", query: query(["filter": filter])))["count"]?.intValue ?? 0
    }

    /// Downloads the collection as text.
    public func export(format: TransferFormat = .json, filter: String? = nil) async throws -> String {
        try await t.text(Request("GET", "\(base)/export", query: query(["format": format.rawValue, "filter": filter])))
    }

    /// Uploads records as text in the given format.
    public func `import`(format: TransferFormat, body: String) async throws -> JSON {
        var r = Request("POST", "\(base)/import", query: query(["format": format.rawValue]))
        r.body = .data(Data(body.utf8), contentType: "application/octet-stream")
        return try await t.json(r)
    }

    private static func ifMatch(_ v: Int?) -> [String: String] {
        v.map { ["If-Match": "\"\($0)\""] } ?? [:]
    }
}

/// Collection definitions (schemas): `fb.collections`.
public struct CollectionsAPI: Sendable {
    let t: Transport

    public func list() async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/collections"))["items"]?.arrayValue ?? []
    }

    public func get(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/collections/\(seg(name))"))
    }

    public func create(_ definition: JSON) async throws -> JSON {
        try await t.json(Request("POST", "/api/v1/collections", body: definition))
    }

    public func update(_ name: String, _ definition: JSON) async throws -> JSON {
        try await t.json(Request("PUT", "/api/v1/collections/\(seg(name))", body: definition))
    }

    public func delete(_ name: String) async throws {
        try await t.perform(Request("DELETE", "/api/v1/collections/\(seg(name))"))
    }
}
