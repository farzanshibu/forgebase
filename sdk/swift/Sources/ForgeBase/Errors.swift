import Foundation

/// A per-field validation failure (`details.fields` of a 400).
public struct FieldError: Sendable, Equatable {
    public let code: String
    public let message: String
}

/// Everything the SDK throws is a `ForgeBaseError`. Switch on `kind`:
///
///     do { try await fb.collection("posts").update(id, patch, version: 3) }
///     catch let e as ForgeBaseError where e.kind == .conflict { /* reload and retry */ }
public struct ForgeBaseError: Error, Sendable, LocalizedError, CustomStringConvertible {
    public enum Kind: String, Sendable {
        /// 400 / 422: `validation_failed` and other malformed requests.
        case validation
        /// 401: missing, expired or invalid credentials.
        case authentication
        /// 403: credentials fine, action not allowed (policy, scope, quota-gated feature).
        case permission
        /// 404.
        case notFound
        /// 409 (unique violation, stale version, ...) and 412.
        case conflict
        /// 429 from rate limiting. Already retried `maxRetries` times when thrown.
        case rateLimited
        /// 429 because a quota is exhausted. Never retried: waiting will not help.
        case quotaExceeded
        /// 5xx.
        case server
        /// Any other non-2xx status.
        case api
        /// No HTTP response (DNS, refused connection, timeout).
        case network
        /// A 2xx answer that is not what the SDK expected.
        case invalidResponse
        /// The request could not be built (bad URL, bad argument).
        case invalidRequest
        /// WebSocket / event-stream failures.
        case realtime
        /// A push request failed signature verification.
        case webhook
        /// A job ended `dead` or `cancelled`, or a wait timed out.
        case jobFailed
        /// The browser sign-in flow was cancelled or failed.
        case oauth
    }

    public let kind: Kind
    /// HTTP status; `0` when no response was received.
    public let status: Int
    /// Stable machine-readable code (`validation_failed`, `network_error`, ...).
    public let code: String
    public let message: String
    public let requestId: String?
    public let details: JSON?
    /// Seconds the server asked the client to wait (`Retry-After`).
    public let retryAfter: TimeInterval?
    /// Parsed response body, when there was one.
    public let body: JSON?
    public let underlying: (any Error)?

    public init(
        kind: Kind, status: Int = 0, code: String, message: String, requestId: String? = nil,
        details: JSON? = nil, retryAfter: TimeInterval? = nil, body: JSON? = nil, underlying: (any Error)? = nil
    ) {
        self.kind = kind
        self.status = status
        self.code = code
        self.message = message
        self.requestId = requestId
        self.details = details
        self.retryAfter = retryAfter
        self.body = body
        self.underlying = underlying
    }

    public var errorDescription: String? { message }
    public var description: String { "ForgeBaseError(\(kind.rawValue), status: \(status), code: \(code)): \(message)" }

    /// Per-field errors of a validation failure.
    public var fields: [String: FieldError] {
        guard let f = details?["fields"]?.objectValue else { return [:] }
        var out: [String: FieldError] = [:]
        for (k, v) in f {
            guard let o = v.objectValue else { continue }
            out[k] = FieldError(code: o["code"]?.stringValue ?? "invalid", message: o["message"]?.stringValue ?? "")
        }
        return out
    }

    // MARK: constructors

    static func network(_ message: String, underlying: (any Error)? = nil) -> ForgeBaseError {
        ForgeBaseError(kind: .network, code: "network_error", message: message, underlying: underlying)
    }

    static func invalidResponse(_ message: String, status: Int = 200) -> ForgeBaseError {
        ForgeBaseError(kind: .invalidResponse, status: status, code: "invalid_response", message: message)
    }

    static func invalidRequest(_ message: String) -> ForgeBaseError {
        ForgeBaseError(kind: .invalidRequest, code: "invalid_request", message: message)
    }

    static func realtime(_ code: String, _ message: String) -> ForgeBaseError {
        ForgeBaseError(kind: .realtime, code: code, message: message)
    }

    private static let quotaCode = try! NSRegularExpression(pattern: "quota|limit_exceeded|usage_limit", options: [.caseInsensitive])

    /// Builds the right error from a failed response (server error format: `{"error": {"code", "message", "request_id", "details"}}`).
    static func from(status: Int, body: JSON?, headers: HTTPURLResponse?) -> ForgeBaseError {
        let env = body?["error"]
        let code = env?["code"]?.stringValue ?? "http_error"
        let message = env?["message"]?.stringValue ?? "HTTP \(status)"
        let requestId = env?["request_id"]?.stringValue ?? headers?.value(forHTTPHeaderField: "X-Request-Id")
        let retry = parseRetryAfter(headers?.value(forHTTPHeaderField: "Retry-After"))
        let kind: Kind
        switch status {
        case 429:
            let range = NSRange(code.startIndex..., in: code)
            kind = quotaCode.firstMatch(in: code, range: range) != nil ? .quotaExceeded : .rateLimited
        case 401: kind = .authentication
        case 403: kind = .permission
        case 404: kind = .notFound
        case 409, 412: kind = .conflict
        case 400, 422: kind = .validation
        case 500...: kind = .server
        default: kind = .api
        }
        return ForgeBaseError(
            kind: kind, status: status, code: code, message: message, requestId: requestId,
            details: env?["details"], retryAfter: retry, body: body)
    }
}

/// `Retry-After` as seconds (delta-seconds or HTTP-date); `nil` when absent or invalid.
func parseRetryAfter(_ value: String?, now: Date = Date()) -> TimeInterval? {
    guard let raw = value?.trimmingCharacters(in: .whitespaces), !raw.isEmpty else { return nil }
    if let secs = Double(raw), secs >= 0 { return secs }
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.timeZone = TimeZone(identifier: "GMT")
    for fmt in ["EEE, dd MMM yyyy HH:mm:ss zzz", "EEEE, dd-MMM-yy HH:mm:ss zzz", "EEE MMM d HH:mm:ss yyyy"] {
        f.dateFormat = fmt
        if let d = f.date(from: raw) { return max(0, d.timeIntervalSince(now)) }
    }
    return nil
}
