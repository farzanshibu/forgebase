import Foundation

public enum GraphDirection: String, Sendable { case out, `in`, both }

/// Equality conditions on traversed nodes.
public struct GraphNodeFilter: Sendable {
    public var label: String?
    /// Up to 8 equality conditions; values are strings, numbers or booleans.
    public var properties: JSONObject
    public init(label: String? = nil, properties: JSONObject = [:]) {
        self.label = label
        self.properties = properties
    }
}

/// Property graph read helpers: `fb.graph`. Writing nodes and edges is left to the JavaScript and Rust SDKs.
public struct GraphAPI: Sendable {
    let t: Transport
    private static let base = "/api/v1/graph"

    public func node(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/nodes/\(seg(id))"))
    }

    /// One page of nodes; continue with `nextAfter`.
    public func nodes(label: String? = nil, after: String? = nil, limit: Int? = nil) async throws -> (items: [JSON], nextAfter: String?) {
        let j = try await t.json(Request("GET", "\(Self.base)/nodes", query: query(["label": label, "after": after, "limit": limit])))
        return (j["items"]?.arrayValue ?? [], j["next_after"]?.stringValue)
    }

    /// Neighbours of a node.
    public func neighbors(
        of id: String, direction: GraphDirection? = nil, edgeType: String? = nil, after: String? = nil, limit: Int? = nil
    ) async throws -> (items: [JSON], nextAfter: String?) {
        let j = try await t.json(
            Request(
                "GET", "\(Self.base)/nodes/\(seg(id))/neighbors",
                query: query(["direction": direction?.rawValue, "edge_type": edgeType, "after": after, "limit": limit])))
        return (j["items"]?.arrayValue ?? [], j["next_after"]?.stringValue)
    }

    public func edge(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/edges/\(seg(id))"))
    }

    /// Breadth-first traversal from a start node.
    public func traverse(
        start: String, direction: GraphDirection? = nil, edgeTypes: [String]? = nil, maxDepth: Int? = nil, limit: Int? = nil,
        filter: GraphNodeFilter? = nil, includeStart: Bool? = nil, includeEdges: Bool? = nil
    ) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "\(Self.base)/traverse",
                body: wire([
                    "start": start, "direction": direction?.rawValue, "edge_types": edgeTypes, "max_depth": maxDepth, "limit": limit,
                    "filter": filter.map { wire(["label": $0.label, "properties": $0.properties.isEmpty ? nil : JSON.object($0.properties)]) },
                    "include_start": includeStart, "include_edges": includeEdges,
                ])))
    }

    public func shortestPath(from: String, to: String, direction: GraphDirection? = nil, edgeTypes: [String]? = nil, maxDepth: Int? = nil) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "\(Self.base)/shortest-path",
                body: wire(["from": from, "to": to, "direction": direction?.rawValue, "edge_types": edgeTypes, "max_depth": maxDepth])))
    }

    public func stats() async throws -> JSON { try await t.json(Request("GET", "\(Self.base)/stats")) }

    /// Read-only openCypher over the tenant's derived graph (needs `engine:graph_cypher` and PostgreSQL with AGE).
    /// One query with a final `RETURN`; parameters are `$name`. The first query of a tenant builds the graph and fails
    /// with a `.conflict` error `graph_materialising` (`details`/`body` name the job): retry after it finishes.
    public func cypher(_ query: String, params: JSONObject? = nil, limit: Int? = nil, timeoutMs: Int? = nil) async throws -> JSON {
        try await t.json(
            Request("POST", "\(Self.base)/cypher", body: wire(["query": query, "params": params.map(JSON.object), "limit": limit, "timeout_ms": timeoutMs])))
    }
}
