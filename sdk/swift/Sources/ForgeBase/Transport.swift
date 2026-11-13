import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// A small lock-protected box for state shared across tasks.
final class Locked<Value>: @unchecked Sendable {
    private var value: Value
    private let lock = NSLock()
    init(_ value: Value) { self.value = value }
    func withLock<R>(_ body: (inout Value) throws -> R) rethrows -> R {
        lock.lock()
        defer { lock.unlock() }
        return try body(&value)
    }
}

/// What the transport needs from the auth layer.
protocol Credentials: Sendable {
    /// Headers for one request (may refresh the access token first).
    func headers(for method: String, transport: Transport) async -> [String: String]
    /// Called after a 401 with the headers that were sent: return `true` when a retry can succeed.
    func onUnauthorized(sent: [String: String], transport: Transport) async -> Bool
}

enum RequestBody: Sendable {
    case none
    case json(JSON)
    case data(Data, contentType: String?)
    case file(URL, contentType: String?)
}

struct Request: Sendable {
    var method = "GET"
    var path: String
    var query: [URLQueryItem] = []
    var body: RequestBody = .none
    var headers: [String: String] = [:]
    /// Skip credentials (sign-in, sign-up, signed URLs).
    var anonymous = false
    /// Override the retry policy; `nil` uses the client's, `.disabled` turns retrying off.
    var retry: RetryPolicy?
    var consistency: Consistency?
    var accept = "application/json"
    /// Absolute URL instead of `path` (signed URLs).
    var absoluteURL: URL?

    init(_ method: String = "GET", _ path: String, query: [URLQueryItem] = [], body: JSON? = nil) {
        self.method = method
        self.path = path
        self.query = query
        if let body { self.body = .json(body) }
    }
}

private let idempotentMethods: Set<String> = ["GET", "HEAD", "OPTIONS", "PUT", "DELETE"]

/// HTTP transport: credentials, consistency headers, retries with backoff, typed errors.
final class Transport: Sendable {
    let baseURL: String
    let session: URLSession
    let retry: RetryPolicy
    let defaultHeaders: [String: String]
    let consistency: Consistency
    let timeout: TimeInterval
    let commitTokens: CommitTokens
    let credentials: (any Credentials)?

    static let userAgent = "forgebase-swift/\(ForgeBaseVersion.current)"

    init(
        baseURL: URL, session: URLSession, retry: RetryPolicy, headers: [String: String],
        consistency: Consistency, timeout: TimeInterval, commitTokens: CommitTokens = CommitTokens(),
        credentials: (any Credentials)?
    ) {
        var s = baseURL.absoluteString
        while s.hasSuffix("/") { s.removeLast() }
        self.baseURL = s
        self.session = session
        self.retry = retry
        self.defaultHeaders = headers
        self.consistency = consistency
        self.timeout = timeout
        self.commitTokens = commitTokens
        self.credentials = credentials
    }

    // MARK: URLs

    func url(_ path: String, query: [URLQueryItem] = []) throws -> URL {
        var s = baseURL + path
        let items = query.filter { $0.value != nil }
        if !items.isEmpty {
            s += "?" + items.map { "\(seg($0.name))=\(seg($0.value ?? ""))" }.joined(separator: "&")
        }
        guard let u = URL(string: s) else { throw ForgeBaseError.invalidRequest("invalid URL: \(s)") }
        return u
    }

    /// The absolute URL of a path or of an already absolute URL string.
    func resolve(_ pathOrURL: String) -> String {
        pathOrURL.lowercased().hasPrefix("http://") || pathOrURL.lowercased().hasPrefix("https://") ? pathOrURL : baseURL + pathOrURL
    }

    // MARK: typed helpers

    /// Sends and returns the raw body bytes.
    func data(_ r: Request) async throws -> Data {
        try await send(r).data
    }

    /// Sends and parses the JSON body (`.null` for an empty one).
    func json(_ r: Request) async throws -> JSON {
        let d = try await send(r).data
        if d.isEmpty { return .null }
        do { return try JSON(data: d) } catch {
            throw ForgeBaseError.invalidResponse("response was not valid JSON")
        }
    }

    /// Sends and decodes the JSON body.
    func decode<T: Decodable>(_ r: Request, as type: T.Type = T.self) async throws -> T {
        let d = try await send(r).data
        do { return try JSONDecoder().decode(T.self, from: d) } catch {
            throw ForgeBaseError.invalidResponse("could not decode \(T.self): \(error)")
        }
    }

    /// Sends, ignoring the body.
    func perform(_ r: Request) async throws {
        _ = try await send(r)
    }

    func text(_ r: Request) async throws -> String {
        String(decoding: try await send(r).data, as: UTF8.self)
    }

    // MARK: core

    struct Response: Sendable {
        let data: Data
        let http: HTTPURLResponse
    }

    func send(_ r: Request) async throws -> Response {
        let (data, http) = try await run(
            r,
            exec: { [session] req, body in
                switch body {
                case .file(let url, _): return try await session.upload(for: req, fromFile: url)
                default: return try await session.data(for: req)
                }
            },
            errorBody: { $0 })
        return Response(data: data, http: http)
    }

    /// Opens a streaming response (server-sent events). Non-2xx answers throw the typed error.
    func openStream(_ r: Request) async throws -> (URLSession.AsyncBytes, HTTPURLResponse) {
        var r = r
        r.retry = r.retry ?? .disabled
        r.accept = "text/event-stream"
        return try await run(
            r,
            exec: { [session] req, _ in try await session.bytes(for: req) },
            errorBody: { bytes in
                var out = Data()
                do {
                    for try await b in bytes {
                        out.append(b)
                        if out.count >= 65_536 { break }
                    }
                } catch {}
                return out
            })
    }

    /// Downloads straight to a file (large objects). Returns the response headers.
    func download(_ r: Request, to destination: URL) async throws -> HTTPURLResponse {
        let (tmp, http) = try await run(
            r,
            exec: { [session] req, _ in try await session.download(for: req) },
            errorBody: { (url: URL) in (try? Data(contentsOf: url)) ?? Data() })
        let fm = FileManager.default
        if fm.fileExists(atPath: destination.path) { try fm.removeItem(at: destination) }
        try fm.moveItem(at: tmp, to: destination)
        return http
    }

    private func prepare(_ r: Request) async throws -> URLRequest {
        let method = r.method.uppercased()
        let target = try r.absoluteURL ?? url(r.path, query: r.query)
        var req = URLRequest(url: target)
        req.httpMethod = method
        req.timeoutInterval = timeout
        var headers: [String: String] = ["Accept": r.accept, "User-Agent": Transport.userAgent]
        for (k, v) in defaultHeaders { headers[k] = v }
        switch r.body {
        case .none: break
        case .json(let j):
            headers["Content-Type"] = "application/json"
            req.httpBody = try j.data()
        case .data(let d, let ct):
            if let ct { headers["Content-Type"] = ct }
            req.httpBody = d
        case .file(_, let ct):
            if let ct { headers["Content-Type"] = ct }
        }
        if !r.anonymous, let credentials {
            for (k, v) in await credentials.headers(for: method, transport: self) { headers[k] = v }
        }
        if let ra = await commitTokens.header() { headers[readAfterHeader] = ra }
        if (r.consistency ?? consistency) == .strong { headers[consistencyHeader] = "strong" }
        for (k, v) in r.headers { headers[k] = v }
        for (k, v) in headers { req.setValue(v, forHTTPHeaderField: k) }
        return req
    }

    private func run<T>(
        _ r: Request,
        exec: (URLRequest, RequestBody) async throws -> (T, URLResponse),
        errorBody: (T) async -> Data
    ) async throws -> (T, HTTPURLResponse) {
        let method = r.method.uppercased()
        let cfg = r.retry ?? retry
        var attempt = 0
        var refreshed = false
        while true {
            try Task.checkCancellation()
            let req = try await prepare(r)
            let sent = req.allHTTPHeaderFields ?? [:]
            let result: T
            let response: URLResponse
            do {
                (result, response) = try await exec(req, r.body)
            } catch {
                if error is CancellationError || (error as? URLError)?.code == .cancelled { throw CancellationError() }
                if attempt < cfg.maxRetries && idempotentMethods.contains(method) {
                    attempt += 1
                    try await cfg.sleep(cfg.backoff(attempt: attempt, retryAfter: nil))
                    continue
                }
                throw ForgeBaseError.network(error.localizedDescription, underlying: error)
            }
            guard let http = response as? HTTPURLResponse else {
                throw ForgeBaseError.invalidResponse("not an HTTP response", status: 0)
            }
            // A failed request may still have committed (a batch, a partial import).
            await commitTokens.note(http.value(forHTTPHeaderField: commitHeader))
            if (200..<300).contains(http.statusCode) { return (result, http) }

            if http.statusCode == 401, !r.anonymous, !refreshed, let credentials,
                await credentials.onUnauthorized(sent: sent, transport: self)
            {
                refreshed = true
                continue
            }

            let retriable = cfg.statuses.contains(http.statusCode)
                || ((http.statusCode == 502 || http.statusCode == 504) && idempotentMethods.contains(method))
            if retriable && attempt < cfg.maxRetries {
                if http.statusCode == 429 {
                    // An exhausted quota is final: waiting will not help.
                    let body = await errorBody(result)
                    let err = ForgeBaseError.from(status: 429, body: try? JSON(data: body), headers: http)
                    if err.kind == .quotaExceeded { throw err }
                }
                let ra = parseRetryAfter(http.value(forHTTPHeaderField: "Retry-After"))
                if ra == nil || ra! <= cfg.maxRetryAfter {
                    attempt += 1
                    try await cfg.sleep(cfg.backoff(attempt: attempt, retryAfter: ra))
                    continue
                }
            }
            let body = await errorBody(result)
            throw ForgeBaseError.from(status: http.statusCode, body: try? JSON(data: body), headers: http)
        }
    }
}

// MARK: - server-sent events

/// One event of a `text/event-stream`.
struct SSEEvent: Sendable, Equatable {
    var event: String
    var data: String
    var id: String?
}

/// Incremental server-sent-events parser: feed bytes, get events. Tolerates events split across chunks,
/// CRLF line ends and comment lines.
struct SSEParser {
    private var line: [UInt8] = []
    private var event = "message"
    private var data: [String] = []
    private var id: String?

    mutating func feed(_ byte: UInt8) -> SSEEvent? {
        if byte != 0x0A {
            line.append(byte)
            return nil
        }
        if line.last == 0x0D { line.removeLast() }
        defer { line.removeAll(keepingCapacity: true) }
        if line.isEmpty {
            defer {
                event = "message"
                data = []
                id = nil
            }
            if data.isEmpty && event == "message" { return nil }
            return SSEEvent(event: event, data: data.joined(separator: "\n"), id: id)
        }
        if line.first == 0x3A { return nil }  // comment
        let text = String(decoding: line, as: UTF8.self)
        let field: String
        var value: String
        if let c = text.firstIndex(of: ":") {
            field = String(text[..<c])
            value = String(text[text.index(after: c)...])
            if value.hasPrefix(" ") { value.removeFirst() }
        } else {
            field = text
            value = ""
        }
        switch field {
        case "event": event = value
        case "data": data.append(value)
        case "id": id = value
        default: break
        }
        return nil
    }

    /// Events of a byte sequence.
    static func events<S: AsyncSequence>(_ bytes: S) -> AsyncThrowingStream<SSEEvent, Error> where S.Element == UInt8, S: Sendable {
        AsyncThrowingStream { continuation in
            let task = Task {
                var parser = SSEParser()
                do {
                    for try await b in bytes {
                        if let e = parser.feed(b) { continuation.yield(e) }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

enum ForgeBaseVersion {
    static let current = "0.1.0"
}
