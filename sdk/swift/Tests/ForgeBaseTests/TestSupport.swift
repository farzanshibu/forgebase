import Foundation
import XCTest
@testable import ForgeBase

// MARK: - URLProtocol mock

struct RecordedRequest: @unchecked Sendable {
    let method: String
    let url: URL
    let headers: [String: String]
    let body: Data

    var path: String { url.path }
    var query: String { url.query ?? "" }
    func header(_ name: String) -> String? { headers.first { $0.key.lowercased() == name.lowercased() }?.value }
    var json: JSON? { try? JSON(data: body) }
}

struct MockResponse: @unchecked Sendable {
    var status = 200
    var headers: [String: String] = ["Content-Type": "application/json"]
    var body = Data()
    /// Delivered one by one (event streams); when set, `body` is ignored.
    var chunks: [Data]?
    /// Keep the connection open after the chunks (until the request is cancelled).
    var hold = false
    var error: Error?

    static func json(_ value: JSON, status: Int = 200, headers: [String: String] = [:]) -> MockResponse {
        var h = ["Content-Type": "application/json"]
        for (k, v) in headers { h[k] = v }
        return MockResponse(status: status, headers: h, body: (try? value.data()) ?? Data())
    }

    static func text(_ s: String, status: Int = 200, contentType: String = "text/plain") -> MockResponse {
        MockResponse(status: status, headers: ["Content-Type": contentType], body: Data(s.utf8))
    }

    static func empty(_ status: Int = 204) -> MockResponse { MockResponse(status: status, headers: [:]) }

    static func apiError(_ status: Int, code: String, message: String = "boom", details: JSON? = nil, headers: [String: String] = [:]) -> MockResponse {
        var err: [String: JSON] = ["code": .string(code), "message": .string(message), "request_id": "req_1"]
        if let details { err["details"] = details }
        return .json(["error": .object(err)], status: status, headers: headers)
    }

    static func sse(_ events: [String], hold: Bool = false) -> MockResponse {
        MockResponse(status: 200, headers: ["Content-Type": "text/event-stream"], chunks: events.map { Data($0.utf8) }, hold: hold)
    }
}

final class MockServer: @unchecked Sendable {
    private let lock = NSLock()
    private var _requests: [RecordedRequest] = []
    private var handler: (RecordedRequest) -> MockResponse = { _ in .empty(404) }

    var requests: [RecordedRequest] {
        lock.lock()
        defer { lock.unlock() }
        return _requests
    }

    func handle(_ h: @escaping (RecordedRequest) -> MockResponse) {
        lock.lock()
        handler = h
        lock.unlock()
    }

    /// Answers requests in order (the last answer repeats).
    func script(_ responses: [MockResponse]) {
        let i = Locked(0)
        handle { _ in
            let n = i.withLock { v -> Int in
                defer { v += 1 }
                return min(v, responses.count - 1)
            }
            return responses[n]
        }
    }

    /// Routes by `"METHOD /path"`.
    func routes(_ table: [String: (RecordedRequest) -> MockResponse]) {
        handle { r in table["\(r.method) \(r.path)"]?(r) ?? .apiError(404, code: "not_found", message: "no route \(r.method) \(r.path)") }
    }

    fileprivate func record(_ r: RecordedRequest) -> MockResponse {
        lock.lock()
        _requests.append(r)
        let h = handler
        lock.unlock()
        return h(r)
    }
}

struct UncheckedSendable<T>: @unchecked Sendable {
    let value: T
    init(_ value: T) { self.value = value }
}

final class MockURLProtocol: URLProtocol {
    static let current = Locked<MockServer?>(nil)

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    private var streamTask: Task<Void, Never>?

    override func startLoading() {
        guard let server = MockURLProtocol.current.withLock({ $0 }) else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        var body = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open()
            var buf = [UInt8](repeating: 0, count: 64 * 1024)
            while stream.hasBytesAvailable {
                let n = stream.read(&buf, maxLength: buf.count)
                if n <= 0 { break }
                body.append(buf, count: n)
            }
            stream.close()
        }
        var headers: [String: String] = [:]
        for (k, v) in request.allHTTPHeaderFields ?? [:] { headers[k] = v }
        let rec = RecordedRequest(method: request.httpMethod ?? "GET", url: request.url!, headers: headers, body: body)
        let res = server.record(rec)
        if let error = res.error {
            client?.urlProtocol(self, didFailWithError: error)
            return
        }
        let http = HTTPURLResponse(url: request.url!, statusCode: res.status, httpVersion: "HTTP/1.1", headerFields: res.headers)!
        client?.urlProtocol(self, didReceive: http, cacheStoragePolicy: .notAllowed)
        if let chunks = res.chunks {
            let me = UncheckedSendable(self)
            streamTask = Task {
                for c in chunks {
                    guard !Task.isCancelled else { return }
                    me.value.client?.urlProtocol(me.value, didLoad: c)
                    try? await Task.sleep(nanoseconds: 2_000_000)
                }
                if !res.hold { me.value.finish() }
            }
        } else {
            if !res.body.isEmpty { client?.urlProtocol(self, didLoad: res.body) }
            client?.urlProtocolDidFinishLoading(self)
        }
    }

    private func finish() { client?.urlProtocolDidFinishLoading(self) }

    override func stopLoading() { streamTask?.cancel() }
}

// MARK: - fake WebSocket

/// A FIFO of server messages; `push` is synchronous so messages arrive in the order they were sent.
final class Mailbox: @unchecked Sendable {
    private struct State {
        var items: [Result<String, WebSocketClosed>] = []
        var waiter: CheckedContinuation<Result<String, WebSocketClosed>, Never>?
    }
    private let state = Locked(State())

    func push(_ item: Result<String, WebSocketClosed>) {
        state.withLock { s in
            if let w = s.waiter {
                s.waiter = nil
                w.resume(returning: item)
            } else {
                s.items.append(item)
            }
        }
    }

    func next() async -> Result<String, WebSocketClosed> {
        await withCheckedContinuation { (c: CheckedContinuation<Result<String, WebSocketClosed>, Never>) in
            state.withLock { s in
                if !s.items.isEmpty { c.resume(returning: s.items.removeFirst()) } else { s.waiter = c }
            }
        }
    }
}

final class FakeWebSocket: WebSocketConnection, @unchecked Sendable {
    let url: URL
    let headers: [String: String]
    private let box = Mailbox()
    private struct State {
        var sent: [JSON] = []
        var closed: (code: Int, reason: String)?
        var onSend: (@Sendable (FakeWebSocket, JSON) -> Void)?
    }
    private let state = Locked(State())

    init(url: URL, headers: [String: String]) {
        self.url = url
        self.headers = headers
    }

    /// Called for every client message; reply with `push`.
    var onSend: (@Sendable (FakeWebSocket, JSON) -> Void)? {
        get { state.withLock { $0.onSend } }
        set { state.withLock { $0.onSend = newValue } }
    }

    var sent: [JSON] { state.withLock { $0.sent } }
    var closedWith: (code: Int, reason: String)? { state.withLock { $0.closed } }

    func send(_ text: String) async throws {
        let j = try JSON(data: Data(text.utf8))
        let cb = state.withLock { s -> (@Sendable (FakeWebSocket, JSON) -> Void)? in
            s.sent.append(j)
            return s.onSend
        }
        cb?(self, j)
    }

    func receive() async throws -> String {
        switch await box.next() {
        case .success(let s): return s
        case .failure(let c): throw c
        }
    }

    func close(code: Int, reason: String) async {
        state.withLock { $0.closed = (code, reason) }
        box.push(.failure(WebSocketClosed(code: code, reason: reason)))
    }

    /// The server sends a message.
    func push(_ json: JSON) {
        let text = String(decoding: (try? json.data()) ?? Data(), as: UTF8.self)
        box.push(.success(text))
    }

    /// The server closes the socket.
    func serverClose(code: Int, reason: String = "") {
        box.push(.failure(WebSocketClosed(code: code, reason: reason)))
    }

    func sent(type: String) -> [JSON] { sent.filter { $0["type"]?.stringValue == type } }
}

final class FakeWebSocketFactory: WebSocketFactory, @unchecked Sendable {
    private struct State {
        var sockets: [FakeWebSocket] = []
        var onConnect: (@Sendable (FakeWebSocket) -> Void)?
        var failFirst = 0
    }
    private let state = Locked(State())

    /// Called for every new connection (send `welcome` here).
    var onConnect: (@Sendable (FakeWebSocket) -> Void)? {
        get { state.withLock { $0.onConnect } }
        set { state.withLock { $0.onConnect = newValue } }
    }

    /// Number of connection attempts to refuse first.
    var failFirst: Int {
        get { state.withLock { $0.failFirst } }
        set { state.withLock { $0.failFirst = newValue } }
    }

    var sockets: [FakeWebSocket] { state.withLock { $0.sockets } }

    func connect(to url: URL, headers: [String: String]) async throws -> any WebSocketConnection {
        let socket = FakeWebSocket(url: url, headers: headers)
        let (cb, refuse) = state.withLock { s -> ((@Sendable (FakeWebSocket) -> Void)?, Bool) in
            let attempt = s.sockets.count
            s.sockets.append(socket)
            return (s.onConnect, attempt < s.failFirst)
        }
        if refuse { throw URLError(.cannotConnectToHost) }
        cb?(socket)
        return socket
    }
}

/// A server that answers `subscribe` / `unsubscribe` / `publish` / `presence.*` / `ping` like the real one.
func autoReply(_ s: FakeWebSocket, _ m: JSON) {
    let ref = m["ref"] ?? .null
    switch m["type"]?.stringValue {
    case "subscribe": s.push(["type": "subscribed", "id": m["id"] ?? .null, "cursor": m["cursor"] ?? 0, "ref": ref])
    case "unsubscribe": s.push(["type": "unsubscribed", "id": m["id"] ?? .null, "ref": ref])
    case "publish": s.push(["type": "ack", "ref": ref])
    case "presence.join": s.push(["type": "presence.state", "channel": m["channel"] ?? .null, "members": [], "ref": ref])
    case "presence.leave": s.push(["type": "presence.left", "channel": m["channel"] ?? .null, "ref": ref])
    case "ping": s.push(["type": "pong", "ref": ref])
    default: break
    }
}

// MARK: - client factory

final class TestEnv: @unchecked Sendable {
    let server = MockServer()
    let sleeps = Locked<[TimeInterval]>([])
    let clock = Locked<Date>(Date(timeIntervalSince1970: 1_700_000_000))
    let sockets = FakeWebSocketFactory()
    let store = InMemoryTokenStore()
    let base = URL(string: "https://fb.test")!

    init() {
        MockURLProtocol.current.withLock { $0 = server }
        sockets.onConnect = { s in
            s.push(["type": "welcome", "heartbeat_ms": 3_600_000])
        }
    }

    deinit { MockURLProtocol.current.withLock { $0 = nil } }

    var session: URLSession {
        let cfg = URLSessionConfiguration.ephemeral
        cfg.protocolClasses = [MockURLProtocol.self]
        return URLSession(configuration: cfg)
    }

    func configuration(
        apiKey: String? = nil, accessToken: String? = nil, retry: RetryPolicy? = nil, consistency: Consistency = .session,
        headers: [String: String] = [:], autoRefresh: Bool = true
    ) -> ForgeBaseConfiguration {
        let sleeps = self.sleeps
        let policy =
            retry
            ?? RetryPolicy(
                maxRetries: 3, sleep: { s in sleeps.withLock { $0.append(s) } },
                random: { 0.5 })
        var rp = policy
        if retry != nil {
            rp.sleep = { s in sleeps.withLock { $0.append(s) } }
        }
        let clock = self.clock
        return ForgeBaseConfiguration(
            baseURL: base, apiKey: apiKey, accessToken: accessToken, tokenStore: store, autoRefresh: autoRefresh, retry: rp,
            headers: headers, consistency: consistency, session: session,
            realtime: RealtimeConfiguration(webSocketFactory: sockets, reconnectBaseDelay: 0.01, reconnectMaxDelay: 0.05),
            now: { clock.withLock { $0 } })
    }

    func client(
        apiKey: String? = nil, accessToken: String? = nil, retry: RetryPolicy? = nil, consistency: Consistency = .session,
        headers: [String: String] = [:], autoRefresh: Bool = true
    ) -> ForgeBaseClient {
        ForgeBaseClient(configuration: configuration(apiKey: apiKey, accessToken: accessToken, retry: retry, consistency: consistency, headers: headers, autoRefresh: autoRefresh))
    }

    func advance(_ seconds: TimeInterval) { clock.withLock { $0 = $0.addingTimeInterval(seconds) } }
}

let sessionJSON: JSON = [
    "token_type": "Bearer", "access_token": "at_1", "expires_in": 900, "refresh_token": "rt_1",
    "session_id": "sess_1", "session_expires_at": "2030-01-01T00:00:00Z",
    "subject": ["kind": "user", "collection": "users", "id": "u1"], "user": ["id": "u1", "email": "ada@example.com"],
]

func sessionJSON(access: String, refresh: String, expiresIn: Int = 900) -> JSON {
    var j = sessionJSON
    guard case .object(var o) = j else { return j }
    o["access_token"] = .string(access)
    o["refresh_token"] = .string(refresh)
    o["expires_in"] = .int(Int64(expiresIn))
    j = .object(o)
    return j
}

/// Polls until `condition` holds (or fails the test after `timeout`).
func eventually(_ timeout: TimeInterval = 3, _ message: @autoclosure () -> String = "condition not met", file: StaticString = #filePath, line: UInt = #line, _ condition: @escaping @Sendable () async -> Bool) async {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if await condition() { return }
        try? await Task.sleep(nanoseconds: 5_000_000)
    }
    XCTFail(message(), file: file, line: line)
}

func XCTAssertThrowsForgeBase<T>(
    _ expression: @autoclosure () async throws -> T, kind: ForgeBaseError.Kind, status: Int? = nil, code: String? = nil,
    file: StaticString = #filePath, line: UInt = #line
) async {
    do {
        _ = try await expression()
        XCTFail("expected a \(kind) error", file: file, line: line)
    } catch let e as ForgeBaseError {
        XCTAssertEqual(e.kind, kind, "\(e)", file: file, line: line)
        if let status { XCTAssertEqual(e.status, status, file: file, line: line) }
        if let code { XCTAssertEqual(e.code, code, file: file, line: line) }
    } catch {
        XCTFail("unexpected error \(error)", file: file, line: line)
    }
}
