import Foundation

/// A signed-in session.
public struct Session: Codable, Sendable, Equatable {
    /// Bearer token.
    public var accessToken: String?
    /// Rotating refresh token.
    public var refreshToken: String?
    /// Access token expiry.
    public var expiresAt: Date
    public var sessionId: String
    /// Absolute session expiry (RFC 3339).
    public var sessionExpiresAt: String
    /// Who signed in (`{kind: "user", collection, id}` and friends).
    public var subject: JSON
    /// The user record, for user sign-ins.
    public var user: JSON?

    public init(
        accessToken: String?, refreshToken: String?, expiresAt: Date, sessionId: String,
        sessionExpiresAt: String, subject: JSON = .null, user: JSON? = nil
    ) {
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        self.expiresAt = expiresAt
        self.sessionId = sessionId
        self.sessionExpiresAt = sessionExpiresAt
        self.subject = subject
        self.user = user
    }

    /// The id of the signed-in user, when the subject is a user.
    public var userId: String? { subject["id"]?.stringValue }
}

/// A second factor is required: call `auth.verifyMFA(mfaToken:code:)`.
public struct MFAChallenge: Sendable, Equatable {
    public let mfaToken: String
    /// Seconds the challenge is valid.
    public let expiresIn: TimeInterval
}

/// Result of a sign-in: a session, or an MFA challenge.
public enum SignInResult: Sendable, Equatable {
    case session(Session)
    case mfaRequired(MFAChallenge)

    /// The session, or `nil` when a second factor is still needed.
    public var session: Session? { if case .session(let s) = self { return s } else { return nil } }
    public var challenge: MFAChallenge? { if case .mfaRequired(let c) = self { return c } else { return nil } }
}

/// Options shared by the sign-in flows.
public struct SignInOptions: Sendable {
    /// User collection (server default `users`).
    public var collection: String?
    /// Name shown in the device list.
    public var deviceName: String?
    public init(collection: String? = nil, deviceName: String? = nil) {
        self.collection = collection
        self.deviceName = deviceName
    }
}

public enum AuthEvent: String, Sendable {
    case signedIn, tokenRefreshed, signedOut
}

public struct AuthStateChange: Sendable {
    public let event: AuthEvent
    public let session: Session?
}

private struct WireSession: Decodable {
    let accessToken: String?
    let expiresIn: Double
    let refreshToken: String?
    let sessionId: String?
    let sessionExpiresAt: String?
    let subject: JSON?
    let user: JSON?

    enum CodingKeys: String, CodingKey {
        case accessToken = "access_token", expiresIn = "expires_in", refreshToken = "refresh_token"
        case sessionId = "session_id", sessionExpiresAt = "session_expires_at", subject, user
    }
}

/// Holds the session and implements the transport's credentials (headers, refresh, retry after a 401).
actor AuthManager: Credentials {
    private var session: Session?
    private var loaded = false
    private var refreshing: Task<Session, Error>?
    private var listeners: [UUID: AsyncStream<AuthStateChange>.Continuation] = [:]
    private let store: any TokenStore
    private let now: @Sendable () -> Date
    private let skew: TimeInterval
    private let autoRefresh: Bool
    private var apiKey: String?
    private var staticToken: String?

    init(configuration c: ForgeBaseConfiguration) {
        store = c.tokenStore
        now = c.now
        skew = c.refreshSkew
        autoRefresh = c.autoRefresh
        apiKey = c.apiKey
        staticToken = c.accessToken
    }

    // MARK: state

    func stateChanges() -> AsyncStream<AuthStateChange> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<AuthStateChange>.makeStream(bufferingPolicy: .bufferingNewest(16))
        listeners[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { await self?.removeListener(id) }
        }
        return stream
    }

    private func removeListener(_ id: UUID) { listeners[id] = nil }

    private func emit(_ event: AuthEvent) {
        let change = AuthStateChange(event: event, session: session)
        for c in listeners.values { c.yield(change) }
    }

    private func load() async {
        if loaded { return }
        loaded = true
        if session == nil { session = try? await store.load() }
    }

    func currentSession() async -> Session? {
        await load()
        return session
    }

    func setSession(_ s: Session?, event: AuthEvent = .signedIn) async {
        loaded = true
        session = s
        if let s { try? await store.save(s) } else { try? await store.clear() }
        emit(s == nil ? .signedOut : event)
    }

    func setAPIKey(_ key: String?) { apiKey = key }
    func setAccessToken(_ token: String?) { staticToken = token }
    var hasAPIKey: Bool { apiKey != nil }

    /// Current access token, refreshed first when it is about to expire.
    func accessToken(transport: Transport) async -> String? {
        if let staticToken { return staticToken }
        await load()
        guard let s = session, let token = s.accessToken else { return nil }
        if autoRefresh, s.refreshToken != nil, s.expiresAt.addingTimeInterval(-skew) <= now() {
            if let fresh = try? await refresh(transport: transport) { return fresh.accessToken }
            return token
        }
        return token
    }

    /// What the realtime connection authenticates with: an API key, else the (refreshed) access token.
    func realtimeCredential(transport: Transport) async -> String? {
        if let apiKey { return apiKey }
        return await accessToken(transport: transport)
    }

    /// Rotates the refresh token. Concurrent callers share one request.
    func refresh(transport: Transport) async throws -> Session {
        if let refreshing { return try await refreshing.value }
        let task = Task { try await self.doRefresh(transport: transport) }
        refreshing = task
        defer { refreshing = nil }
        return try await task.value
    }

    private func doRefresh(transport: Transport) async throws -> Session {
        await load()
        guard let rt = session?.refreshToken else {
            throw ForgeBaseError(kind: .authentication, status: 401, code: "no_refresh_token", message: "no refresh token: sign in first")
        }
        do {
            var req = Request("POST", "/api/auth/refresh", body: wire(["refresh_token": rt]))
            req.anonymous = true
            let w: WireSession = try await transport.decode(req)
            var next = try toSession(w)
            if next.user == nil { next.user = session?.user }
            await setSession(next, event: .tokenRefreshed)
            return next
        } catch let e as ForgeBaseError where e.status == 401 {
            // A rejected refresh token (revoked, reused) ends the session; transient failures keep it.
            await setSession(nil)
            throw e
        }
    }

    private func toSession(_ w: WireSession) throws -> Session {
        guard let id = w.sessionId else { throw ForgeBaseError.invalidResponse("sign-in response carries no session") }
        return Session(
            accessToken: w.accessToken, refreshToken: w.refreshToken,
            expiresAt: now().addingTimeInterval(w.expiresIn), sessionId: id,
            sessionExpiresAt: w.sessionExpiresAt ?? "", subject: w.subject ?? .null, user: w.user)
    }

    // MARK: Credentials

    func headers(for method: String, transport: Transport) async -> [String: String] {
        if let apiKey { return ["X-API-Key": apiKey] }
        if let token = await accessToken(transport: transport) { return ["Authorization": "Bearer \(token)"] }
        return [:]
    }

    func onUnauthorized(sent: [String: String], transport: Transport) async -> Bool {
        guard apiKey == nil, autoRefresh, let auth = sent["Authorization"] else { return false }
        await load()
        guard let s = session, s.refreshToken != nil else { return false }
        // Someone else already rotated the token since this request was built: just retry.
        if let current = s.accessToken, auth != "Bearer \(current)" { return true }
        return (try? await refresh(transport: transport)) != nil
    }

    /// Converts a sign-in style response and stores the session.
    func accept(_ body: JSON) async throws -> SignInResult {
        if body["mfa_required"]?.boolValue == true {
            return .mfaRequired(
                MFAChallenge(mfaToken: body["mfa_token"]?.stringValue ?? "", expiresIn: body["expires_in"]?.doubleValue ?? 0))
        }
        let s = try toSession(try body.decode(WireSession.self))
        await setSession(s)
        return .session(s)
    }
}

/// `client.auth`: sign-up, sign-in flows, refresh and account helpers.
public struct AuthAPI: Sendable {
    let t: Transport
    let mgr: AuthManager
    public let oauth: OAuthAPI

    init(transport: Transport, manager: AuthManager) {
        t = transport
        mgr = manager
        oauth = OAuthAPI(t: transport, mgr: manager)
    }

    /// Sign-in, refresh and sign-out events (a new stream per call).
    public func stateChanges() async -> AsyncStream<AuthStateChange> { await mgr.stateChanges() }

    /// The current session (restored from the token store on first use).
    public func session() async -> Session? { await mgr.currentSession() }

    /// The current access token, refreshed first when it is about to expire.
    public func accessToken() async -> String? { await mgr.accessToken(transport: t) }

    /// Adopt a session obtained elsewhere (or `nil` to forget it locally).
    public func setSession(_ s: Session?) async { await mgr.setSession(s) }

    /// Use an API key (`fb_...`) for every request, or `nil` to go back to the session.
    public func setAPIKey(_ key: String?) async { await mgr.setAPIKey(key) }

    /// Creates a user. Does not sign in. Returns the user record.
    @discardableResult
    public func signUp(email: String, password: String, collection: String? = nil, data: JSONObject? = nil) async throws -> JSON {
        var r = Request("POST", "/api/auth/signup", body: wire(["email": email, "password": password, "collection": collection, "data": data.map(JSON.object)]))
        r.anonymous = true
        return try await t.json(r)["user"] ?? .null
    }

    /// Email + password. Returns a session, or an MFA challenge when a second factor is needed.
    public func signIn(email: String, password: String, options: SignInOptions = .init()) async throws -> SignInResult {
        try await mgr.accept(post("/api/auth/login", ["email": email, "password": password], options))
    }

    /// Administrator sign-in (`/api/auth/admins/login`).
    public func signInAdmin(email: String, password: String, deviceName: String? = nil) async throws -> SignInResult {
        try await mgr.accept(post("/api/auth/admins/login", ["email": email, "password": password], SignInOptions(deviceName: deviceName)))
    }

    /// Second step after `signIn` returned an MFA challenge: a TOTP or recovery code.
    @discardableResult
    public func verifyMFA(mfaToken: String, code: String) async throws -> Session {
        let r = try await mgr.accept(post("/api/auth/mfa/verify", ["mfa_token": mfaToken, "code": code], .init()))
        guard let s = r.session else { throw ForgeBaseError.invalidResponse("MFA verification returned another challenge") }
        return s
    }

    /// Emails a magic link; pass the `token` of the link to `confirmMagicLink`.
    public func requestMagicLink(email: String, collection: String? = nil) async throws {
        _ = try await post("/api/auth/magic-link/request", ["email": email], SignInOptions(collection: collection))
    }

    public func confirmMagicLink(token: String, deviceName: String? = nil) async throws -> SignInResult {
        try await mgr.accept(post("/api/auth/magic-link/confirm", ["token": token], SignInOptions(deviceName: deviceName)))
    }

    /// Emails a one-time code.
    public func requestOTP(email: String, collection: String? = nil) async throws {
        _ = try await post("/api/auth/otp/request", ["email": email], SignInOptions(collection: collection))
    }

    public func verifyOTP(email: String, code: String, options: SignInOptions = .init()) async throws -> SignInResult {
        try await mgr.accept(post("/api/auth/otp/verify", ["email": email, "code": code], options))
    }

    /// Rotates the refresh token and returns the new session (also done automatically).
    @discardableResult
    public func refresh() async throws -> Session { try await mgr.refresh(transport: t) }

    /// Revokes the session on the server and forgets it locally (also when the server call fails).
    public func signOut() async throws {
        defer { Task { await t.commitTokens.clear() } }
        do {
            if await mgr.currentSession() != nil { try await t.perform(Request("POST", "/api/auth/logout")) }
        } catch {
            await mgr.setSession(nil)
            throw error
        }
        await mgr.setSession(nil)
    }

    /// The caller as the server sees it (`subject`, `credential`, `mfa`, scopes, ...).
    public func me() async throws -> JSON { try await t.json(Request("GET", "/api/auth/me")) }

    public func requestPasswordReset(email: String, collection: String? = nil) async throws {
        _ = try await post("/api/auth/password-reset/request", ["email": email], SignInOptions(collection: collection))
    }

    public func confirmPasswordReset(token: String, password: String) async throws {
        _ = try await post("/api/auth/password-reset/confirm", ["token": token, "password": password], .init())
    }

    public func requestEmailVerification(email: String, collection: String? = nil) async throws {
        _ = try await post("/api/auth/verify-email/request", ["email": email], SignInOptions(collection: collection))
    }

    public func confirmEmailVerification(token: String) async throws {
        _ = try await post("/api/auth/verify-email/confirm", ["token": token], .init())
    }

    public func changePassword(current: String, new: String) async throws {
        try await t.perform(Request("POST", "/api/auth/password/change", body: wire(["current_password": current, "new_password": new])))
    }

    /// Personal access tokens: the secret is only returned here.
    public func createToken(name: String, scopes: [String], expiresInDays: Int? = nil) async throws -> JSON {
        try await t.json(Request("POST", "/api/auth/tokens", body: wire(["name": name, "scopes": scopes, "expires_in_days": expiresInDays])))
    }

    public func listSessions() async throws -> JSON { try await t.json(Request("GET", "/api/auth/sessions")) }

    public func revokeSession(_ id: String) async throws {
        try await t.perform(Request("DELETE", "/api/auth/sessions/\(seg(id))"))
    }

    private func post(_ path: String, _ fields: [String: String], _ o: SignInOptions) async throws -> JSON {
        var body: [String: JSON] = fields.mapValues { .string($0) }
        if let c = o.collection { body["collection"] = .string(c) }
        if let d = o.deviceName { body["device_name"] = .string(d) }
        var r = Request("POST", path, body: .object(body))
        r.anonymous = true
        return try await t.json(r)
    }
}

/// The URL the browser is sent to, and the `state` that ties the redirect back to the flow.
public struct OAuthStart: Sendable, Equatable {
    public let authorizationURL: URL
    public let state: String
}

/// Social / OIDC sign-in.
///
/// PKCE: the server is the OAuth client of the provider. It generates the S256 code verifier, keeps it sealed
/// next to the one-time `state`, sends the challenge in the authorization URL and redeems the code itself. The app
/// therefore never handles a verifier; it only has to bring `code` and `state` back (`complete`). The `state` is
/// single use and bound to the flow started by `start`.
public struct OAuthAPI: Sendable {
    let t: Transport
    let mgr: AuthManager

    /// Providers configured on the server.
    public func providers() async throws -> [JSON] {
        var r = Request("GET", "/api/auth/oauth/providers")
        r.anonymous = true
        return try await t.json(r)["providers"]?.arrayValue ?? []
    }

    /// Starts the flow: the provider URL to open in a browser session, and the `state`.
    public func start(provider: String, options: SignInOptions = .init()) async throws -> OAuthStart {
        var r = Request(
            "POST", "/api/auth/oauth/\(seg(provider))/start",
            body: wire(["collection": options.collection, "device_name": options.deviceName]))
        r.anonymous = true
        let j = try await t.json(r)
        return try Self.parseStart(j)
    }

    /// Finishes the flow with the `code` and `state` the provider sent to your redirect URI.
    public func complete(provider: String, code: String, state: String) async throws -> SignInResult {
        var r = Request("GET", "/api/auth/oauth/\(seg(provider))/callback", query: query(["code": code, "state": state]))
        r.anonymous = true
        return try await mgr.accept(try await t.json(r))
    }

    /// Parses `?code=...&state=...` (or `?error=...`) from the redirect URL and completes the flow.
    public func complete(provider: String, redirectURL: URL) async throws -> SignInResult {
        let items = URLComponents(url: redirectURL, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func value(_ n: String) -> String? { items.first { $0.name == n }?.value }
        if let error = value("error") {
            throw ForgeBaseError(kind: .oauth, status: 400, code: "oauth_provider_error", message: "provider error: \(error)")
        }
        guard let code = value("code"), let state = value("state") else {
            throw ForgeBaseError(kind: .oauth, status: 400, code: "oauth_missing_code", message: "redirect URL has no code/state")
        }
        return try await complete(provider: provider, code: code, state: state)
    }

    /// The whole flow with a pluggable browser: `start`, show the URL, `complete` from the redirect.
    /// With `ASWebAuthenticationSession` use `signIn(provider:callbackURLScheme:...)` (iOS, macOS, visionOS, watchOS).
    public func signIn(provider: String, options: SignInOptions = .init(), authenticator: any BrowserAuthenticator, callbackURLScheme: String) async throws -> SignInResult {
        let started = try await start(provider: provider, options: options)
        let redirect = try await authenticator.authenticate(url: started.authorizationURL, callbackURLScheme: callbackURLScheme)
        return try await complete(provider: provider, redirectURL: redirect)
    }

    /// Starts linking a provider to the signed-in user.
    public func link(provider: String) async throws -> OAuthStart {
        try Self.parseStart(try await t.json(Request("POST", "/api/auth/oauth/\(seg(provider))/link")))
    }

    public func identities() async throws -> JSON { try await t.json(Request("GET", "/api/auth/oauth/identities")) }

    public func unlink(identityId: String) async throws {
        try await t.perform(Request("DELETE", "/api/auth/oauth/identities/\(seg(identityId))"))
    }

    private static func parseStart(_ j: JSON) throws -> OAuthStart {
        guard let s = j["authorization_url"]?.stringValue, let u = URL(string: s), let state = j["state"]?.stringValue else {
            throw ForgeBaseError.invalidResponse("OAuth start response has no authorization_url/state")
        }
        return OAuthStart(authorizationURL: u, state: state)
    }
}

/// Shows a provider's sign-in page and returns the URL it redirected to.
public protocol BrowserAuthenticator: Sendable {
    func authenticate(url: URL, callbackURLScheme: String) async throws -> URL
}
