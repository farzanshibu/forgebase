import Foundation

/// One item to insert or replace.
public struct VectorUpsertItem: Sendable {
    /// 1 to 64 of `[A-Za-z0-9_-]`; generated when absent. An existing item with this id is replaced completely.
    public var id: String?
    /// Vector per field name. Components must be finite and match the field's dimension.
    public var vectors: [String: [Double]]
    /// Filterable JSON object (up to 64 KiB).
    public var metadata: JSONObject?
    /// Text for the keyword side of hybrid search.
    public var text: String?
    public init(id: String? = nil, vectors: [String: [Double]], metadata: JSONObject? = nil, text: String? = nil) {
        self.id = id
        self.vectors = vectors
        self.metadata = metadata
        self.text = text
    }
}

public struct VectorQuery: Sendable {
    /// Vector field; may be left out when the collection has exactly one.
    public var field: String?
    /// Number of results, 1 to 1000 (default 10).
    public var k: Int?
    /// Metadata filter, e.g. `category = 'news' && year >= {:min}`.
    public var filter: String?
    /// Values of the `{:name}` placeholders of `filter`.
    public var params: JSONObject?
    public var filterMode: String?
    public var includeVectors: Bool?
    /// Return each hit's metadata (server default `true`).
    public var includeMetadata: Bool?
    public var includeText: Bool?
    public init(
        field: String? = nil, k: Int? = nil, filter: String? = nil, params: JSONObject? = nil, filterMode: String? = nil,
        includeVectors: Bool? = nil, includeMetadata: Bool? = nil, includeText: Bool? = nil
    ) {
        self.field = field
        self.k = k
        self.filter = filter
        self.params = params
        self.filterMode = filterMode
        self.includeVectors = includeVectors
        self.includeMetadata = includeMetadata
        self.includeText = includeText
    }
}

/// One search hit.
public struct VectorHit: Sendable, Equatable {
    public let id: String
    /// Distance (smaller is closer) for `search`, fused score (larger is better) for `hybrid`.
    public let score: Double
    public let metadata: JSON?
    public let text: String?
    /// The whole hit object as the server sent it.
    public let raw: JSON

    init(_ j: JSON) {
        id = j["id"]?.stringValue ?? ""
        score = j["distance"]?.doubleValue ?? j["score"]?.doubleValue ?? 0
        metadata = j["metadata"].flatMap { $0.isNull ? nil : $0 }
        text = j["text"]?.stringValue
        raw = j
    }
}

/// Vector collections and similarity search: `fb.vector`. Creating collections and fields is left to the JavaScript
/// and Rust SDKs (administration); the mobile surface is search and item access.
public struct VectorAPI: Sendable {
    let t: Transport

    public func collection(_ name: String) -> VectorCollectionAPI { VectorCollectionAPI(t: t, name: name) }

    public func list() async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/vector/collections"))["items"]?.arrayValue ?? []
    }
}

public struct VectorCollectionAPI: Sendable {
    let t: Transport
    public let name: String
    private var base: String { "/api/v1/vector/collections/\(seg(name))" }

    public func get() async throws -> JSON { try await t.json(Request("GET", base)) }

    /// Inserts or replaces items.
    public func upsert(_ items: [VectorUpsertItem]) async throws -> JSON {
        let list: [JSON] = items.map { wire(["id": $0.id, "vectors": $0.vectors, "metadata": $0.metadata.map(JSON.object), "text": $0.text]) }
        return try await t.json(Request("POST", "\(base)/upsert", body: ["items": .array(list)]))
    }

    /// One item with its canonical vectors.
    public func getItem(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "\(base)/items/\(seg(id))"))
    }

    public func deleteItem(_ id: String) async throws {
        try await t.perform(Request("DELETE", "\(base)/items/\(seg(id))"))
    }

    /// Deletes 1 to 1000 items by id; unknown ids are ignored. Returns how many were deleted.
    public func deleteItems(_ ids: [String]) async throws -> Int {
        try await t.json(Request("POST", "\(base)/delete", body: wire(["ids": ids])))["deleted"]?.intValue ?? 0
    }

    /// k-nearest-neighbour search with an optional metadata filter; closest first.
    /// - Parameter exact: brute force instead of the ANN index (ground truth, small sets).
    public func search(vector: [Double], _ q: VectorQuery = .init(), exact: Bool? = nil) async throws -> [VectorHit] {
        let j = try await t.json(
            Request(
                "POST", "\(base)/search",
                body: wire([
                    "field": q.field, "vector": vector, "k": q.k, "filter": q.filter, "params": q.params.map(JSON.object),
                    "filter_mode": q.filterMode, "exact": exact, "include_vectors": q.includeVectors,
                    "include_metadata": q.includeMetadata, "include_text": q.includeText,
                ])))
        return (j["hits"]?.arrayValue ?? []).map(VectorHit.init)
    }

    /// Hybrid search: keyword (BM25) and vector rankings fused with Reciprocal Rank Fusion; best first.
    public func hybrid(vector: [Double]? = nil, text: String? = nil, _ q: VectorQuery = .init(), candidates: Int? = nil, rrfK: Int? = nil) async throws -> [VectorHit] {
        let j = try await t.json(
            Request(
                "POST", "\(base)/hybrid",
                body: wire([
                    "field": q.field, "vector": vector, "text": text, "k": q.k, "candidates": candidates, "rrf_k": rrfK,
                    "filter": q.filter, "params": q.params.map(JSON.object), "filter_mode": q.filterMode,
                    "include_vectors": q.includeVectors, "include_metadata": q.includeMetadata, "include_text": q.includeText,
                ])))
        return (j["hits"]?.arrayValue ?? []).map(VectorHit.init)
    }

    public func stats() async throws -> JSON { try await t.json(Request("GET", "\(base)/stats")) }
}
