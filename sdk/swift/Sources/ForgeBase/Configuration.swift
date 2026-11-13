import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Retry policy for `429` / `503` (and idempotent `502` / `504`) and network failures of idempotent calls.
public struct RetryPolicy: Sendable {
    /// Retries after the first attempt (default 3). `0` disables retrying.
    public var maxRetries: Int
    /// First backoff delay in seconds (default 0.25); doubles each attempt with full jitter.
    public var baseDelay: TimeInterval
    /// Upper bound of one backoff delay in seconds (default 10).
    public var maxDelay: TimeInterval
    /// Longest `Retry-After` the SDK sleeps for (default 60 s); longer asks fail at once.
    public var maxRetryAfter: TimeInterval
    /// Statuses retried for every method (default `[429, 503]`). 502 and 504 are retried for idempotent methods only.
    public var statuses: Set<Int>
    /// Test hook: how to wait.
    public var sleep: @Sendable (TimeInterval) async throws -> Void
    /// Test hook: returns `[0, 1)`.
    public var random: @Sendable () -> Double

    public init(
        maxRetries: Int = 3, baseDelay: TimeInterval = 0.25, maxDelay: TimeInterval = 10,
        maxRetryAfter: TimeInterval = 60, statuses: Set<Int> = [429, 503],
        sleep: @escaping @Sendable (TimeInterval) async throws -> Void = RetryPolicy.defaultSleep,
        random: @escaping @Sendable () -> Double = { Double.random(in: 0..<1) }
    ) {
        self.maxRetries = maxRetries
        self.baseDelay = baseDelay
        self.maxDelay = maxDelay
        self.maxRetryAfter = maxRetryAfter
        self.statuses = statuses
        self.sleep = sleep
        self.random = random
    }

    public static let `default` = RetryPolicy()
    public static var disabled: RetryPolicy { RetryPolicy(maxRetries: 0) }

    @Sendable public static func defaultSleep(_ seconds: TimeInterval) async throws {
        guard seconds > 0 else { return }
        try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }

    /// Delay before retry number `attempt` (1-based): `Retry-After` when sent, else exponential backoff with full jitter.
    func backoff(attempt: Int, retryAfter: TimeInterval?) -> TimeInterval {
        if let retryAfter { return retryAfter }
        let ceiling = min(maxDelay, baseDelay * pow(2, Double(attempt - 1)))
        return random() * ceiling
    }
}

/// Everything a `ForgeBaseClient` needs.
public struct ForgeBaseConfiguration: Sendable {
    /// Server root, e.g. `https://api.example.com` (no trailing path).
    public var baseURL: URL
    /// Static API key / service-account key / personal access token, sent as `X-API-Key`.
    public var apiKey: String?
    /// Static bearer access token (no refresh).
    public var accessToken: String?
    /// Where a signed-in session is kept between launches. Default: memory only. Use `KeychainTokenStore` on devices.
    public var tokenStore: any TokenStore
    /// Refresh the access token this long before it expires (default 30 s).
    public var refreshSkew: TimeInterval
    /// Refresh automatically (default true).
    public var autoRefresh: Bool
    public var retry: RetryPolicy
    /// Headers sent with every request (`X-Forgebase-Org`, tenant selection, ...).
    public var headers: [String: String]
    /// `.session` (default) reads your own writes through commit tokens; `.strong` reads every call from the primary.
    public var consistency: Consistency
    /// The URLSession used for every request (tests install a `URLProtocol`; apps may add pinning, proxies, ...).
    /// Must be a default or ephemeral session: background sessions do not support the async APIs (see README).
    public var session: URLSession
    /// Per-request timeout in seconds (default 60).
    public var timeout: TimeInterval
    /// Realtime connection settings.
    public var realtime: RealtimeConfiguration
    /// Clock, for tests.
    public var now: @Sendable () -> Date

    public init(
        baseURL: URL,
        apiKey: String? = nil,
        accessToken: String? = nil,
        tokenStore: any TokenStore = InMemoryTokenStore(),
        refreshSkew: TimeInterval = 30,
        autoRefresh: Bool = true,
        retry: RetryPolicy = .default,
        headers: [String: String] = [:],
        consistency: Consistency = .session,
        session: URLSession = .shared,
        timeout: TimeInterval = 60,
        realtime: RealtimeConfiguration = RealtimeConfiguration(),
        now: @escaping @Sendable () -> Date = { Date() }
    ) {
        self.baseURL = baseURL
        self.apiKey = apiKey
        self.accessToken = accessToken
        self.tokenStore = tokenStore
        self.refreshSkew = refreshSkew
        self.autoRefresh = autoRefresh
        self.retry = retry
        self.headers = headers
        self.consistency = consistency
        self.session = session
        self.timeout = timeout
        self.realtime = realtime
        self.now = now
    }
}

/// Percent-encodes one URL path segment or query component.
func seg(_ s: String) -> String {
    s.addingPercentEncoding(withAllowedCharacters: .forgebaseUnreserved) ?? s
}

/// A storage key in a URL path: slashes stay, every segment is encoded.
func keyPath(_ key: String) -> String {
    key.split(separator: "/", omittingEmptySubsequences: false).map { seg(String($0)) }.joined(separator: "/")
}

extension CharacterSet {
    static let forgebaseUnreserved = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
}
