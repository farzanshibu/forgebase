import XCTest
@testable import ForgeBase

final class AuthTests: XCTestCase {
    func testSignUpDoesNotSignIn() async throws {
        let env = TestEnv()
        env.server.script([.json(["user": ["id": "u1", "email": "ada@example.com"]], status: 201)])
        let fb = env.client()
        let user = try await fb.auth.signUp(email: "ada@example.com", password: "pw", data: ["name": "Ada"])
        XCTAssertEqual(user["id"], "u1")
        let r = env.server.requests[0]
        XCTAssertEqual(r.path, "/api/auth/signup")
        XCTAssertEqual(r.json?["email"], "ada@example.com")
        XCTAssertEqual(r.json?["data"]?["name"], "Ada")
        XCTAssertNil(r.header("Authorization"), "sign-up is anonymous")
        let s = await fb.auth.session()
        XCTAssertNil(s)
    }

    func testSignInStoresSessionAndAuthorisesLaterCalls() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON) },
            "GET /api/auth/me": { _ in .json(["subject": ["id": "u1"]]) },
        ])
        let fb = env.client()
        let result = try await fb.auth.signIn(email: "ada@example.com", password: "pw", options: .init(collection: "members", deviceName: "iPhone"))
        let session = try XCTUnwrap(result.session)
        XCTAssertEqual(session.accessToken, "at_1")
        XCTAssertEqual(session.refreshToken, "rt_1")
        XCTAssertEqual(session.userId, "u1")
        XCTAssertEqual(session.expiresAt, env.clock.withLock { $0 }.addingTimeInterval(900))

        let login = env.server.requests[0]
        XCTAssertNil(login.header("Authorization"))
        XCTAssertEqual(login.json?["collection"], "members")
        XCTAssertEqual(login.json?["device_name"], "iPhone")

        _ = try await fb.auth.me()
        XCTAssertEqual(env.server.requests[1].header("Authorization"), "Bearer at_1")
        let stored = try await env.store.load()
        XCTAssertEqual(stored, session)
    }

    func testMFAStep() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(["mfa_required": true, "mfa_token": "mfa_1", "expires_in": 300]) },
            "POST /api/auth/mfa/verify": { _ in .json(sessionJSON) },
        ])
        let fb = env.client()
        let r = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        XCTAssertEqual(r.challenge, MFAChallenge(mfaToken: "mfa_1", expiresIn: 300))
        XCTAssertNil(r.session)
        let stillNone = await fb.auth.session()
        XCTAssertNil(stillNone, "a challenge is not a session")
        let s = try await fb.auth.verifyMFA(mfaToken: "mfa_1", code: "123456")
        XCTAssertEqual(s.accessToken, "at_1")
        XCTAssertEqual(env.server.requests[1].json?["mfa_token"], "mfa_1")
        XCTAssertEqual(env.server.requests[1].json?["code"], "123456")
    }

    func testRefreshesBeforeTheTokenExpires() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1", expiresIn: 900)) },
            "POST /api/auth/refresh": { _ in .json(sessionJSON(access: "at_2", refresh: "rt_2")) },
            "GET /api/auth/me": { _ in .json([:]) },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        env.advance(800)  // 100 s left: outside the 30 s skew
        _ = try await fb.auth.me()
        XCTAssertEqual(env.server.requests.filter { $0.path == "/api/auth/refresh" }.count, 0)
        env.advance(75)  // 25 s left: inside the skew
        _ = try await fb.auth.me()
        let refresh = try XCTUnwrap(env.server.requests.first { $0.path == "/api/auth/refresh" })
        XCTAssertEqual(refresh.json?["refresh_token"], "rt_1", "the rotating refresh token is sent")
        XCTAssertEqual(env.server.requests.last?.header("Authorization"), "Bearer at_2")
        let rotated = try await env.store.load()
        XCTAssertEqual(rotated?.refreshToken, "rt_2")
        XCTAssertEqual(rotated?.user?["email"], "ada@example.com", "the user survives a refresh")
    }

    func test401RefreshesOnceAndRetries() async throws {
        let env = TestEnv()
        let calls = Locked(0)
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1")) },
            "POST /api/auth/refresh": { _ in .json(sessionJSON(access: "at_2", refresh: "rt_2")) },
            "GET /api/v1/collections/posts/records/p1": { r in
                calls.withLock { $0 += 1 }
                return r.header("Authorization") == "Bearer at_2" ? .json(["id": "p1"]) : .apiError(401, code: "token_expired")
            },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        let post = try await fb.collection("posts").get("p1")
        XCTAssertEqual(post["id"], "p1")
        XCTAssertEqual(calls.withLock { $0 }, 2)
    }

    func testConcurrentRefreshesShareOneRequest() async throws {
        let env = TestEnv()
        let refreshes = Locked(0)
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1", expiresIn: 10)) },
            "POST /api/auth/refresh": { _ in
                refreshes.withLock { $0 += 1 }
                return .json(sessionJSON(access: "at_2", refresh: "rt_2"))
            },
            "GET /api/auth/me": { _ in .json([:]) },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        try await withThrowingTaskGroup(of: Void.self) { g in
            for _ in 0..<8 { g.addTask { _ = try await fb.auth.me() } }
            try await g.waitForAll()
        }
        XCTAssertEqual(refreshes.withLock { $0 }, 1)
    }

    func testRejectedRefreshTokenEndsTheSession() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1")) },
            "POST /api/auth/refresh": { _ in .apiError(401, code: "refresh_reused") },
            "GET /api/auth/me": { _ in .apiError(401, code: "token_expired") },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        await XCTAssertThrowsForgeBase(try await fb.auth.me(), kind: .authentication, status: 401)
        let s = await fb.auth.session()
        XCTAssertNil(s)
        let stored = try await env.store.load()
        XCTAssertNil(stored)
    }

    func testTransientRefreshFailureKeepsTheSession() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1", expiresIn: 10)) },
            "POST /api/auth/refresh": { _ in .apiError(500, code: "boom") },
            "GET /api/auth/me": { _ in .json([:]) },
        ])
        let fb = env.client(retry: .disabled)
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        _ = try await fb.auth.me()  // the stale token is still sent
        XCTAssertEqual(env.server.requests.last?.header("Authorization"), "Bearer at_1")
        let s = await fb.auth.session()
        XCTAssertNotNil(s)
    }

    func testSignOutRevokesAndForgets() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON) },
            "POST /api/auth/logout": { _ in .empty() },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        try await fb.auth.signOut()
        XCTAssertEqual(env.server.requests.last?.path, "/api/auth/logout")
        let s = await fb.auth.session()
        XCTAssertNil(s)
        let stored = try await env.store.load()
        XCTAssertNil(stored)
    }

    func testSignOutForgetsLocallyEvenWhenTheServerFails() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON) },
            "POST /api/auth/logout": { _ in .apiError(500, code: "boom") },
        ])
        let fb = env.client(retry: .disabled)
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        await XCTAssertThrowsForgeBase(try await fb.auth.signOut(), kind: .server)
        let s = await fb.auth.session()
        XCTAssertNil(s)
    }

    func testSessionIsRestoredFromTheTokenStore() async throws {
        let env = TestEnv()
        let saved = Session(accessToken: "at_saved", refreshToken: "rt_saved", expiresAt: env.clock.withLock { $0 }.addingTimeInterval(600), sessionId: "s", sessionExpiresAt: "x")
        try await env.store.save(saved)
        env.server.script([.json([:])])
        let fb = env.client()
        _ = try await fb.auth.me()
        XCTAssertEqual(env.server.requests[0].header("Authorization"), "Bearer at_saved")
    }

    func testAPIKeyAndStaticTokenModes() async throws {
        let env = TestEnv()
        env.server.script([.json([:])])
        let key = env.client(apiKey: "fb_live")
        _ = try await key.auth.me()
        XCTAssertEqual(env.server.requests[0].header("X-API-Key"), "fb_live")
        XCTAssertNil(env.server.requests[0].header("Authorization"))

        let bearer = env.client(accessToken: "static")
        _ = try await bearer.auth.me()
        XCTAssertEqual(env.server.requests[1].header("Authorization"), "Bearer static")

        await key.auth.setAPIKey(nil)
        _ = try await key.auth.me()
        XCTAssertNil(env.server.requests[2].header("X-API-Key"))
    }

    func testStateChangesStream() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON) },
            "POST /api/auth/logout": { _ in .empty() },
        ])
        let fb = env.client()
        let changes = await fb.auth.stateChanges()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        try await fb.auth.signOut()
        var seen: [AuthEvent] = []
        for await c in changes {
            seen.append(c.event)
            if seen.count == 2 { break }
        }
        XCTAssertEqual(seen, [.signedIn, .signedOut])
    }

    func testPasswordAndVerificationFlows() async throws {
        let env = TestEnv()
        env.server.script([.empty()])
        let fb = env.client()
        try await fb.auth.requestPasswordReset(email: "a@b.c")
        try await fb.auth.confirmPasswordReset(token: "t", password: "new")
        try await fb.auth.requestEmailVerification(email: "a@b.c")
        try await fb.auth.confirmEmailVerification(token: "t")
        try await fb.auth.requestMagicLink(email: "a@b.c")
        try await fb.auth.requestOTP(email: "a@b.c")
        XCTAssertEqual(
            env.server.requests.map(\.path),
            [
                "/api/auth/password-reset/request", "/api/auth/password-reset/confirm", "/api/auth/verify-email/request",
                "/api/auth/verify-email/confirm", "/api/auth/magic-link/request", "/api/auth/otp/request",
            ])
        XCTAssertEqual(env.server.requests[1].json?["password"], "new")
    }

    func testOTPAndMagicLinkSignIn() async throws {
        let env = TestEnv()
        env.server.script([.json(sessionJSON)])
        let fb = env.client()
        let r = try await fb.auth.verifyOTP(email: "a@b.c", code: "424242")
        XCTAssertNotNil(r.session)
        XCTAssertEqual(env.server.requests[0].path, "/api/auth/otp/verify")
        let m = try await fb.auth.confirmMagicLink(token: "ml")
        XCTAssertNotNil(m.session)
        XCTAssertEqual(env.server.requests[1].json?["token"], "ml")
    }

    // MARK: OAuth

    func testOAuthStartCompleteAndRedirectParsing() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/oauth/github/start": { _ in .json(["authorization_url": "https://github.com/login/oauth/authorize?x=1", "state": "st_1"]) },
            "GET /api/auth/oauth/github/callback": { _ in .json(sessionJSON) },
        ])
        let fb = env.client()
        let start = try await fb.auth.oauth.start(provider: "github", options: .init(deviceName: "Mac"))
        XCTAssertEqual(start.state, "st_1")
        XCTAssertEqual(start.authorizationURL.host, "github.com")
        XCTAssertEqual(env.server.requests[0].json?["device_name"], "Mac")

        let result = try await fb.auth.oauth.complete(provider: "github", redirectURL: URL(string: "myapp://auth/callback?code=c%2B1&state=st_1")!)
        XCTAssertNotNil(result.session)
        XCTAssertEqual(env.server.requests[1].query, "code=c%2B1&state=st_1")
    }

    func testOAuthRedirectErrorsAreTyped() async {
        let env = TestEnv()
        let fb = env.client()
        await XCTAssertThrowsForgeBase(
            try await fb.auth.oauth.complete(provider: "github", redirectURL: URL(string: "myapp://cb?error=access_denied")!), kind: .oauth, code: "oauth_provider_error")
        await XCTAssertThrowsForgeBase(
            try await fb.auth.oauth.complete(provider: "github", redirectURL: URL(string: "myapp://cb")!), kind: .oauth, code: "oauth_missing_code")
    }

    func testOAuthSignInWithABrowserAuthenticator() async throws {
        struct Browser: BrowserAuthenticator {
            let opened = Locked<[URL]>([])
            func authenticate(url: URL, callbackURLScheme: String) async throws -> URL {
                opened.withLock { $0.append(url) }
                XCTAssertEqual(callbackURLScheme, "myapp")
                return URL(string: "myapp://auth/callback?code=abc&state=st_9")!
            }
        }
        let env = TestEnv()
        env.server.routes([
            "POST /api/auth/oauth/google/start": { _ in .json(["authorization_url": "https://accounts.google.com/o/oauth2/v2/auth?state=st_9", "state": "st_9"]) },
            "GET /api/auth/oauth/google/callback": { _ in .json(sessionJSON) },
        ])
        let fb = env.client()
        let browser = Browser()
        let r = try await fb.auth.oauth.signIn(provider: "google", authenticator: browser, callbackURLScheme: "myapp")
        XCTAssertEqual(r.session?.accessToken, "at_1")
        XCTAssertEqual(browser.opened.withLock { $0.count }, 1)
        let stored = try await env.store.load()
        XCTAssertNotNil(stored)
    }

    func testOAuthProvidersAndIdentities() async throws {
        let env = TestEnv()
        env.server.routes([
            "GET /api/auth/oauth/providers": { _ in .json(["providers": [["id": "github"], ["id": "google"]]]) },
            "GET /api/auth/oauth/identities": { _ in .json(["items": []]) },
            "DELETE /api/auth/oauth/identities/i 1": { _ in .empty() },
        ])
        let fb = env.client()
        let providers = try await fb.auth.oauth.providers()
        XCTAssertEqual(providers.compactMap { $0["id"]?.stringValue }, ["github", "google"])
        _ = try await fb.auth.oauth.identities()
    }

    // MARK: token stores

    func testInMemoryTokenStore() async throws {
        let store = InMemoryTokenStore()
        let s = Session(accessToken: "a", refreshToken: "r", expiresAt: Date(timeIntervalSince1970: 1), sessionId: "id", sessionExpiresAt: "x", subject: ["id": "u"])
        try await store.save(s)
        let loaded = try await store.load()
        XCTAssertEqual(loaded, s)
        try await store.clear()
        let cleared = try await store.load()
        XCTAssertNil(cleared)
    }

    func testKeychainTokenStoreRoundTrip() async throws {
        #if canImport(Security)
        let store = KeychainTokenStore(service: "forgebase.tests.\(UUID().uuidString)", account: "t")
        let s = Session(accessToken: "a", refreshToken: "r", expiresAt: Date(timeIntervalSince1970: 100), sessionId: "id", sessionExpiresAt: "x", subject: ["id": "u"], user: ["email": "e"])
        do {
            try await store.save(s)
        } catch {
            throw XCTSkip("Keychain is not available in this environment: \(error)")
        }
        let loaded = try await store.load()
        XCTAssertEqual(loaded, s)
        var s2 = s
        s2.accessToken = "b"
        try await store.save(s2)  // update in place
        let updated = try await store.load()
        XCTAssertEqual(updated?.accessToken, "b")
        try await store.clear()
        let gone = try await store.load()
        XCTAssertNil(gone)
        try await store.clear()  // clearing nothing is fine
        #else
        throw XCTSkip("no Security framework")
        #endif
    }
}
