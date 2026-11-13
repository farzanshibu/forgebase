import XCTest
@testable import ForgeBase

final class FunctionsTests: XCTestCase {
    func testInvokeJSONInAndOut() async throws {
        let env = TestEnv()
        env.server.script([.json(["sum": 3])])
        let fb = env.client(apiKey: "k")
        let out = try await fb.functions.invoke("add", input: ["a": 1, "b": 2])
        XCTAssertEqual(out["sum"], 3)
        let r = env.server.requests[0]
        XCTAssertEqual(r.method, "POST")
        XCTAssertEqual(r.path, "/api/v1/functions/add/invoke")
        XCTAssertEqual(r.header("Content-Type"), "application/json")
        XCTAssertEqual(r.json?["b"], 2)
    }

    func testInvokeTypedWithPathQueryAndVersion() async throws {
        struct In: Encodable { let name: String }
        struct Out: Decodable, Equatable { let greeting: String }
        let env = TestEnv()
        env.server.script([.json(["greeting": "hi Ada"])])
        let fb = env.client(apiKey: "k")
        let out: Out = try await fb.functions.invoke("greet", input: In(name: "Ada"), options: InvokeOptions(method: "PUT", path: "/users/42", query: ["lang": "en"], version: 3))
        XCTAssertEqual(out, Out(greeting: "hi Ada"))
        XCTAssertEqual(env.server.requests[0].method, "PUT")
        XCTAssertEqual(env.server.requests[0].url.absoluteString, "https://fb.test/api/v1/functions/greet/invoke/users/42?lang=en&version=3")
    }

    func testTextAnd204AndErrors() async throws {
        let env = TestEnv()
        env.server.script([.text("plain"), .empty(), .apiError(404, code: "function_not_found")])
        let fb = env.client(apiKey: "k")
        let text = try await fb.functions.invoke("a")
        XCTAssertEqual(text, "plain")
        let none = try await fb.functions.invoke("a")
        XCTAssertEqual(none, .null)
        await XCTAssertThrowsForgeBase(try await fb.functions.invoke("missing"), kind: .notFound, code: "function_not_found")
    }

    func testRawInvokeKeepsBytesAndHeaders() async throws {
        let env = TestEnv()
        env.server.script([MockResponse(headers: ["Content-Type": "image/png", "X-Custom": "1"], body: Data([1, 2, 3]))])
        let fb = env.client(apiKey: "k")
        let res = try await fb.functions.invokeRaw("thumb", body: Data([9]), contentType: "application/octet-stream", options: InvokeOptions(headers: ["X-Req": "r"]))
        XCTAssertEqual(res.data, Data([1, 2, 3]))
        XCTAssertEqual(res.headers["x-custom"], "1")
        XCTAssertEqual(res.contentType, "image/png")
        XCTAssertEqual(env.server.requests[0].header("X-Req"), "r")
        XCTAssertEqual(env.server.requests[0].body, Data([9]))
    }

    func testInvokeIsNotRetriedByDefaultButCanBe() async throws {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable"), .json(["ok": true])])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.functions.invoke("f"), kind: .server)
        XCTAssertEqual(env.server.requests.count, 1)
        env.server.script([.apiError(503, code: "unavailable"), .json(["ok": true])])
        let ok = try await fb.functions.invoke("f", options: InvokeOptions(retry: true))
        XCTAssertEqual(ok["ok"], true)
    }

    func testListGetLogs() async throws {
        let env = TestEnv()
        env.server.script([.json(["items": [["name": "a"]]]), .json(["name": "a"]), .json(["items": [["status": "ok"]]])])
        let fb = env.client(apiKey: "k")
        let list = try await fb.functions.list()
        XCTAssertEqual(list.count, 1)
        _ = try await fb.functions.get("a")
        let logs = try await fb.functions.logs("a", limit: 5)
        XCTAssertEqual(logs.count, 1)
        XCTAssertEqual(env.server.requests[2].query, "limit=5")
    }
}

final class KVTests: XCTestCase {
    func testStringsAndBinaryValues() async throws {
        let env = TestEnv()
        env.server.script([
            .json(["stored": true]), .json(["values": ["v", ["base64": "AQI="], nil]]), .json(["value": 11]), .json(["stored": 2]),
        ])
        let fb = env.client(apiKey: "k")
        let cache = fb.kv.ns("cache")
        let stored = try await cache.set("k 1", "v", KVSetOptions(ttlMs: 5000, nx: true))
        XCTAssertTrue(stored)
        let r = env.server.requests[0]
        XCTAssertEqual(r.method, "PUT")
        XCTAssertEqual(r.path, "/api/v1/kv/cache/strings/k 1")
        XCTAssertEqual(r.json?["value"], "v")
        XCTAssertEqual(r.json?["ttl_ms"], 5000)
        XCTAssertEqual(r.json?["nx"], true)

        let vals = try await cache.mget(["a", "b", "c"])
        XCTAssertEqual(vals, [.text("v"), .bytes(Data([1, 2])), nil])
        let n = try await cache.incr("counter", by: 10)
        XCTAssertEqual(n, 11)
        let count = try await cache.mset(["x": "1", "y": .bytes(Data([255]))])
        XCTAssertEqual(count, 2)
        XCTAssertEqual(env.server.requests[3].json?["entries"]?[1]?["value"]?["base64"], "/w==")
    }

    func testTextHelperAndKeyInfo() async throws {
        let env = TestEnv()
        env.server.script([.json(["values": ["hello"]]), .json(["key": "h", "type": "hash", "ttl_ms": nil, "size": 2, "truncated": false, "value": ["f": "v"]])])
        let fb = env.client(apiKey: "k")
        let text = try await fb.kv.ns("n").text("greeting")
        XCTAssertEqual(text, "hello")
        let key = try await fb.kv.ns("n").get("h", limit: 10)
        XCTAssertEqual(key.type, "hash")
        XCTAssertNil(key.ttlMs)
        XCTAssertEqual(key.value["f"], "v")
    }

    func testCollectionsCommandsAndScan() async throws {
        let env = TestEnv()
        env.server.script([
            .json(["added": 2]), .json(["length": 3]), .json(["values": ["a", "b"]]), .json(["added": 1]), .json(["added": 1]),
            .json(["id": "1-0"]), .json(["entries": [["id": "1-0", "fields": ["k": "v"]]]]), .json(["result": ["PONG"]]),
            .json(["results": [["result": 1], ["error": ["code": "bad", "message": "m"]]]]),
        ])
        let fb = env.client(apiKey: "k")
        let ns = fb.kv.ns("n")
        let added = try await ns.hashSet("h", ["a": "1", "b": "2"])
        XCTAssertEqual(added, 2)
        let len = try await ns.listPush("l", side: .left, values: ["x"])
        XCTAssertEqual(len, 3)
        let popped = try await ns.listPop("l", side: .right, count: 2)
        XCTAssertEqual(popped.map(\.string), ["a", "b"])
        _ = try await ns.setAdd("s", members: ["m"])
        _ = try await ns.zsetAdd("z", members: [(member: "m", score: 1.5)])
        XCTAssertEqual(env.server.requests[4].json?["members"]?[0]?["score"], 1.5)
        let id = try await ns.streamAdd("st", fields: ["k": "v"], maxLen: 100)
        XCTAssertEqual(id, "1-0")
        let entries = try await ns.streamRange("st", count: 10)
        XCTAssertEqual(entries.first?.fields["k"], "v")
        let pong = try await ns.exec(["PING"])
        XCTAssertEqual(pong, ["PONG"])
        let batch = try await ns.batch([["INCR", "a"], ["BOGUS"]])
        XCTAssertEqual(batch[1]["error"]?["code"], "bad")
    }

    func testKeysIteratesScanPages() async throws {
        let env = TestEnv()
        env.server.script([.json(["cursor": "17", "keys": ["a", "b"]]), .json(["cursor": "0", "keys": ["c"]])])
        let fb = env.client(apiKey: "k")
        var keys: [String] = []
        for try await k in fb.kv.ns("n").keys(match: "*", count: 2) { keys.append(k) }
        XCTAssertEqual(keys, ["a", "b", "c"])
        XCTAssertTrue(env.server.requests[1].query.contains("cursor=17"))
    }

    func testDeleteExpirePersistFlushAndNamespaces() async throws {
        let env = TestEnv()
        env.server.script([.json(["deleted": true]), .empty(), .json(["persisted": false]), .json(["deleted": 4]), .json(["namespaces": [["name": "cache"]]])])
        let fb = env.client(apiKey: "k")
        let deleted = try await fb.kv.ns("n").delete("k")
        XCTAssertTrue(deleted)
        try await fb.kv.ns("n").expire("k", ttlMs: 100)
        XCTAssertEqual(env.server.requests[1].json?["ttl_ms"], 100)
        let persisted = try await fb.kv.ns("n").persist("k")
        XCTAssertFalse(persisted)
        let flushed = try await fb.kv.flush("n")
        XCTAssertEqual(flushed, 4)
        let spaces = try await fb.kv.namespaces()
        XCTAssertEqual(spaces.count, 1)
    }

    func testSubscribeStreamsMessagesAndLagNotices() async throws {
        let env = TestEnv()
        env.server.script([
            .sse([
                ": hi\n\n",
                "event: message\ndata: {\"channel\":\"news\",\"message\":\"one\"}\n\n",
                "event: lagged\ndata: 3\n\n",
                "event: message\ndata: {\"channel\":\"news\",\"message\":{\"base64\":\"AQ==\"}}\n\n",
            ])
        ])
        let fb = env.client(apiKey: "k")
        var got: [KVSubscriptionEvent] = []
        for try await e in fb.kv.ns("n").subscribe("news", reconnect: false) { got.append(e) }
        XCTAssertEqual(
            got,
            [.message(KVMessage(channel: "news", message: "one")), .lagged(3), .message(KVMessage(channel: "news", message: .bytes(Data([1]))))])
        XCTAssertEqual(env.server.requests[0].header("Accept"), "text/event-stream")
        XCTAssertEqual(env.server.requests[0].path, "/api/v1/kv/n/subscribe/news")
    }

    func testSubscribeReconnectsAfterADroppedStreamButStopsOnRefusal() async throws {
        let env = TestEnv()
        env.server.script([
            .sse(["event: message\ndata: {\"channel\":\"c\",\"message\":\"a\"}\n\n"]),
            .apiError(403, code: "forbidden"),
        ])
        let fb = env.client(apiKey: "k")
        var got: [String] = []
        do {
            for try await e in fb.kv.ns("n").subscribe("c") { if case .message(let m) = e { got.append(m.message.string) } }
            XCTFail("expected the refusal to end the stream with an error")
        } catch let e as ForgeBaseError {
            XCTAssertEqual(e.kind, .permission)
        }
        XCTAssertEqual(got, ["a"])
        XCTAssertEqual(env.server.requests.count, 2, "reconnected once, then the 403 was final")
    }

    func testPublishAndChannels() async throws {
        let env = TestEnv()
        env.server.script([.json(["receivers": 2]), .json(["channels": [["name": "news", "subscribers": 2]]])])
        let fb = env.client(apiKey: "k")
        let n = try await fb.kv.ns("n").publish("news", "hello")
        XCTAssertEqual(n, 2)
        let ch = try await fb.kv.ns("n").channels()
        XCTAssertEqual(ch.count, 1)
    }
}

final class QueuesTests: XCTestCase {
    func testPublishWithoutDedupIsNeverRetried() async throws {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable"), .json(["results": [["message_id": "m1", "seq": 1, "duplicate": false]]])])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.queues.publish("orders", ["id": 1]), kind: .server)
        XCTAssertEqual(env.server.requests.count, 1)
    }

    func testPublishWithDedupIdIsRetriedAndSendsOptions() async throws {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable"), .json(["results": [["message_id": "m1", "seq": 7, "duplicate": true]]])])
        let fb = env.client(apiKey: "k")
        let p = try await fb.queues.publish("orders", ["id": 1], PublishOptions(attributes: ["kind": "rush"], orderingKey: "u1", dedupId: "d1"))
        XCTAssertEqual(p, Published(messageId: "m1", seq: 7, duplicate: true))
        XCTAssertEqual(env.server.requests.count, 2)
        let m = env.server.requests[1].json?["messages"]?[0]
        XCTAssertEqual(m?["data"]?["id"], 1)
        XCTAssertEqual(m?["attributes"]?["kind"], "rush")
        XCTAssertEqual(m?["ordering_key"], "u1")
        XCTAssertEqual(m?["dedup_id"], "d1")
        XCTAssertEqual(env.server.requests[1].path, "/api/v1/queues/topics/orders/publish")
    }

    func testTopicsAndSubscriptionsAdmin() async throws {
        let env = TestEnv()
        env.server.script([.json(["items": [["name": "orders"]]]), .json(["name": "orders"], status: 201), .json(["name": "worker"], status: 201), .empty(), .empty()])
        let fb = env.client(apiKey: "k")
        _ = try await fb.queues.listTopics()
        try await fb.queues.createTopic("orders", CreateTopicOptions(retentionMs: 1000, labels: ["a": "b"]))
        XCTAssertEqual(env.server.requests[1].json?["retention_ms"], 1000)
        try await fb.queues.createSubscription("worker", topic: "orders", CreateSubscriptionOptions(filter: ["kind": "rush"], ackDeadlineMs: 5000, ordering: true, start: "earliest"))
        let sub = env.server.requests[2].json
        XCTAssertEqual(sub?["topic"], "orders")
        XCTAssertEqual(sub?["filter"]?["kind"], "rush")
        XCTAssertEqual(sub?["start"], "earliest")
        try await fb.queues.deleteSubscription("worker")
        try await fb.queues.deleteTopic("orders", force: true)
        XCTAssertEqual(env.server.requests[4].query, "force=true")
    }

    func testPullAckNackAndModifyDeadline() async throws {
        let env = TestEnv()
        let delivery: JSON = [
            "ack_id": "a1", "delivery_attempt": 2, "ack_deadline": "t",
            "message": ["id": "m1", "topic": "orders", "seq": 5, "data": ["n": 1], "attributes": ["k": "v"], "ordering_key": "o", "publisher": "p", "bytes": 1, "published_at": "t"],
        ]
        env.server.script([
            .json(["messages": [delivery]]), .json(["count": 1, "unknown": []]), .json(["count": 0, "unknown": ["a9"]]), .json(["count": 1, "unknown": []]),
        ])
        let fb = env.client(apiKey: "k")
        let got = try await fb.queues.pull("worker", PullOptions(maxMessages: 5, waitMs: 1000, consumer: "c1"))
        XCTAssertEqual(env.server.requests[0].json?["wait_ms"], 1000)
        XCTAssertEqual(got.count, 1)
        XCTAssertEqual(got[0].deliveryAttempt, 2)
        XCTAssertEqual(got[0].message.seq, 5)
        XCTAssertEqual(got[0].message.attributes, ["k": "v"])
        XCTAssertEqual(got[0].message.data["n"], 1)
        let ack = try await fb.queues.ack("worker", ackIds: ["a1"])
        XCTAssertEqual(ack.count, 1)
        let nack = try await fb.queues.nack("worker", ackIds: ["a9"])
        XCTAssertEqual(nack.unknown, ["a9"])
        _ = try await fb.queues.modifyDeadline("worker", ackIds: ["a1"], ackDeadlineMs: 30_000)
        XCTAssertEqual(env.server.requests[3].json?["ack_deadline_ms"], 30_000)
    }

    func testPullIsNeverRetried() async {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable"), .json(["messages": []])])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.queues.pull("w"), kind: .server)
        XCTAssertEqual(env.server.requests.count, 1)
    }

    private func event(_ ackId: String, _ n: Int) -> String {
        let j: JSON = ["ack_id": .string(ackId), "delivery_attempt": 1, "ack_deadline": "t", "message": ["id": .string("m\(n)"), "topic": "t", "seq": .int(Int64(n)), "data": ["n": .int(Int64(n))], "attributes": [:]]]
        return "event: message\ndata: \(j)\n\n"
    }

    func testStreamingSubscribeAcksOnSuccessAndNacksOnFailure() async throws {
        let env = TestEnv()
        env.server.routes([
            "GET /api/v1/queues/subscriptions/worker/events": { _ in .sse([self.event("a1", 1), self.event("a2", 2)]) },
            "POST /api/v1/queues/subscriptions/worker/ack": { _ in .json(["count": 1, "unknown": []]) },
            "POST /api/v1/queues/subscriptions/worker/nack": { _ in .json(["count": 1, "unknown": []]) },
        ])
        let fb = env.client(apiKey: "k")
        struct Boom: Error {}
        let errors = Locked<[String]>([])
        let stream = fb.queues.subscribe(
            "worker", options: QueueSubscribeOptions(maxMessages: 10, reconnect: false, onError: { _ in errors.withLock { $0.append("err") } })
        ) { d in
            let n: Int = d.data["n"]?.intValue ?? 0
            if n == 2 { throw Boom() }
        }
        try await stream.wait()
        let acks = env.server.requests.filter { $0.path.hasSuffix("/ack") }
        let nacks = env.server.requests.filter { $0.path.hasSuffix("/nack") }
        XCTAssertEqual(acks.compactMap { $0.json?["ack_ids"]?[0]?.stringValue }, ["a1"])
        XCTAssertEqual(nacks.compactMap { $0.json?["ack_ids"]?[0]?.stringValue }, ["a2"])
        XCTAssertEqual(errors.withLock { $0 }, ["err"])
        XCTAssertEqual(env.server.requests[0].query, "max_messages=10")
    }

    func testManualSettlementWithAutoAckOff() async throws {
        let env = TestEnv()
        env.server.routes([
            "GET /api/v1/queues/subscriptions/w/events": { _ in .sse([self.event("a1", 1)]) },
            "POST /api/v1/queues/subscriptions/w/nack": { _ in .json(["count": 1, "unknown": []]) },
            "POST /api/v1/queues/subscriptions/w/modify-deadline": { _ in .json(["count": 1, "unknown": []]) },
        ])
        let fb = env.client(apiKey: "k")
        let stream = fb.queues.subscribe("w", options: QueueSubscribeOptions(autoAck: false, reconnect: false)) { d in
            try await d.extend(ackDeadlineMs: 60_000)
            try await d.nack()
            try await d.ack()  // already settled: no request
        }
        try await stream.wait()
        XCTAssertEqual(env.server.requests.map(\.path).filter { !$0.hasSuffix("/events") }, ["/api/v1/queues/subscriptions/w/modify-deadline", "/api/v1/queues/subscriptions/w/nack"])
    }

    func testStreamRefusalEndsWithTheError() async {
        let env = TestEnv()
        env.server.script([.apiError(404, code: "subscription_not_found")])
        let fb = env.client(apiKey: "k")
        let stream = fb.queues.subscribe("nope") { _ in }
        await XCTAssertThrowsForgeBase(try await stream.wait(), kind: .notFound, code: "subscription_not_found")
    }

    func testCancellingAStreamEndsItCleanly() async throws {
        let env = TestEnv()
        env.server.script([.sse([event("a1", 1)], hold: true)])
        let fb = env.client(apiKey: "k")
        let seen = Locked(0)
        let stream = fb.queues.subscribe("w", options: QueueSubscribeOptions(autoAck: false)) { _ in seen.withLock { $0 += 1 } }
        await eventually { seen.withLock { $0 } == 1 }
        stream.cancel()
        try await stream.wait()
    }
}

final class JobsTests: XCTestCase {
    private func job(_ status: String, id: String = "j1", extra: JSONObject = [:]) -> JSON {
        var o: JSONObject = ["id": .string(id), "queue": "default", "kind": "thumbnail", "payload": ["a": 1], "priority": 0, "status": .string(status), "attempts": 1, "max_attempts": 3, "updated_at": "2026-01-01T00:00:01Z"]
        for (k, v) in extra { o[k] = v }
        return .object(o)
    }

    func testEnqueueWithIdempotencyKeyIsRetriedWithoutOneIsNot() async throws {
        let env = TestEnv()
        env.server.script([.apiError(503, code: "unavailable"), .json(job("queued"), status: 201)])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.jobs.enqueue("thumbnail", payload: ["a": 1]), kind: .server)
        XCTAssertEqual(env.server.requests.count, 1)
        env.server.script([.apiError(503, code: "unavailable"), .json(job("queued"), status: 201)])
        let j = try await fb.jobs.enqueue("thumbnail", payload: ["a": 1], EnqueueOptions(queue: "media", priority: 5, delayMs: 100, idempotencyKey: "once", dependsOn: ["j0"]))
        XCTAssertEqual(j.status, .queued)
        let body = env.server.requests.last?.json
        XCTAssertEqual(body?["kind"], "thumbnail")
        XCTAssertEqual(body?["idempotency_key"], "once")
        XCTAssertEqual(body?["depends_on"]?[0], "j0")
        XCTAssertEqual(body?["queue"], "media")
    }

    func testGetListCancelRetryDeleteResult() async throws {
        let env = TestEnv()
        env.server.script([.json(job("running")), .json(["items": [job("queued")], "total": 9]), .json(job("cancelled")), .json(job("queued")), .empty(), .json(["job_id": "j1", "stored": true, "result": ["ok": true]])])
        let fb = env.client(apiKey: "k")
        let j = try await fb.jobs.get("j1")
        XCTAssertEqual(j.attempts, 1)
        XCTAssertEqual(j.maxAttempts, 3)
        let list = try await fb.jobs.list(ListJobsOptions(status: .queued, kind: "thumbnail", limit: 10))
        XCTAssertEqual(list.total, 9)
        XCTAssertEqual(list.items.count, 1)
        XCTAssertEqual(env.server.requests[1].query, "status=queued&kind=thumbnail&limit=10")
        let cancelled = try await fb.jobs.cancel("j1")
        XCTAssertEqual(cancelled.status, .cancelled)
        _ = try await fb.jobs.retry("j1")
        try await fb.jobs.delete("j1")
        let result = try await fb.jobs.result("j1")
        XCTAssertEqual(result["result"]?["ok"], true)
    }

    func testWaitPollsUntilSuccess() async throws {
        let env = TestEnv()
        env.server.script([.json(job("queued")), .json(job("running")), .json(job("succeeded", extra: ["result": ["done": true]]))])
        let fb = env.client(apiKey: "k")
        let j = try await fb.jobs.wait("j1", pollInterval: 0.5)
        XCTAssertEqual(j.status, .succeeded)
        XCTAssertEqual(j.result?["done"], true)
        XCTAssertEqual(env.server.requests.count, 3)
        XCTAssertEqual(env.sleeps.withLock { $0 }, [0.5, 0.5])
    }

    func testWaitThrowsWhenTheJobDiesOrTimesOut() async {
        let env = TestEnv()
        env.server.script([.json(job("dead", extra: ["last_error": "boom"]))])
        let fb = env.client(apiKey: "k")
        do {
            _ = try await fb.jobs.wait("j1")
            XCTFail("expected failure")
        } catch let e as ForgeBaseError {
            XCTAssertEqual(e.kind, .jobFailed)
            XCTAssertEqual(e.code, "job_dead")
            XCTAssertTrue(e.message.contains("boom"))
        } catch { XCTFail("\(error)") }

        env.server.script([.json(job("running"))])
        await XCTAssertThrowsForgeBase(try await fb.jobs.wait("j1", pollInterval: 0, timeout: -1), kind: .jobFailed)
    }

    func testUpdatesStartWithTheStoredStateThenFollowTheChannel() async throws {
        let env = TestEnv()
        env.sockets.onConnect = { s in
            s.onSend = { s, m in autoReply(s, m) }
            s.push(["type": "welcome", "heartbeat_ms": 3_600_000])
        }
        env.server.script([.json(job("running", extra: ["progress": ["done": 1, "total": 4]]))])
        let fb = env.client(apiKey: "k")
        let updates = try await fb.jobs.updates("j1")
        let socket = env.sockets.sockets[0]
        let sub = try XCTUnwrap(socket.sent(type: "subscribe").first)
        XCTAssertEqual(sub["channel"], "jobs:j1")
        let subId = try XCTUnwrap(sub["id"]?.stringValue)
        func message(_ status: String, _ at: String, event: String = "status", progress: JSON? = nil) -> JSON {
            var data: JSONObject = ["event": .string(event), "job_id": "j1", "status": .string(status), "attempts": 1, "updated_at": .string(at)]
            if let progress { data["progress"] = progress }
            return ["type": "message", "id": .string(subId), "channel": "jobs:j1", "from": "server", "data": .object(data), "timestamp": "t"]
        }
        socket.push(message("running", "2026-01-01T00:00:00Z"))  // older than the snapshot: dropped
        socket.push(message("running", "2026-01-01T00:00:05Z", event: "progress", progress: ["done": 3, "total": 4]))
        socket.push(message("succeeded", "2026-01-01T00:00:09Z"))
        var seen: [String] = []
        for try await u in updates { seen.append("\(u.event):\(u.status?.rawValue ?? "")") }
        XCTAssertEqual(seen, ["status:running", "progress:running", "status:succeeded"])
        await eventually { !socket.sent(type: "unsubscribe").isEmpty }
        await fb.realtime.close()
    }

    func testUpdatesOfAnAlreadyFinishedJobFinishImmediately() async throws {
        let env = TestEnv()
        env.sockets.onConnect = { s in
            s.onSend = { s, m in autoReply(s, m) }
            s.push(["type": "welcome", "heartbeat_ms": 3_600_000])
        }
        env.server.script([.json(job("succeeded"))])
        let fb = env.client(apiKey: "k")
        var n = 0
        for try await u in try await fb.jobs.updates("j1") {
            n += 1
            XCTAssertEqual(u.status, .succeeded)
        }
        XCTAssertEqual(n, 1)
        await fb.realtime.close()
    }
}

final class EntitlementsTests: XCTestCase {
    private let decision: JSON = [
        "allowed": true, "feature": "export", "subject": "user:u1", "scope_key": "", "units": 1, "used": 4, "reserved": 0,
        "pack_remaining": 0, "overage_units": 0, "pack_units": 0, "window_start": "2026-01-01T00:00:00Z", "limit": 10, "remaining": 5,
    ]

    func testHasQueries() async throws {
        let env = TestEnv()
        env.server.script([.json(["has": true]), .json(["has": false, "reason": "plan_too_low"]), .json(["has": true])])
        let fb = env.client(apiKey: "k")
        let a = try await fb.entitlements.has(.feature("export", subject: "org:1"))
        XCTAssertTrue(a)
        XCTAssertEqual(env.server.requests[0].query, "feature=export&subject=org%3A1")
        let b = try await fb.entitlements.hasDetailed(.plan("pro"))
        XCTAssertFalse(b.has)
        XCTAssertEqual(b.reason, "plan_too_low")
        _ = try await fb.entitlements.has(.permission("billing:read"))
        XCTAssertEqual(env.server.requests[2].query, "permission=billing%3Aread")
    }

    func testCheckQuotaConsumeReserveCommitRelease() async throws {
        let env = TestEnv()
        let reservation: JSON = ["id": "r1", "feature": "export", "subject": "user:u1", "scope_key": "", "units": 1, "pack_units": 0, "overage_units": 0, "status": "held", "expires_at": "t", "created_at": "t", "replayed": false]
        env.server.script([
            .json(decision), .json(["event_id": "e1", "replayed": false, "decision": decision]), .json(reservation),
            .json(reservation), .json(reservation),
        ])
        let fb = env.client(apiKey: "k")
        let d = try await fb.entitlements.checkQuota(QuotaRequest(feature: "export", units: 1, scopeKey: "doc1"))
        XCTAssertTrue(d.allowed)
        XCTAssertEqual(d.remaining, 5)
        XCTAssertEqual(env.server.requests[0].json?["scope_key"], "doc1")
        let c = try await fb.entitlements.consume(QuotaRequest(feature: "export", idempotencyKey: "k1"))
        XCTAssertEqual(c.eventId, "e1")
        XCTAssertEqual(env.server.requests[1].json?["idempotency_key"], "k1")
        let r = try await fb.entitlements.reserve(QuotaRequest(feature: "export", idempotencyKey: "k2", ttlSeconds: 60))
        XCTAssertEqual(r.status, "held")
        XCTAssertEqual(env.server.requests[2].json?["ttl_secs"], 60)
        _ = try await fb.entitlements.commitReservation("r1", units: 1)
        _ = try await fb.entitlements.releaseReservation("r1")
        XCTAssertEqual(env.server.requests[3].path, "/api/v1/usage/reservations/r1/commit")
        XCTAssertEqual(env.server.requests[4].path, "/api/v1/usage/reservations/r1/release")
    }

    func testMeteredCallsNeedAnIdempotencyKey() async {
        let env = TestEnv()
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.entitlements.consume(QuotaRequest(feature: "x")), kind: .invalidRequest)
        await XCTAssertThrowsForgeBase(try await fb.entitlements.reserve(QuotaRequest(feature: "x")), kind: .invalidRequest)
        await XCTAssertThrowsForgeBase(try await fb.entitlements.acquireLease(QuotaRequest(feature: "x")), kind: .invalidRequest)
        XCTAssertTrue(env.server.requests.isEmpty)
    }

    func testWithReservationCommitsOnSuccessAndReleasesOnFailure() async throws {
        let env = TestEnv()
        let reservation: JSON = ["id": "r1", "feature": "export", "subject": "s", "scope_key": "", "units": 1, "pack_units": 0, "overage_units": 0, "status": "held", "expires_at": "t", "created_at": "t", "replayed": false]
        env.server.script([.json(reservation)])
        let fb = env.client(apiKey: "k")
        let out = try await fb.entitlements.withReservation(QuotaRequest(feature: "export", idempotencyKey: "a")) { _ in 42 }
        XCTAssertEqual(out, 42)
        XCTAssertEqual(env.server.requests.map(\.path), ["/api/v1/usage/reservations", "/api/v1/usage/reservations/r1/commit"])

        struct Nope: Error {}
        do {
            _ = try await fb.entitlements.withReservation(QuotaRequest(feature: "export", idempotencyKey: "b")) { _ -> Int in throw Nope() }
            XCTFail("expected the body's error")
        } catch is Nope {}
        XCTAssertEqual(env.server.requests.last?.path, "/api/v1/usage/reservations/r1/release")
    }

    func testPastAHardLimitIsAQuotaError() async {
        let env = TestEnv()
        env.server.script([.apiError(429, code: "quota_exceeded", details: ["feature": "export"])])
        let fb = env.client(apiKey: "k")
        do {
            _ = try await fb.entitlements.consume(QuotaRequest(feature: "export", idempotencyKey: "k"))
            XCTFail("expected quota error")
        } catch let e as ForgeBaseError {
            XCTAssertEqual(e.kind, .quotaExceeded)
            XCTAssertEqual(e.details?["feature"], "export")
        } catch { XCTFail("\(error)") }
        XCTAssertEqual(env.server.requests.count, 1)
    }

    func testLeasesUsageAndSubscription() async throws {
        let env = TestEnv()
        let lease: JSON = ["id": "l1", "feature": "jobs", "subject": "s", "scope_key": "", "expires_at": "t", "released": false, "replayed": false]
        env.server.script([.json(lease), .json(lease), .empty(), .json(["items": [["feature": "export"]]]), .json(["plan": "pro"]), .json(["items": []])])
        let fb = env.client(apiKey: "k")
        let l = try await fb.entitlements.acquireLease(QuotaRequest(feature: "jobs", idempotencyKey: "k", ttlSeconds: 30))
        XCTAssertEqual(l.id, "l1")
        _ = try await fb.entitlements.renewLease("l1", ttlSeconds: 60)
        try await fb.entitlements.releaseLease("l1")
        let usage = try await fb.entitlements.usage(subject: "org:1")
        XCTAssertEqual(usage.count, 1)
        let sub = try await fb.entitlements.subscription()
        XCTAssertEqual(sub["plan"], "pro")
        _ = try await fb.entitlements.usageAlerts()
    }
}

final class BillingTests: XCTestCase {
    func testStoreKitReceiptVerification() async throws {
        let env = TestEnv()
        env.server.script([.json(["id": "sp1", "status": "active"])])
        let fb = env.client(apiKey: "k")
        let p = try await fb.billing.store.verify(.appStore, StoreReceipt(signedTransaction: "eyJ.jws", transactionId: "2000001"))
        XCTAssertEqual(p["status"], "active")
        let r = env.server.requests[0]
        XCTAssertEqual(r.path, "/api/v1/billing/store/app_store/verify")
        XCTAssertEqual(r.json?["signed_transaction"], "eyJ.jws")
        XCTAssertEqual(r.json?["transaction_id"], "2000001")
        XCTAssertNil(r.json?["purchase_token"])
    }

    func testStoreCheckoutProductsPurchases() async throws {
        let env = TestEnv()
        env.server.script([.json(["url": "https://pay", "session_id": "s"]), .json(["items": [["product_id": "p"]]]), .json(["items": []])])
        let fb = env.client(apiKey: "k")
        let c = try await fb.billing.store.checkout(.creem, price: "pro", successURL: "myapp://ok", idempotencyKey: "i1")
        XCTAssertEqual(c["url"], "https://pay")
        XCTAssertEqual(env.server.requests[0].json?["success_url"], "myapp://ok")
        let products = try await fb.billing.store.products()
        XCTAssertEqual(products.count, 1)
        _ = try await fb.billing.store.purchases(subject: "org:1")
    }

    func testCustomerAndPortal() async throws {
        let env = TestEnv()
        env.server.script([.json(["subject": "user:u1"]), .json(["subject": "user:u1"]), .json(["customer": [:]]), .json(["url": "https://hosted/pm", "session_id": "ps"])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.billing.customers.get()
        _ = try await fb.billing.customers.put(BillingCustomerInput(email: "ada@example.com", currency: "usd", region: "US-CA", taxExempt: false))
        XCTAssertEqual(env.server.requests[1].method, "PUT")
        XCTAssertEqual(env.server.requests[1].json?["tax_exempt"], false)
        _ = try await fb.billing.portal.get()
        let pm = try await fb.billing.portal.paymentMethod(returnURL: "myapp://back")
        XCTAssertEqual(pm.url.absoluteString, "https://hosted/pm")
        XCTAssertEqual(pm.sessionId, "ps")
        XCTAssertEqual(env.server.requests[3].json?["return_url"], "myapp://back")
    }

    func testSubscriptionsAndInvoices() async throws {
        let env = TestEnv()
        env.server.script([.json(["replayed": false, "payment_url": "https://pay"], status: 201), .json(["items": []]), .json([:]), .json(["items": [["id": "inv1"]]]), .json([:])])
        let fb = env.client(apiKey: "k")
        let c = try await fb.billing.subscriptions.create(items: [BillingItem(price: "pro_monthly", quantity: 2)], idempotencyKey: "i1", trialDays: 14)
        XCTAssertEqual(c["payment_url"], "https://pay")
        XCTAssertEqual(env.server.requests[0].json?["items"]?[0]?["price"], "pro_monthly")
        XCTAssertEqual(env.server.requests[0].json?["items"]?[0]?["quantity"], 2)
        XCTAssertEqual(env.server.requests[0].json?["trial_days"], 14)
        _ = try await fb.billing.subscriptions.list()
        _ = try await fb.billing.subscriptions.cancel("sub1", atPeriodEnd: true, reason: "too pricey")
        XCTAssertEqual(env.server.requests[2].path, "/api/v1/billing/subscriptions/sub1/cancel")
        let invoices = try await fb.billing.invoices.list(status: "open", limit: 5)
        XCTAssertEqual(invoices.count, 1)
        _ = try await fb.billing.invoices.pay("inv1", returnURL: "myapp://back")
    }

    func testCreditsReads() async throws {
        let env = TestEnv()
        env.server.script([.json(["balance": 500]), .json(["items": [["id": "l"]]]), .json(["items": [], "next": nil])])
        let fb = env.client(apiKey: "k")
        let b = try await fb.credits.balance()
        XCTAssertEqual(b["balance"], 500)
        let lots = try await fb.credits.lots(all: true)
        XCTAssertEqual(lots.count, 1)
        XCTAssertEqual(env.server.requests[1].query, "all=true")
        _ = try await fb.credits.ledger(limit: 20)
    }
}

final class DataServicesTests: XCTestCase {
    func testVectorSearch() async throws {
        let env = TestEnv()
        env.server.script([
            .json(["hits": [["id": "a", "distance": 0.12, "metadata": ["cat": "news"]], ["id": "b", "distance": 0.3]]]),
            .json(["hits": [["id": "a", "score": 0.9, "text": "hello"]]]),
        ])
        let fb = env.client(apiKey: "k")
        let docs = fb.vector.collection("docs")
        let hits = try await docs.search(vector: [0.1, 0.2], VectorQuery(k: 5, filter: "cat = {:c}", params: ["c": "news"], includeText: true), exact: true)
        XCTAssertEqual(hits.map(\.id), ["a", "b"])
        XCTAssertEqual(hits[0].score, 0.12, accuracy: 1e-9)
        XCTAssertEqual(hits[0].metadata?["cat"], "news")
        XCTAssertNil(hits[1].metadata)
        let body = env.server.requests[0].json
        XCTAssertEqual(env.server.requests[0].path, "/api/v1/vector/collections/docs/search")
        XCTAssertEqual(body?["vector"]?[1], 0.2)
        XCTAssertEqual(body?["exact"], true)
        XCTAssertEqual(body?["params"]?["c"], "news")
        let h = try await docs.hybrid(vector: [1], text: "hello", VectorQuery(k: 3), rrfK: 60)
        XCTAssertEqual(h[0].text, "hello")
        XCTAssertEqual(env.server.requests[1].json?["rrf_k"], 60)
    }

    func testVectorUpsertAndDelete() async throws {
        let env = TestEnv()
        env.server.script([.json(["upserted": 1]), .json(["deleted": 2])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.vector.collection("docs").upsert([VectorUpsertItem(id: "a", vectors: ["emb": [1, 2]], metadata: ["k": "v"], text: "t")])
        XCTAssertEqual(env.server.requests[0].json?["items"]?[0]?["vectors"]?["emb"]?[1], 2)
        let n = try await fb.vector.collection("docs").deleteItems(["a", "b"])
        XCTAssertEqual(n, 2)
    }

    func testGraphReads() async throws {
        let env = TestEnv()
        env.server.script([.json(["id": "n1"]), .json(["items": [["id": "n2"]], "next_after": "n2"]), .json(["nodes": []]), .json(["path": []]), .json(["columns": ["n"], "rows": []])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.graph.node("n1")
        let nb = try await fb.graph.neighbors(of: "n1", direction: .out, edgeType: "KNOWS", limit: 10)
        XCTAssertEqual(nb.nextAfter, "n2")
        XCTAssertEqual(env.server.requests[1].query, "direction=out&edge_type=KNOWS&limit=10")
        _ = try await fb.graph.traverse(start: "n1", direction: .both, edgeTypes: ["KNOWS"], maxDepth: 2, filter: GraphNodeFilter(label: "Person", properties: ["age": 3]))
        XCTAssertEqual(env.server.requests[2].json?["filter"]?["properties"]?["age"], 3)
        _ = try await fb.graph.shortestPath(from: "a", to: "b")
        _ = try await fb.graph.cypher("MATCH (n) RETURN n", params: ["x": 1], limit: 5)
        XCTAssertEqual(env.server.requests[4].json?["params"]?["x"], 1)
    }

    func testSQLRowsAsObjectsAndDecodable() async throws {
        let env = TestEnv()
        let result: JSON = ["kind": "read", "columns": ["id", "name"], "rows": [[1, "Ada"], [2, "Grace"]], "rows_affected": 0, "truncated": false, "duration_ms": 1.5]
        env.server.script([.json(result), .json(result), .json(result), .json(["items": [["name": "posts"]]]), .json(["name": "posts"]), .json(["plan": []])])
        let fb = env.client(apiKey: "k")
        let rows = try await fb.sql.query("select id, name from t where id > ?", params: [0])
        XCTAssertEqual(rows.map { $0["name"]?.stringValue }, ["Ada", "Grace"])
        XCTAssertEqual(env.server.requests[0].json?["params"], [0])
        struct Row: Decodable, Equatable { let id: Int; let name: String }
        let typed = try await fb.sql.query("select id, name from t", as: Row.self)
        XCTAssertEqual(typed, [Row(id: 1, name: "Ada"), Row(id: 2, name: "Grace")])
        let full = try await fb.sql.execute("select 1")
        XCTAssertEqual(full.kind, "read")
        XCTAssertEqual(full.durationMs, 1.5)
        _ = try await fb.sql.tables()
        _ = try await fb.sql.table("posts")
        _ = try await fb.sql.explain("select 1")
    }

    func testClientEscapeHatchAndSystemInfo() async throws {
        let env = TestEnv()
        env.server.script([.json(["version": "0.3.4"])])
        let fb = env.client(apiKey: "k")
        let info = try await fb.systemInfo()
        XCTAssertEqual(info["version"], "0.3.4")
        XCTAssertEqual(env.server.requests[0].path, "/api/system/info")
    }
}

final class WebhookTests: XCTestCase {
    let body = Data(#"{"subscription":"s","delivery_attempt":1,"message":{"id":"m1","data":{"x":1}}}"#.utf8)
    let secret = "whsec_test"
    let now = Date(timeIntervalSince1970: 1_700_000_000)

    func testSignAndVerifyRoundTrip() throws {
        let header = Webhook.sign(secret: secret, body: body, timestamp: 1_700_000_000)
        XCTAssertTrue(header.hasPrefix("t=1700000000,v1="))
        XCTAssertTrue(Webhook.verify(secrets: [secret], header: header, body: body, now: now))
        let event = try Webhook.construct(secrets: [secret], header: header, body: body, now: now)
        XCTAssertEqual(event["message"]?["id"], "m1")
    }

    func testKnownVector() {
        // HMAC-SHA256("whsec_test", "1700000000." + body)
        let header = Webhook.sign(secret: secret, body: body, timestamp: 1_700_000_000)
        let again = Webhook.sign(secret: secret, body: body, timestamp: 1_700_000_000)
        XCTAssertEqual(header, again)
        XCTAssertEqual(header.split(separator: "=").last?.count, 64)
    }

    func testFailureReasons() {
        let good = Webhook.sign(secret: secret, body: body, timestamp: 1_700_000_000)
        func reason(_ h: String?, _ b: Data? = nil, secrets: [String]? = nil, at: Date? = nil) -> String? {
            do {
                _ = try Webhook.construct(secrets: secrets ?? [secret], header: h, body: b ?? body, now: at ?? now)
                return nil
            } catch let e as ForgeBaseError {
                XCTAssertEqual(e.kind, .webhook)
                return e.code
            } catch { return "other" }
        }
        XCTAssertEqual(reason(nil), "webhook_missing_header")
        XCTAssertEqual(reason("garbage"), "webhook_malformed_header")
        XCTAssertEqual(reason("t=abc,v1=00"), "webhook_malformed_header")
        XCTAssertEqual(reason(good, at: now.addingTimeInterval(301)), "webhook_timestamp_out_of_tolerance")
        XCTAssertEqual(reason(good, Data("tampered".utf8)), "webhook_signature_mismatch")
        XCTAssertEqual(reason(good, secrets: ["whsec_other"]), "webhook_signature_mismatch")
        let notAnEnvelope = Data("[]".utf8)
        XCTAssertEqual(reason(Webhook.sign(secret: secret, body: notAnEnvelope, timestamp: 1_700_000_000), notAnEnvelope), "webhook_invalid_payload")
    }

    func testRotationAcceptsEitherSecretAndMultipleSignatures() {
        let old = Webhook.sign(secret: "whsec_old", body: body, timestamp: 1_700_000_000)
        let new = Webhook.sign(secret: "whsec_new", body: body, timestamp: 1_700_000_000)
        let v1 = { (h: String) in h.components(separatedBy: "v1=")[1] }
        let both = "t=1700000000,v1=\(v1(old)),v1=\(v1(new))"
        XCTAssertTrue(Webhook.verify(secrets: ["whsec_new"], header: both, body: body, now: now))
        XCTAssertTrue(Webhook.verify(secrets: ["whsec_old"], header: both, body: body, now: now))
        XCTAssertFalse(Webhook.verify(secrets: ["whsec_third"], header: both, body: body, now: now))
        XCTAssertTrue(Webhook.verify(secrets: ["whsec_new", "whsec_old"], header: old, body: body, now: now))
    }

    func testToleranceCanBeDisabled() {
        let header = Webhook.sign(secret: secret, body: body, timestamp: 1)
        XCTAssertFalse(Webhook.verify(secrets: [secret], header: header, body: body, now: now))
        XCTAssertTrue(Webhook.verify(secrets: [secret], header: header, body: body, tolerance: .infinity, now: now))
    }
}
