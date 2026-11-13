import XCTest
@testable import ForgeBase

final class RealtimeTests: XCTestCase {
    private func env(replying: Bool = true) -> TestEnv {
        let env = TestEnv()
        let base = env.sockets.onConnect
        env.sockets.onConnect = { s in
            if replying { s.onSend = { s, m in autoReply(s, m) } }
            base?(s)
        }
        return env
    }

    private func recordEvent(id: String, cursor: Int, recordId: String = "p1", kind: String = "record.created", extra: JSONObject = [:]) -> JSON {
        var o: JSONObject = [
            "type": .string(kind), "id": .string(id), "cursor": .int(Int64(cursor)), "collection": "posts", "record_id": .string(recordId),
            "timestamp": "2026-01-01T00:00:00Z", "data": ["id": .string(recordId), "title": .string("T\(cursor)")],
        ]
        for (k, v) in extra { o[k] = v }
        return .object(o)
    }

    func testSubscribeReceivesRecordEventsAndStoresCursors() async throws {
        let env = env()
        let fb = env.client(apiKey: "fb_key")
        let sub = try await fb.realtime.subscribe(.collection("posts", filter: "published = true"))
        let socket = try XCTUnwrap(env.sockets.sockets.first)
        XCTAssertEqual(socket.url.absoluteString, "wss://fb.test/api/v1/realtime/ws")
        XCTAssertEqual(socket.headers["X-API-Key"], "fb_key", "the credential travels in a header, not in the URL")

        let subscribe = try XCTUnwrap(socket.sent(type: "subscribe").first)
        XCTAssertEqual(subscribe["id"]?.stringValue, sub.id)
        XCTAssertEqual(subscribe["collection"], "posts")
        XCTAssertEqual(subscribe["filter"], "published = true")
        XCTAssertNotNil(subscribe["ref"])
        XCTAssertNil(subscribe["cursor"])

        socket.push(recordEvent(id: sub.id, cursor: 5))
        socket.push(recordEvent(id: sub.id, cursor: 6, kind: "record.updated"))
        var got: [RecordEvent] = []
        for await e in sub.events {
            if case .record(let r) = e { got.append(r) }
            if got.count == 2 { break }
        }
        XCTAssertEqual(got.map(\.kind), [.created, .updated])
        XCTAssertEqual(got[0].collection, "posts")
        XCTAssertEqual(got[1].cursor, 6)
        struct Post: Decodable { let id: String; let title: String }
        XCTAssertEqual(try got[0].record(as: Post.self).title, "T5")
        let cursor = await sub.cursor()
        XCTAssertEqual(cursor, 6)
        await fb.realtime.close()
    }

    func testEventsOfOtherSubscriptionsAreRoutedByID() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        let a = try await fb.realtime.subscribe(.collection("posts"))
        let b = try await fb.realtime.subscribe(.channel("room"))
        let socket = env.sockets.sockets[0]
        socket.push(["type": "message", "id": .string(b.id), "channel": "room", "from": "u2", "data": ["hello": "world"], "timestamp": "t"])
        socket.push(recordEvent(id: a.id, cursor: 1))
        for await e in b.events {
            guard case .message(let m) = e else { XCTFail("wrong event"); break }
            XCTAssertEqual(m.channel, "room")
            XCTAssertEqual(m.data["hello"], "world")
            break
        }
        for await e in a.events {
            guard case .record = e else { XCTFail("wrong event"); break }
            break
        }
        await fb.realtime.close()
    }

    func testReconnectResubscribesFromTheLastCursor() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        let sub = try await fb.realtime.subscribe(.collection("posts"))
        let first = env.sockets.sockets[0]
        first.push(recordEvent(id: sub.id, cursor: 41))
        var it = sub.events.makeAsyncIterator()
        _ = await it.next()

        let events = await fb.realtime.connectionEvents()
        first.serverClose(code: 1006, reason: "dropped")
        await eventually { env.sockets.sockets.count == 2 && !env.sockets.sockets[1].sent(type: "subscribe").isEmpty }
        let second = env.sockets.sockets[1]
        let resub = try XCTUnwrap(second.sent(type: "subscribe").first)
        XCTAssertEqual(resub["id"]?.stringValue, sub.id, "the same subscription id is reused")
        XCTAssertEqual(resub["cursor"], 41, "resume after the last event seen: nothing lost, nothing repeated")

        second.push(recordEvent(id: sub.id, cursor: 42))
        if case .record(let r) = await it.next() { XCTAssertEqual(r.cursor, 42) } else { XCTFail("no event after reconnect") }

        var kinds: [String] = []
        for await e in events {
            switch e {
            case .close(let code, _): kinds.append("close \(code)")
            case .reconnecting: kinds.append("reconnecting")
            case .open: kinds.append("open")
            case .error: break
            }
            if kinds.contains("open") { break }
        }
        XCTAssertEqual(kinds.first, "close 1006")
        await fb.realtime.close()
    }

    func testFailedConnectsBackOffAndEventuallyConnect() async throws {
        let env = env()
        env.sockets.failFirst = 2
        let fb = env.client(apiKey: "k")
        let sub = try await fb.realtime.subscribe(.collection("posts"))
        XCTAssertEqual(env.sockets.sockets.count, 3)
        XCTAssertFalse(sub.id.isEmpty)
        await fb.realtime.close()
    }

    func testGivesUpAfterMaxReconnectAttempts() async {
        let env = env()
        env.sockets.failFirst = 100
        var cfg = env.configuration(apiKey: "k")
        cfg.realtime.maxReconnectAttempts = 2
        let fb = ForgeBaseClient(configuration: cfg)
        await XCTAssertThrowsForgeBase(try await fb.realtime.subscribe(.collection("posts")), kind: .realtime, code: "reconnect_failed")
    }

    func testUnauthenticatedCloseRefreshesTheTokenAndReconnects() async throws {
        let env = env()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1")) },
            "POST /api/auth/refresh": { _ in .json(sessionJSON(access: "at_2", refresh: "rt_2")) },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        _ = try await fb.realtime.subscribe(.collection("posts"))
        XCTAssertEqual(env.sockets.sockets[0].headers["Authorization"], "Bearer at_1")
        env.sockets.sockets[0].serverClose(code: 4001, reason: "token expired")
        await eventually { env.sockets.sockets.count == 2 }
        XCTAssertEqual(env.sockets.sockets[1].headers["Authorization"], "Bearer at_2")
        await fb.realtime.close()
    }

    func testRefreshedTokensAreSentToTheServerOnTheLiveSocket() async throws {
        let env = env()
        env.server.routes([
            "POST /api/auth/login": { _ in .json(sessionJSON(access: "at_1", refresh: "rt_1")) },
            "POST /api/auth/refresh": { _ in .json(sessionJSON(access: "at_9", refresh: "rt_9")) },
        ])
        let fb = env.client()
        _ = try await fb.auth.signIn(email: "a@b.c", password: "pw")
        _ = try await fb.realtime.subscribe(.collection("posts"))
        _ = try await fb.auth.refresh()
        await eventually { !env.sockets.sockets[0].sent(type: "auth").isEmpty }
        XCTAssertEqual(env.sockets.sockets[0].sent(type: "auth")[0]["token"], "at_9")
        await fb.realtime.close()
    }

    func testServerRefusalOfASubscriptionThrows() async {
        let env = env(replying: false)
        env.sockets.onConnect = { s in
            s.push(["type": "welcome", "heartbeat_ms": 3_600_000])
            s.onSend = { s, m in
                if m["type"]?.stringValue == "subscribe" {
                    s.push(["type": "error", "code": "forbidden", "message": "no read rule", "ref": m["ref"] ?? .null])
                }
            }
        }
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.realtime.subscribe(.collection("secrets")), kind: .realtime, code: "forbidden")
        await fb.realtime.close()
    }

    func testDuplicateIDsAndMissingTargetsAreRejected() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        let first = try await fb.realtime.subscribe(SubscribeOptions(id: "dup", collection: "posts"))
        await XCTAssertThrowsForgeBase(try await fb.realtime.subscribe(SubscribeOptions(id: "dup", collection: "posts")), kind: .realtime, code: "duplicate_subscription")
        await XCTAssertThrowsForgeBase(try await fb.realtime.subscribe(SubscribeOptions()), kind: .invalidRequest)
        XCTAssertEqual(first.id, "dup")
        await fb.realtime.close()
    }

    func testUnsubscribeTellsTheServerAndEndsTheStream() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        let sub = try await fb.realtime.subscribe(.collection("posts"))
        await sub.unsubscribe()
        let unsub = try XCTUnwrap(env.sockets.sockets[0].sent(type: "unsubscribe").first)
        XCTAssertEqual(unsub["id"]?.stringValue, sub.id)
        var count = 0
        for await _ in sub.events { count += 1 }
        XCTAssertEqual(count, 0, "the stream finished")
        await fb.realtime.close()
    }

    func testCancellingTheConsumerUnsubscribes() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        let sub = try await fb.realtime.subscribe(.collection("posts"))
        let task = Task { for await _ in sub.events {} }
        try await Task.sleep(nanoseconds: 20_000_000)
        task.cancel()
        await eventually { !env.sockets.sockets[0].sent(type: "unsubscribe").isEmpty }
        await fb.realtime.close()
    }

    func testPublishPresenceAndResetEvents() async throws {
        let env = env()
        env.sockets.onConnect = { s in
            s.push(["type": "welcome", "heartbeat_ms": 3_600_000])
            s.onSend = { s, m in
                if m["type"]?.stringValue == "presence.join" {
                    s.push(["type": "presence.state", "channel": "room", "ref": m["ref"] ?? .null,
                            "members": [["connection_id": "c1", "actor": "user:u1", "meta": ["name": "Ada"], "joined_at": "t"]]])
                } else { autoReply(s, m) }
            }
        }
        let fb = env.client(apiKey: "k")
        let room = try await fb.realtime.subscribe(.channel("room"))
        try await fb.realtime.publish(channel: "room", data: ["hi": 1])
        let publish = try XCTUnwrap(env.sockets.sockets[0].sent(type: "publish").first)
        XCTAssertEqual(publish["channel"], "room")
        XCTAssertEqual(publish["data"]?["hi"], 1)

        let members = try await fb.realtime.joinPresence(channel: "room", meta: ["name": "Ada"])
        XCTAssertEqual(members.map(\.actor), ["user:u1"])
        XCTAssertEqual(members[0].meta?["name"], "Ada")
        XCTAssertEqual(members[0].connectionId, "c1")

        let socket = env.sockets.sockets[0]
        socket.push(["type": "presence.join", "id": .string(room.id), "channel": "room",
                     "member": ["connection_id": "c2", "actor": "user:u2", "joined_at": "t"]])
        socket.push(["type": "reset", "id": .string(room.id), "reason": "cursor_expired", "cursor": 99])
        var seen: [String] = []
        for await e in room.events {
            switch e {
            case .presence(let p): seen.append("presence \(p.change) \(p.member.actor)")
            case .reset(let r): seen.append("reset \(r.reason) \(r.cursor)")
            default: break
            }
            if seen.count == 2 { break }
        }
        XCTAssertEqual(seen, ["presence join user:u2", "reset cursor_expired 99"])
        let cursor = await room.cursor()
        XCTAssertEqual(cursor, 99)
        try await fb.realtime.leavePresence(channel: "room")
        XCTAssertEqual(env.sockets.sockets[0].sent(type: "presence.leave").count, 1)
        await fb.realtime.close()
    }

    func testPublishWithoutASocketFallsBackToREST() async throws {
        let env = env()
        env.server.script([.empty(202)])
        let fb = env.client(apiKey: "k")
        try await fb.realtime.publish(channel: "room 1", data: "x")
        XCTAssertEqual(env.server.requests[0].path, "/api/v1/realtime/channels/room 1/messages")
        XCTAssertEqual(env.server.requests[0].json?["data"], "x")
        XCTAssertTrue(env.sockets.sockets.isEmpty)
    }

    func testPresenceListOverREST() async throws {
        let env = env()
        env.server.script([.json(["members": [["connection_id": "c1", "actor": "user:u1", "joined_at": "t"]]])])
        let fb = env.client(apiKey: "k")
        let m = try await fb.realtime.presence(channel: "room")
        XCTAssertEqual(m.first?.actor, "user:u1")
    }

    func testRecordEventCommitTokenFeedsReadYourWrites() async throws {
        let env = env()
        env.server.script([.json([:])])
        let fb = env.client(apiKey: "k")
        let sub = try await fb.realtime.subscribe(.collection("posts"))
        env.sockets.sockets[0].push(recordEvent(id: sub.id, cursor: 1, extra: ["commit": "control/5/ABC"]))
        for await _ in sub.events { break }
        await eventually { await fb.commitTokens.header() == "control/5/ABC" }
        _ = try await fb.request("GET", "/api/x")
        XCTAssertEqual(env.server.requests.last?.header(readAfterHeader), "control/5/ABC")
        await fb.realtime.close()
    }

    func testHeartbeatPingsAndClosesAStalledConnection() async throws {
        let env = env()
        let sleeps = Locked(0)
        var cfg = env.configuration(apiKey: "k")
        // Each heartbeat sleep advances the fake clock by the interval: 1 s ticks, welcome says 1000 ms.
        let clock = env.clock
        cfg.retry.sleep = { s in
            if s == 1 {
                sleeps.withLock { $0 += 1 }
                clock.withLock { $0 = $0.addingTimeInterval(1) }
                try await Task.sleep(nanoseconds: 5_000_000)
            }
        }
        env.sockets.onConnect = { s in
            s.onSend = { s, m in if m["type"]?.stringValue == "ping" { /* never answers */ } else { autoReply(s, m) } }
            s.push(["type": "welcome", "heartbeat_ms": 1000])
        }
        let fb = ForgeBaseClient(configuration: cfg)
        _ = try await fb.realtime.subscribe(.collection("posts"))
        let first = env.sockets.sockets[0]
        // No traffic: after 2.5 intervals the client gives up on the socket (4002) and reconnects.
        await eventually(5) { first.closedWith?.code == 4002 }
        XCTAssertFalse(first.sent(type: "ping").isEmpty, "it pinged first")
        await eventually { env.sockets.sockets.count >= 2 }
        await fb.realtime.close()
    }

    func testCloseFailsPendingConnectsAndStopsReconnecting() async throws {
        let env = env()
        let fb = env.client(apiKey: "k")
        _ = try await fb.realtime.subscribe(.collection("posts"))
        await fb.realtime.close()
        let state = await fb.realtime.connectionState
        XCTAssertEqual(state, .closed)
        env.sockets.sockets[0].serverClose(code: 1006)
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertEqual(env.sockets.sockets.count, 1, "no reconnect after close")
    }
}
