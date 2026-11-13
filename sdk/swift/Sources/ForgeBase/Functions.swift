import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// What `invokeRaw` sends back.
public struct FunctionResponse: Sendable {
    public let statusCode: Int
    public let headers: [String: String]
    public let data: Data
    public var text: String { String(decoding: data, as: UTF8.self) }
    public var contentType: String? { headers["content-type"] }

    /// Decodes a JSON body.
    public func decode<T: Decodable>(_ type: T.Type = T.self) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: data) } catch {
            throw ForgeBaseError.invalidResponse("function response was not the expected JSON: \(error)", status: statusCode)
        }
    }
}

public struct InvokeOptions: Sendable {
    /// HTTP method (default `POST`); the function sees it in its input.
    public var method: String
    /// Sub-path after `/invoke/` (`"users/42"`); the function sees it as its request path.
    public var path: String?
    /// Query string parameters passed to the function.
    public var query: [String: String]
    /// Administrators: run this version instead of the active one (a test run).
    public var version: Int?
    public var headers: [String: String]
    /// Retry the call on 429/503 and network errors. Default off: a function may have side effects.
    public var retry: Bool

    public init(method: String = "POST", path: String? = nil, query: [String: String] = [:], version: Int? = nil, headers: [String: String] = [:], retry: Bool = false) {
        self.method = method
        self.path = path
        self.query = query
        self.version = version
        self.headers = headers
        self.retry = retry
    }
}

/// Server functions: `fb.functions`. Managing (deploying, versions, secrets) is for the JavaScript SDK, the Rust SDK and the CLI.
public struct FunctionsAPI: Sendable {
    let t: Transport

    /// Every function, by name.
    public func list() async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/functions"))["items"]?.arrayValue ?? []
    }

    public func get(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "/api/v1/functions/\(seg(name))"))
    }

    /// Recent executions, newest first.
    public func logs(_ name: String, limit: Int? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/functions/\(seg(name))/logs", query: query(["limit": limit])))["items"]?.arrayValue ?? []
    }

    /// Invokes an HTTP function with a JSON input and returns what it sends back: parsed JSON for a JSON response,
    /// a string for any other text, `.null` for 204. A function that answers with a status of 400 or more throws the
    /// matching error (`.notFound`, `.validation`, ...).
    public func invoke(_ name: String, input: JSON? = nil, options: InvokeOptions = .init()) async throws -> JSON {
        let res = try await invokeRaw(name, body: input.map { try? $0.data() } ?? nil, contentType: "application/json", options: options)
        if res.statusCode == 204 || res.data.isEmpty { return .null }
        if let ct = res.contentType, ct.lowercased().contains("json") {
            do { return try JSON(data: res.data) } catch { throw ForgeBaseError.invalidResponse("function response was not valid JSON", status: res.statusCode) }
        }
        return .string(res.text)
    }

    /// Invokes with an `Encodable` input and decodes the JSON answer.
    public func invoke<I: Encodable, O: Decodable>(_ name: String, input: I, as type: O.Type = O.self, options: InvokeOptions = .init()) async throws -> O {
        let body = try JSONEncoder().encode(input)
        let res = try await invokeRaw(name, body: body, contentType: "application/json", options: options)
        return try res.decode(O.self)
    }

    /// Invokes and returns the 2xx response as is (headers, binary bodies).
    public func invokeRaw(_ name: String, body: Data? = nil, contentType: String? = nil, options o: InvokeOptions = .init()) async throws -> FunctionResponse {
        let sub = (o.path ?? "").drop(while: { $0 == "/" })
        let suffix = sub.isEmpty ? "" : "/" + keyPath(String(sub))
        var items = o.query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
        if let v = o.version { items.append(URLQueryItem(name: "version", value: String(v))) }
        var r = Request(o.method, "/api/v1/functions/\(seg(name))/invoke\(suffix)", query: items)
        if let body, !body.isEmpty { r.body = .data(body, contentType: contentType) }
        r.headers = o.headers
        r.accept = "*/*"
        if !o.retry { r.retry = .disabled }
        let res = try await t.send(r)
        var headers: [String: String] = [:]
        for (k, v) in res.http.allHeaderFields { headers[String(describing: k).lowercased()] = String(describing: v) }
        return FunctionResponse(statusCode: res.http.statusCode, headers: headers, data: res.data)
    }
}
