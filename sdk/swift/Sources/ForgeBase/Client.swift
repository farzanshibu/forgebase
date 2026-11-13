import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Entry point.
///
///     let fb = ForgeBaseClient(baseURL: URL(string: "https://api.example.com")!)
///     try await fb.auth.signIn(email: "ada@example.com", password: "...")
///     let posts = try await fb.collection("posts").list(.init(filter: "published = true", sort: ["-created"]))
public final class ForgeBaseClient: Sendable {
    public let baseURL: URL
    /// Password, magic-link, OTP, MFA and OAuth sign-in; sessions.
    public let auth: AuthAPI
    /// Collection definitions.
    public let collections: CollectionsAPI
    /// WebSocket subscriptions: collections, records, channels, presence.
    public let realtime: RealtimeClient
    /// Buckets, objects, multipart and resumable uploads, signed URLs.
    public let storage: StorageAPI
    /// Key-value engine: `fb.kv.ns("cache")`.
    public let kv: KVAPI
    /// Server functions: invoke.
    public let functions: FunctionsAPI
    /// Durable topics and queues.
    public let queues: QueuesAPI
    /// Background jobs.
    public let jobs: JobsAPI
    /// Entitlement checks, quotas, usage.
    public let entitlements: EntitlementsAPI
    /// Customer billing, store purchases.
    public let billing: BillingAPI
    public let credits: CreditsAPI
    public let vector: VectorAPI
    public let graph: GraphAPI
    public let sql: SQLAPI

    let transport: Transport
    private let manager: AuthManager

    public init(configuration c: ForgeBaseConfiguration) {
        let manager = AuthManager(configuration: c)
        let transport = Transport(
            baseURL: c.baseURL, session: c.session, retry: c.retry, headers: c.headers, consistency: c.consistency,
            timeout: c.timeout, credentials: manager)
        self.baseURL = c.baseURL
        self.manager = manager
        self.transport = transport
        auth = AuthAPI(transport: transport, manager: manager)
        collections = CollectionsAPI(t: transport)
        let realtime = RealtimeClient(transport: transport, auth: manager, configuration: c.realtime, now: c.now)
        self.realtime = realtime
        storage = StorageAPI(transport: transport)
        kv = KVAPI(t: transport)
        functions = FunctionsAPI(t: transport)
        queues = QueuesAPI(t: transport)
        jobs = JobsAPI(t: transport, realtime: realtime)
        entitlements = EntitlementsAPI(t: transport)
        billing = BillingAPI(transport: transport)
        credits = CreditsAPI(t: transport)
        vector = VectorAPI(t: transport)
        graph = GraphAPI(t: transport)
        sql = SQLAPI(t: transport)
    }

    /// Shortcut for the common setup.
    public convenience init(
        baseURL: URL, apiKey: String? = nil, tokenStore: any TokenStore = InMemoryTokenStore(), session: URLSession = .shared
    ) {
        self.init(configuration: ForgeBaseConfiguration(baseURL: baseURL, apiKey: apiKey, tokenStore: tokenStore, session: session))
    }

    /// Records of a collection as plain JSON objects: `fb.collection("posts")`.
    public func collection(_ name: String) -> RecordsAPI<JSONObject> {
        RecordsAPI(transport: transport, name: name)
    }

    /// Records of a collection as your own `Codable` type: `fb.collection("posts", as: Post.self)`.
    public func collection<T: Codable & Sendable>(_ name: String, as type: T.Type) -> RecordsAPI<T> {
        RecordsAPI(transport: transport, name: name)
    }

    /// Commit tokens of this client's writes, sent as `X-ForgeBase-Read-After`.
    public var commitTokens: CommitTokens { transport.commitTokens }

    /// Escape hatch for any route: `try await fb.request("GET", "/api/v1/kv/_info")`.
    public func request(_ method: String, _ path: String, query: [String: String] = [:], body: JSON? = nil) async throws -> JSON {
        let items = query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
        return try await transport.json(Request(method, path, query: items, body: body))
    }

    /// Server version and build info.
    public func systemInfo() async throws -> JSON { try await request("GET", "/api/system/info") }

    /// Liveness probe: `true` when the server answers.
    public func health() async -> Bool {
        var r = Request("GET", "/health")
        r.anonymous = true
        r.retry = .disabled
        return (try? await transport.perform(r)) != nil
    }
}
