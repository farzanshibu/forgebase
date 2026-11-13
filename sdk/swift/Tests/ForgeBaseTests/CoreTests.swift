import XCTest
@testable import ForgeBase

final class JSONTests: XCTestCase {
    func testRoundTripKeepsIntegersAndDoubles() throws {
        let j = try JSON(data: Data(#"{"a":1,"b":1.5,"c":"x","d":[true,null],"e":{"f":9007199254740993}}"#.utf8))
        XCTAssertEqual(j["a"], .int(1))
        XCTAssertEqual(j["b"], .double(1.5))
        XCTAssertEqual(j["d"]?[0], .bool(true))
        XCTAssertEqual(j["d"]?[1], .null)
        XCTAssertEqual(j["e"]?["f"]?.int64Value, 9_007_199_254_740_993)
        let again = try JSON(data: try j.data())
        XCTAssertEqual(again, j)
    }

    func testAccessorsAndDynamicMembers() {
        let j: JSON = ["title": "Hi", "n": 3, "tags": ["a", "b"], "nested": ["k": 1.5]]
        XCTAssertEqual(j.title?.stringValue, "Hi")
        XCTAssertEqual(j.n?.intValue, 3)
        XCTAssertEqual(j.tags?.arrayValue?.compactMap(\.stringValue), ["a", "b"])
        XCTAssertEqual(j.nested?.k?.doubleValue, 1.5)
        XCTAssertNil(j.missing)
        XCTAssertEqual(JSON.double(2).intValue, 2)
        XCTAssertNil(JSON.double(2.5).intValue)
    }

    func testEncodeAndDecodeThroughCodable() throws {
        struct Post: Codable, Equatable { var title: String; var views: Int; var draft: Bool? }
        let j = try JSON(encoding: Post(title: "a", views: 2, draft: nil))
        XCTAssertEqual(j["title"], "a")
        XCTAssertEqual(try j.decode(Post.self), Post(title: "a", views: 2, draft: nil))
    }

    func testWireSkipsNilValues() {
        let name: String? = nil
        let size: Int? = 3
        let j = wire(["name": name, "size": size, "tags": ["a"], "flag": true])
        XCTAssertEqual(j, ["size": 3, "tags": ["a"], "flag": true])
    }

    func testDecodeFailureIsInvalidResponse() {
        struct T: Decodable { let x: Int }
        XCTAssertThrowsError(try JSON.object(["x": "no"]).decode(T.self)) {
            XCTAssertEqual(($0 as? ForgeBaseError)?.kind, .invalidResponse)
        }
    }
}

final class ErrorTests: XCTestCase {
    private func http(_ headers: [String: String] = [:]) -> HTTPURLResponse {
        HTTPURLResponse(url: URL(string: "https://x.test")!, statusCode: 400, httpVersion: nil, headerFields: headers)!
    }

    func testStatusMapping() {
        let cases: [(Int, ForgeBaseError.Kind)] = [
            (400, .validation), (422, .validation), (401, .authentication), (403, .permission), (404, .notFound),
            (409, .conflict), (412, .conflict), (429, .rateLimited), (500, .server), (503, .server), (418, .api),
        ]
        for (status, kind) in cases {
            XCTAssertEqual(ForgeBaseError.from(status: status, body: nil, headers: http()).kind, kind, "status \(status)")
        }
    }

    func testServerErrorEnvelope() {
        let body: JSON = [
            "error": [
                "code": "validation_failed", "message": "bad", "request_id": "r-9",
                "details": ["fields": ["title": ["code": "required", "message": "must be set"]]],
            ]
        ]
        let e = ForgeBaseError.from(status: 400, body: body, headers: http())
        XCTAssertEqual(e.code, "validation_failed")
        XCTAssertEqual(e.message, "bad")
        XCTAssertEqual(e.requestId, "r-9")
        XCTAssertEqual(e.fields["title"], FieldError(code: "required", message: "must be set"))
        XCTAssertEqual(e.errorDescription, "bad")
    }

    func testQuotaVersusRateLimitAndFallbacks() {
        XCTAssertEqual(ForgeBaseError.from(status: 429, body: ["error": ["code": "quota_exceeded", "message": "m"]], headers: http()).kind, .quotaExceeded)
        XCTAssertEqual(ForgeBaseError.from(status: 429, body: ["error": ["code": "rate_limited", "message": "m"]], headers: http(["Retry-After": "7"])).retryAfter, 7)
        let e = ForgeBaseError.from(status: 502, body: nil, headers: http(["X-Request-Id": "hdr"]))
        XCTAssertEqual(e.code, "http_error")
        XCTAssertEqual(e.message, "HTTP 502")
        XCTAssertEqual(e.requestId, "hdr")
    }

    func testRetryAfterParsing() {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        XCTAssertEqual(parseRetryAfter("5"), 5)
        XCTAssertEqual(parseRetryAfter("1.5"), 1.5)
        XCTAssertNil(parseRetryAfter("soon"))
        XCTAssertNil(parseRetryAfter(nil))
        // Tue, 14 Nov 2023 22:13:30 GMT is 10 s after `now` (22:13:20 GMT).
        XCTAssertEqual(parseRetryAfter("Tue, 14 Nov 2023 22:13:30 GMT", now: now) ?? -1, 10, accuracy: 0.01)
        XCTAssertEqual(parseRetryAfter("Tue, 14 Nov 2023 22:00:00 GMT", now: now), 0)
    }
}

final class CommitTokenTests: XCTestCase {
    func testKeepsTheHighestPositionPerSet() async {
        let t = CommitTokens()
        await t.note("control/16/B374D848, s1/0/9")
        await t.note("control/16/B374D000")  // older: ignored
        await t.note("s1/1/0")
        let h = await t.header()
        XCTAssertEqual(h, "control/16/B374D848, s1/1/0")
        let held = await t.get("control")
        XCTAssertEqual(held, "16/B374D848")
    }

    func testIgnoresMalformedTokens() async {
        let t = CommitTokens()
        await t.note("nonsense, /1/2, set/xyz/1, set/1, a b/1/2, ok/1/2")
        let h = await t.header()
        XCTAssertEqual(h, "ok/1/2")
        await t.clear()
        let cleared = await t.header()
        XCTAssertNil(cleared)
    }
}

final class SSEParserTests: XCTestCase {
    func parse(_ s: String) -> [SSEEvent] {
        var p = SSEParser()
        return s.utf8.compactMap { p.feed($0) }
    }

    func testEventsAcrossLinesAndCRLF() {
        let evs = parse("event: message\r\ndata: {\"a\":1}\r\nid: 7\r\n\r\n: keep-alive\n\nevent: lagged\ndata: 3\n\ndata: one\ndata: two\n\n")
        XCTAssertEqual(evs.count, 3)
        XCTAssertEqual(evs[0], SSEEvent(event: "message", data: #"{"a":1}"#, id: "7"))
        XCTAssertEqual(evs[1], SSEEvent(event: "lagged", data: "3", id: nil))
        XCTAssertEqual(evs[2], SSEEvent(event: "message", data: "one\ntwo", id: nil))
    }

    func testMultiByteCharactersSplitAcrossChunks() {
        var p = SSEParser()
        let bytes = Array("data: héllo ✓\n\n".utf8)
        var out: [SSEEvent] = []
        for b in bytes { if let e = p.feed(b) { out.append(e) } }
        XCTAssertEqual(out.first?.data, "héllo ✓")
    }
}

final class TransportTests: XCTestCase {
    func testSendsCredentialsHeadersAndEncodesQuery() async throws {
        let env = TestEnv()
        env.server.script([.json(["ok": true])])
        let fb = env.client(apiKey: "fb_key", headers: ["X-Forgebase-Org": "org1"])
        _ = try await fb.request("GET", "/api/x", query: ["q": "a+b c&d", "z": "é"])
        let r = env.server.requests[0]
        XCTAssertEqual(r.header("X-API-Key"), "fb_key")
        XCTAssertEqual(r.header("X-Forgebase-Org"), "org1")
        XCTAssertEqual(r.header("Accept"), "application/json")
        XCTAssertTrue(r.header("User-Agent")?.hasPrefix("forgebase-swift/") == true)
        XCTAssertEqual(r.url.absoluteString, "https://fb.test/api/x?q=a%2Bb%20c%26d&z=%C3%A9")
    }

    func testRetriesOn503HonouringRetryAfter() async throws {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable", headers: ["Retry-After": "2"]), .json(["ok": true])])
        let fb = env.client(apiKey: "k")
        let j = try await fb.request("GET", "/api/x")
        XCTAssertEqual(j["ok"], true)
        XCTAssertEqual(env.server.requests.count, 2)
        XCTAssertEqual(env.sleeps.withLock { $0 }, [2])
    }

    func testBackoffGrowsWithJitterAndStopsAtMaxRetries() async {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable")])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.request("GET", "/api/x"), kind: .server, status: 503)
        XCTAssertEqual(env.server.requests.count, 4)
        // random() == 0.5: 0.5 * 0.25, 0.5 * 0.5, 0.5 * 1
        XCTAssertEqual(env.sleeps.withLock { $0 }, [0.125, 0.25, 0.5])
    }

    func testBadGatewayRetriesOnlyIdempotentMethods() async {
        let env = TestEnv()
        env.server.script([.apiError(502, code: "bad_gateway"), .json(["ok": true])])
        let fb = env.client(apiKey: "k")
        _ = try? await fb.request("GET", "/api/x")
        XCTAssertEqual(env.server.requests.count, 2)
        env.server.script([.apiError(502, code: "bad_gateway"), .json(["ok": true])])
        await XCTAssertThrowsForgeBase(try await fb.request("POST", "/api/x", body: ["a": 1]), kind: .server, status: 502)
        XCTAssertEqual(env.server.requests.count, 3)
    }

    func testLongRetryAfterFailsAtOnce() async {
        let env = TestEnv()
        env.server.script([.apiError(429, code: "rate_limited", headers: ["Retry-After": "120"])])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.request("GET", "/api/x"), kind: .rateLimited, status: 429)
        XCTAssertEqual(env.server.requests.count, 1)
        XCTAssertTrue(env.sleeps.withLock { $0 }.isEmpty)
    }

    func testQuotaErrorsAreNeverRetried() async {
        let env = TestEnv()
        env.server.script([.apiError(429, code: "quota_exceeded")])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.request("GET", "/api/x"), kind: .quotaExceeded)
        XCTAssertEqual(env.server.requests.count, 1)
    }

    func testNetworkErrorsRetryForIdempotentCallsOnly() async throws {
        let env = TestEnv()
        env.server.script([MockResponse(error: URLError(.networkConnectionLost)), .json(["ok": true])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.request("GET", "/api/x")
        XCTAssertEqual(env.server.requests.count, 2)

        env.server.script([MockResponse(error: URLError(.networkConnectionLost)), .json(["ok": true])])
        await XCTAssertThrowsForgeBase(try await fb.request("POST", "/api/x", body: ["a": 1]), kind: .network, status: 0, code: "network_error")
        XCTAssertEqual(env.server.requests.count, 3)
    }

    func testCommitTokensAndStrongConsistency() async throws {
        let env = TestEnv()
        env.server.script([.json(["ok": true], headers: [commitHeader: "control/1/FF"]), .json(["ok": true])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.request("POST", "/api/w", body: [:])
        _ = try await fb.request("GET", "/api/r")
        XCTAssertNil(env.server.requests[0].header(readAfterHeader))
        XCTAssertEqual(env.server.requests[1].header(readAfterHeader), "control/1/FF")
        XCTAssertNil(env.server.requests[1].header(consistencyHeader))

        let strong = env.client(apiKey: "k", consistency: .strong)
        env.server.script([.json([:])])
        _ = try await strong.request("GET", "/api/r")
        XCTAssertEqual(env.server.requests.last?.header(consistencyHeader), "strong")
    }

    func testFailedWriteStillRecordsItsCommitToken() async {
        let env = TestEnv()
        env.server.script([.apiError(500, code: "partial", headers: [commitHeader: "s/2/3"]), .json([:])])
        let fb = env.client(apiKey: "k", retry: .disabled)
        _ = try? await fb.request("POST", "/api/w", body: [:])
        let h = await fb.commitTokens.header()
        XCTAssertEqual(h, "s/2/3")
    }

    func testEmptyAndInvalidBodies() async throws {
        let env = TestEnv()
        env.server.script([.empty(204), .text("not json"), .apiError(404, code: "not_found")])
        let fb = env.client(apiKey: "k")
        let none = try await fb.request("DELETE", "/api/x")
        XCTAssertEqual(none, .null)
        await XCTAssertThrowsForgeBase(try await fb.request("GET", "/api/x"), kind: .invalidResponse, code: "invalid_response")
        await XCTAssertThrowsForgeBase(try await fb.request("GET", "/api/x"), kind: .notFound, status: 404, code: "not_found")
    }

    func testHealth() async {
        let env = TestEnv()
        env.server.script([.text("ok"), .apiError(503, code: "down")])
        let fb = env.client()
        let up = await fb.health()
        XCTAssertTrue(up)
        let down = await fb.health()
        XCTAssertFalse(down)
        XCTAssertEqual(env.server.requests.count, 2, "health does not retry")
    }

    func testCancellationSurfacesAsCancellationError() async {
        let env = TestEnv()
        env.server.script([MockResponse(error: URLError(.cancelled))])
        let fb = env.client(apiKey: "k")
        do {
            _ = try await fb.request("GET", "/api/x")
            XCTFail("expected cancellation")
        } catch is CancellationError {
        } catch {
            XCTFail("unexpected \(error)")
        }
    }
}
