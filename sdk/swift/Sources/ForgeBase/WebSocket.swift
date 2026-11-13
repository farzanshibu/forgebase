import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// The socket closed. `code` is the WebSocket close code (`1006` when the connection dropped without a close frame).
public struct WebSocketClosed: Error, Sendable, Equatable {
    public let code: Int
    public let reason: String
    public init(code: Int, reason: String = "") {
        self.code = code
        self.reason = reason
    }
}

/// One open text WebSocket. The SDK talks to this protocol, so tests (or another transport) can stand in for the network.
public protocol WebSocketConnection: Sendable {
    func send(_ text: String) async throws
    /// The next text message. Throws `WebSocketClosed` when the socket closed, any other error when it failed.
    func receive() async throws -> String
    func close(code: Int, reason: String) async
}

/// Opens WebSockets.
public protocol WebSocketFactory: Sendable {
    func connect(to url: URL, headers: [String: String]) async throws -> any WebSocketConnection
}

/// The default factory: `URLSessionWebSocketTask`.
public struct URLSessionWebSocketFactory: WebSocketFactory {
    public let session: URLSession
    public init(session: URLSession = .shared) { self.session = session }

    public func connect(to url: URL, headers: [String: String]) async throws -> any WebSocketConnection {
        var req = URLRequest(url: url)
        for (k, v) in headers { req.setValue(v, forHTTPHeaderField: k) }
        let task = session.webSocketTask(with: req)
        task.maximumMessageSize = 16 * 1024 * 1024
        task.resume()
        return URLSessionWebSocketConnection(task: task)
    }
}

final class URLSessionWebSocketConnection: WebSocketConnection, @unchecked Sendable {
    private let task: URLSessionWebSocketTask
    init(task: URLSessionWebSocketTask) { self.task = task }

    func send(_ text: String) async throws {
        try await task.send(.string(text))
    }

    func receive() async throws -> String {
        do {
            switch try await task.receive() {
            case .string(let s): return s
            case .data(let d): return String(decoding: d, as: UTF8.self)
            @unknown default: return ""
            }
        } catch {
            if Task.isCancelled { throw CancellationError() }
            let code = task.closeCode
            if code != .invalid {
                throw WebSocketClosed(code: code.rawValue, reason: task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? "")
            }
            throw error
        }
    }

    func close(code: Int, reason: String) async {
        task.cancel(with: URLSessionWebSocketTask.CloseCode(rawValue: code) ?? .normalClosure, reason: reason.data(using: .utf8))
    }
}

/// Realtime connection settings.
public struct RealtimeConfiguration: Sendable {
    public var webSocketFactory: (any WebSocketFactory)?
    /// First reconnect delay in seconds (default 0.5); doubles with full jitter.
    public var reconnectBaseDelay: TimeInterval
    /// Longest reconnect delay in seconds (default 15).
    public var reconnectMaxDelay: TimeInterval
    /// Give up after this many consecutive failed reconnects (default: never).
    public var maxReconnectAttempts: Int?

    /// - Parameter webSocketFactory: `nil` uses `URLSessionWebSocketTask` on the client's URLSession.
    public init(
        webSocketFactory: (any WebSocketFactory)? = nil, reconnectBaseDelay: TimeInterval = 0.5,
        reconnectMaxDelay: TimeInterval = 15, maxReconnectAttempts: Int? = nil
    ) {
        self.webSocketFactory = webSocketFactory
        self.reconnectBaseDelay = reconnectBaseDelay
        self.reconnectMaxDelay = reconnectMaxDelay
        self.maxReconnectAttempts = maxReconnectAttempts
    }
}
