package dev.forgebase

import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.take
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.RecordedRequest
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

private fun sse(vararg events: String): MockResponse =
    MockResponse().setHeader("Content-Type", "text/event-stream").setBody(events.joinToString("") { it + "\n\n" })

class FunctionsTest : ServerTest() {
    @Test
    fun `invoke sends json and parses a json answer`() = blocking {
        server.enqueue(json("""{"greeting":"hi bob"}"""))
        val r = client().functions.invoke("hello", mapOf("name" to "bob"), InvokeOptions(path = "/users/42", query = mapOf("v" to 1), headers = mapOf("X-T" to "1")))
        assertEquals("hi bob", r.jsonObject["greeting"]?.jsonPrimitive?.content)
        val req = next()
        assertEquals("POST", req.method)
        assertEquals("/api/v1/functions/hello/invoke/users/42?v=1", req.path)
        assertTrue(req.getHeader("Content-Type")!!.startsWith("application/json"))
        assertEquals("bob", req.jsonBody()["name"]?.jsonPrimitive?.content)
        assertEquals("1", req.getHeader("X-T"))
    }

    @Test
    fun `strings go out as text, text answers come back as strings, 204 is null`() = blocking {
        server.enqueue(MockResponse().setHeader("Content-Type", "text/plain").setBody("pong"))
        server.enqueue(MockResponse().setResponseCode(204))
        val fn = client().functions
        assertEquals(JsonPrimitive("pong"), fn.invoke("echo", "ping", InvokeOptions(method = "PUT")))
        assertEquals(JsonNull, fn.invoke("noop"))
        val a = next()
        assertEquals("PUT", a.method)
        assertTrue(a.getHeader("Content-Type")!!.startsWith("text/plain"))
        assertEquals("ping", a.body.readUtf8())
    }

    @Test
    fun `typed invoke, query version and error mapping`() = blocking {
        server.enqueue(json("""{"id":"1","title":"t"}"""))
        server.enqueue(json(errorBody("function_not_found", "no such function"), 404))
        val fn = client().functions
        assertEquals("t", fn.invokeAs<Post>("make", null, InvokeOptions(version = 3)).title)
        assertEquals("/api/v1/functions/make/invoke?version=3", next().path)
        assertFailsWith<NotFoundException> { fn.invoke("ghost") }
    }
}

class KvTest : ServerTest() {
    @Test
    fun `strings, ttl and counters`() = blocking {
        server.enqueue(json("""{"stored":true}"""))
        server.enqueue(json("""{"values":["v",null,{"base64":"AQI="}]}"""))
        server.enqueue(json("""{"value":3}"""))
        server.enqueue(json("""{"deleted":true}"""))
        val cache = client().kv.ns("cache")
        assertTrue(cache.set("k", "v", KvSetOptions(ttlMs = 60_000, nx = true)))
        val set = next()
        assertEquals("PUT", set.method)
        assertEquals("/api/v1/kv/cache/strings/k", set.path)
        val body = set.jsonBody()
        assertEquals("v", body["value"]?.jsonPrimitive?.content)
        assertEquals(60000, body["ttl_ms"]?.jsonPrimitive?.content?.toInt())
        assertEquals(true, body["nx"]?.jsonPrimitive?.content?.toBoolean())
        val vals = cache.mget(listOf("a", "b", "c"))
        assertEquals("v", vals[0]?.toText())
        assertNull(vals[1])
        assertEquals(listOf<Byte>(1, 2), (vals[2] as KvBin.Bytes).value.toList())
        assertEquals(3L, cache.incr("n", 2))
        assertTrue(cache.delete("k"))
    }

    @Test
    fun `bytes are sent as base64 and numbers in decimal`() = blocking {
        server.enqueue(json("""{"stored":true}"""))
        server.enqueue(json("""{"added":1}"""))
        val ns = client().kv.ns("n")
        ns.set("k", byteArrayOf(1, 2))
        ns.hashSet("h", mapOf("f" to 5, "g" to "x"))
        assertEquals("AQI=", next().jsonBody()["value"]!!.jsonObject["base64"]?.jsonPrimitive?.content)
        val fields = next().jsonBody()["fields"]!!.jsonObject
        assertEquals("5", fields["f"]?.jsonPrimitive?.content)
    }

    @Test
    fun `scan pages and keys flow`() = blocking {
        server.enqueue(json("""{"cursor":"7","keys":["a","b"]}"""))
        server.enqueue(json("""{"cursor":"0","keys":["c"]}"""))
        assertEquals(listOf("a", "b", "c"), client().kv.ns("n").keys(KvScanOptions(match = "*", count = 2)).toList())
        next()
        assertEquals("7", next().requestUrl!!.queryParameter("cursor"))
    }

    @Test
    fun `exec and batch`() = blocking {
        server.enqueue(json("""{"result":["a",1,null]}"""))
        server.enqueue(json("""{"results":[{"result":"OK"},{"error":{"code":"wrong_type","message":"nope"}}]}"""))
        val ns = client().kv.ns("n")
        assertEquals(3, ns.exec(listOf("ZRANGE", "board", 0, -1)).jsonArray.size)
        assertEquals(listOf("ZRANGE", "board", "0", "-1"), next().jsonBody()["command"]!!.jsonArray.map { it.jsonPrimitive.content })
        val res = ns.batch(listOf(listOf("SET", "a", 1), listOf("LPUSH", "a", 2)))
        assertEquals("OK", res[0].result?.jsonPrimitive?.content)
        assertTrue(res[1].isError)
        assertEquals("wrong_type", res[1].errorCode)
    }

    @Test
    fun `streams and sorted sets`() = blocking {
        server.enqueue(json("""{"id":"1-0"}"""))
        server.enqueue(json("""{"entries":[{"id":"1-0","fields":{"a":"1"}}]}"""))
        server.enqueue(json("""{"added":1}"""))
        val ns = client().kv.ns("n")
        assertEquals("1-0", ns.streamAdd("s", mapOf("a" to 1), maxLen = 100))
        assertEquals("1", ns.streamRange("s").single().fields["a"]?.toText())
        assertEquals(1L, ns.zsetAdd("z", listOf("m" to 1.5)))
    }

    @Test
    fun `subscribe streams messages and reports lag`() = blocking {
        server.enqueue(
            sse(
                """event: message
data: {"channel":"c","message":"one"}""",
                "event: lagged\ndata: 4",
                """event: message
data: {"channel":"c","message":{"base64":"AQI="}}""",
            ),
        )
        val lag = AtomicInteger()
        val msgs = client().kv.ns("n").subscribe("c", reconnect = false, onLagged = { lag.addAndGet(it.toInt()) }).toList()
        assertEquals(2, msgs.size)
        assertEquals("one", msgs[0].message.toText())
        assertIs<KvBin.Bytes>(msgs[1].message)
        assertEquals(4, lag.get())
        assertEquals("text/event-stream", next().getHeader("Accept"))
    }

    @Test
    fun `subscribe fails for good on a refusal and reconnects after a drop`() = blocking {
        server.enqueue(json(errorBody("forbidden", "no"), 403))
        assertFailsWith<PermissionException> { client().kv.ns("n").subscribe("c").toList() }

        val n = AtomicInteger()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse =
                if (n.getAndIncrement() == 0) sse("""event: message
data: {"channel":"c","message":"a"}""") else sse("""event: message
data: {"channel":"c","message":"b"}""")
        }
        val msgs = client().kv.ns("n").subscribe("c").take(2).toList()
        assertEquals(listOf("a", "b"), msgs.map { it.message.toText() })
    }
}

class QueuesTest : ServerTest() {
    private fun msg(seq: Int, data: String = "1") =
        """{"id":"m$seq","topic":"orders","seq":$seq,"data":$data,"attributes":{"kind":"x"},"publisher":"u","bytes":1,"published_at":"t"}"""

    @Test
    fun `publish returns the id and sequence, with attributes and dedup`() = blocking {
        server.enqueue(json("""{"results":[{"message_id":"m1","seq":1,"duplicate":false}]}"""))
        val p = client().queues.publish("orders", mapOf("order" to 42), PublishOptions(attributes = mapOf("kind" to "created"), orderingKey = "o42", dedupId = "d1"))
        assertEquals("m1", p.messageId)
        assertEquals(1L, p.seq)
        val m = next().jsonBody()["messages"]!!.jsonArray.single().jsonObject
        assertEquals(42, m["data"]!!.jsonObject["order"]?.jsonPrimitive?.content?.toInt())
        assertEquals("o42", m["ordering_key"]?.jsonPrimitive?.content)
        assertEquals("d1", m["dedup_id"]?.jsonPrimitive?.content)
    }

    @Test
    fun `publish is retried only with dedup ids`() = blocking {
        server.enqueue(MockResponse().setSocketPolicy(okhttp3.mockwebserver.SocketPolicy.DISCONNECT_AT_START))
        assertFailsWith<NetworkException> { client().queues.publish("orders", 1) }
        assertEquals(1, server.requestCount)
        server.enqueue(MockResponse().setSocketPolicy(okhttp3.mockwebserver.SocketPolicy.DISCONNECT_AT_START))
        server.enqueue(json("""{"results":[{"message_id":"m1","seq":1,"duplicate":true}]}"""))
        // POST is not idempotent for the transport, so even with a dedup id a dropped connection is not replayed blindly
        assertFailsWith<NetworkException> { client().queues.publish("orders", 1, PublishOptions(dedupId = "d")) }
    }

    @Test
    fun `pull is not retried, ack and nack carry ack ids`() = blocking {
        server.enqueue(json("""{"messages":[{"ack_id":"a1","delivery_attempt":2,"ack_deadline":"t","message":${msg(1, """{"order":1}""")}}]}"""))
        server.enqueue(json("""{"count":1,"unknown":[]}"""))
        server.enqueue(json("""{"count":0,"unknown":["a2"]}"""))
        val q = client().queues
        val got = q.pull("worker", PullOptions(maxMessages = 5, waitMs = 1000, consumer = "c1"))
        assertEquals(2, got.single().deliveryAttempt)
        assertEquals(1, got.single().data<Map<String, Int>>()["order"])
        assertEquals(1, q.ack("worker", listOf("a1")).count)
        assertEquals(listOf("a2"), q.nack("worker", listOf("a2")).unknown)
        val pull = next().jsonBody()
        assertEquals("5", pull["max_messages"]?.jsonPrimitive?.content)
        assertEquals("c1", pull["consumer"]?.jsonPrimitive?.content)
        assertEquals(listOf("a1"), next().jsonBody()["ack_ids"]!!.jsonArray.map { it.jsonPrimitive.content })
    }

    @Test
    fun `streaming subscribe acks on success and nacks on failure`() = blocking {
        val settled = java.util.concurrent.CopyOnWriteArrayList<String>()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = when {
                request.path!!.contains("/events") -> sse(
                    """data: {"ack_id":"a1","delivery_attempt":1,"ack_deadline":"t","message":${msg(1, "\"ok\"")}}""",
                    """data: {"ack_id":"a2","delivery_attempt":1,"ack_deadline":"t","message":${msg(2, "\"bad\"")}}""",
                )
                request.path!!.endsWith("/ack") -> json("""{"count":1,"unknown":[]}""").also { settled.add("ack:" + request.jsonBody()["ack_ids"]!!.jsonArray[0].jsonPrimitive.content) }
                request.path!!.endsWith("/nack") -> json("""{"count":1,"unknown":[]}""").also { settled.add("nack:" + request.jsonBody()["ack_ids"]!!.jsonArray[0].jsonPrimitive.content) }
                else -> MockResponse().setResponseCode(404)
            }
        }
        val errors = java.util.concurrent.CopyOnWriteArrayList<Throwable>()
        val seen = java.util.concurrent.CopyOnWriteArrayList<String>()
        client().queues.subscribe("worker", QueueStreamOptions(reconnect = false, onError = { errors.add(it) })) { d ->
            seen.add(d.decode<String>())
            if (d.decode<String>() == "bad") error("handler failed")
        }
        withTimeoutPoll { settled.size == 2 }
        assertEquals(setOf("ack:a1", "nack:a2"), settled.toSet())
        assertEquals(listOf("ok", "bad"), seen.sorted().reversed())
        assertEquals(1, errors.size)
    }

    @Test
    fun `manual settle with autoAck off, extend and refusals`() = blocking {
        val paths = java.util.concurrent.CopyOnWriteArrayList<String>()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                paths.add(request.path!!)
                return if (request.path!!.contains("/events")) sse("""data: {"ack_id":"a1","delivery_attempt":1,"ack_deadline":"t","message":${msg(1)}}""")
                else json("""{"count":1,"unknown":[]}""")
            }
        }
        client().queues.subscribe("worker", QueueStreamOptions(autoAck = false, reconnect = false)) { d ->
            d.extend(5000)
            d.ack()
            d.ack() // settling twice is a no-op
        }
        withTimeoutPoll { paths.any { it.endsWith("/ack") } }
        assertEquals(1, paths.count { it.endsWith("/ack") })
        assertTrue(paths.any { it.endsWith("/modify-deadline") })

        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = json(errorBody("forbidden", "push subscription"), 403)
        }
        assertFailsWith<PermissionException> { client().queues.subscribe("push-sub") { } }
    }

    @Test
    fun `topic and subscription administration`() = blocking {
        server.enqueue(json("""{"name":"orders","retention_ms":1}"""))
        server.enqueue(json("""{"name":"worker","topic":"orders"}"""))
        val q = client().queues
        assertEquals("orders", q.createTopic("orders", CreateTopicOptions(retentionMs = 1000, labels = mapOf("a" to "b")))["name"]?.jsonPrimitive?.content)
        q.createSubscription("worker", "orders", CreateSubscriptionOptions(ackDeadlineMs = 5000, ordering = true))
        val t = next().jsonBody()
        assertEquals("1000", t["retention_ms"]?.jsonPrimitive?.content)
        val s = next().jsonBody()
        assertEquals("true", s["ordering"]?.jsonPrimitive?.content)
        assertEquals("5000", s["ack_deadline_ms"]?.jsonPrimitive?.content)
    }
}

/** Waits (up to 5 s) for a condition that another thread makes true. */
private suspend fun withTimeoutPoll(cond: () -> Boolean) {
    val end = System.currentTimeMillis() + 5000
    while (!cond()) {
        if (System.currentTimeMillis() > end) error("condition not met in time")
        delay(10)
    }
}

class JobsTest : ServerTest() {
    private fun job(status: String, id: String = "j1", extra: String = "") =
        """{"id":"$id","queue":"default","kind":"report","payload":{"a":1},"priority":0,"status":"$status","attempts":1,"max_attempts":3,"updated_at":"2026-01-01T00:00:0${if (status == "running") 1 else 2}Z"$extra}"""

    @Test
    fun `enqueue sends the options and is only retried with an idempotency key`() = blocking {
        server.enqueue(json(job("queued")))
        val j = client().jobs.enqueue("report", mapOf("a" to 1), EnqueueOptions(queue = "slow", delayMs = 1000, idempotencyKey = "k1", dependsOn = listOf("j0")))
        assertEquals(JobStatus.QUEUED, j.state)
        val b = next().jsonBody()
        assertEquals("report", b["kind"]?.jsonPrimitive?.content)
        assertEquals("slow", b["queue"]?.jsonPrimitive?.content)
        assertEquals("k1", b["idempotency_key"]?.jsonPrimitive?.content)
        server.enqueue(MockResponse().setSocketPolicy(okhttp3.mockwebserver.SocketPolicy.DISCONNECT_AT_START))
        assertFailsWith<NetworkException> { client().jobs.enqueue("report") }
    }

    @Test
    fun `await polls until success, and throws for dead, cancelled and timeouts`() = blocking {
        server.enqueue(json(job("running")))
        server.enqueue(json(job("succeeded", extra = ""","result":{"ok":true}""")))
        assertEquals(JobStatus.SUCCEEDED, client().jobs.await("j1", pollMs = 5).state)

        server.enqueue(json(job("dead", extra = ""","last_error":"boom"""")))
        val e = assertFailsWith<JobFailedException> { client().jobs.await("j1", pollMs = 5) }
        assertTrue(e.message!!.contains("boom"))
        assertEquals("job_failed", e.code)

        repeat(50) { server.enqueue(json(job("running"))) }
        assertFailsWith<JobFailedException> { client().jobs.await("j1", pollMs = 5, timeoutMs = 30) }
    }

    @Test
    fun `list, cancel, retry and result`() = blocking {
        server.enqueue(json("""{"items":[${job("queued")}],"total":7}"""))
        server.enqueue(json(job("cancelled")))
        server.enqueue(json("""{"job_id":"j1","stored":true,"result":{"x":1}}"""))
        val jobs = client().jobs
        val page = jobs.list(ListJobsOptions(status = JobStatus.QUEUED, queue = "default", limit = 5))
        assertEquals(7L, page.total)
        assertEquals("queued", next().requestUrl!!.queryParameter("status"))
        assertEquals(JobStatus.CANCELLED, jobs.cancel("j1").state)
        assertEquals("/api/v1/jobs/j1/cancel", next().path)
        assertEquals(true, jobs.result("j1")["stored"]?.jsonPrimitive?.content?.toBoolean())
    }
}

class BusinessApisTest : ServerTest() {
    @Test
    fun `entitlement checks`() = blocking {
        server.enqueue(json("""{"has":true}"""))
        server.enqueue(json("""{"has":false,"reason":"plan"}"""))
        server.enqueue(json("""{"entitled":true,"subject":"user:1","plan":"pro","status":"active","features":{"seats":{"limit":5}}}"""))
        val e = client().entitlements
        assertTrue(e.hasFeature("export", subject = "org:1"))
        assertEquals(false, e.hasPlan("enterprise"))
        val set = e.get()
        assertEquals("pro", set.plan)
        assertTrue("seats" in set.features)
        val first = next().requestUrl!!
        assertEquals("export", first.queryParameter("feature"))
        assertEquals("org:1", first.queryParameter("subject"))
        assertEquals("enterprise", next().requestUrl!!.queryParameter("plan"))
    }

    @Test
    fun `quota calls and reservations`() = blocking {
        val decision = """{"allowed":true,"feature":"api","units":1,"used":3,"limit":10,"remaining":7,"scope_key":"","subject":"user:1","window_start":"t"}"""
        server.enqueue(json(decision))
        server.enqueue(json("""{"decision":$decision,"event_id":"e1","replayed":false}"""))
        server.enqueue(json("""{"id":"r1","feature":"api","status":"held","units":2}"""))
        server.enqueue(json("""{"id":"r1","feature":"api","status":"committed","units":2}"""))
        server.enqueue(json("""{"id":"r2","feature":"api","status":"held","units":1}"""))
        server.enqueue(json("""{"id":"r2","feature":"api","status":"released","units":1}"""))
        val e = client().entitlements
        assertEquals(7L, e.checkQuota(QuotaRequest("api")).remaining)
        assertEquals("e1", e.consume(QuotaRequest("api", units = 1, scopeKey = "u1"), "idem-1").eventId)
        e.withReservation(QuotaRequest("api", units = 2), "idem-2") { }
        assertFailsWith<IllegalStateException> { runBlockingReservation(e) }
        next()
        val consume = next().jsonBody()
        assertEquals("idem-1", consume["idempotency_key"]?.jsonPrimitive?.content)
        assertEquals("u1", consume["scope_key"]?.jsonPrimitive?.content)
        assertEquals("/api/v1/usage/reservations", next().path)
        assertEquals("/api/v1/usage/reservations/r1/commit", next().path)
        assertEquals("/api/v1/usage/reservations", next().path)
        assertEquals("/api/v1/usage/reservations/r2/release", next().path)
    }

    private suspend fun runBlockingReservation(e: EntitlementsApi) {
        e.withReservation(QuotaRequest("api"), "idem-3") { error("work failed") }
    }

    @Test
    fun `exhausted quota is a QuotaExceededException`() = blocking {
        server.enqueue(json(errorBody("quota_exceeded", "limit reached", """{"feature":"api"}"""), 429))
        assertFailsWith<QuotaExceededException> { client().entitlements.consume(QuotaRequest("api"), "k") }
    }

    @Test
    fun `billing customer, portal and store verification`() = blocking {
        server.enqueue(json("""{"id":"c1","subject":"user:1","currency":"usd","balance":0,"has_payment_method":true}"""))
        server.enqueue(json("""{"id":"c1","subject":"user:1","currency":"eur","balance":0,"has_payment_method":false}"""))
        server.enqueue(json("""{"session_id":"s","url":"https://pay.example/pm"}"""))
        server.enqueue(json("""{"id":"p1","status":"active"}"""))
        server.enqueue(json("""{"session_id":"s2","url":"https://store.example/checkout"}"""))
        val b = client().billing
        assertTrue(b.customers.get().hasPaymentMethod)
        assertEquals("eur", b.customers.put(email = "a@b.c", currency = "eur", taxExempt = false).currency)
        assertEquals("https://pay.example/pm", b.portal.paymentMethod(returnUrl = "myapp://back").url)
        b.store.verify("google_play", StoreReceipt(purchaseToken = "tok", productId = "pro_month"))
        assertEquals("https://store.example/checkout", b.store.checkout("creem", "pro", "myapp://ok", "idem").url)
        assertEquals("/api/v1/billing/customer", next().path)
        val put = next()
        assertEquals("PUT", put.method)
        assertEquals("eur", put.jsonBody()["currency"]?.jsonPrimitive?.content)
        assertEquals("myapp://back", next().jsonBody()["return_url"]?.jsonPrimitive?.content)
        val v = next()
        assertEquals("/api/v1/billing/store/google_play/verify", v.path)
        assertEquals("pro_month", v.jsonBody()["product_id"]?.jsonPrimitive?.content)
    }
}

class SearchApisTest : ServerTest() {
    @Test
    fun `vector search and hybrid`() = blocking {
        server.enqueue(json("""{"hits":[{"id":"a","score":0.9}]}"""))
        server.enqueue(json("""{"hits":[]}"""))
        val docs = client().vector.collection("docs")
        val r = docs.search(listOf(0.1f, 0.2f), VectorQuery(field = "emb", k = 5, filter = "year >= {:y}", params = mapOf("y" to 2020)), exact = true)
        assertEquals("a", r["hits"]!!.jsonArray.single().jsonObject["id"]?.jsonPrimitive?.content)
        docs.hybrid(text = "kotlin", q = VectorQuery(k = 3), textOperator = "and")
        val s = next()
        assertEquals("/api/v1/vector/collections/docs/search", s.path)
        val b = s.jsonBody()
        assertEquals("5", b["k"]?.jsonPrimitive?.content)
        assertEquals("true", b["exact"]?.jsonPrimitive?.content)
        assertEquals(2, b["vector"]!!.jsonArray.size)
        val h = next().jsonBody()
        assertEquals("and", h["text_operator"]?.jsonPrimitive?.content)
        assertNull(h["vector"])
    }

    @Test
    fun `graph traversal, neighbours and cypher`() = blocking {
        server.enqueue(json("""{"start":"n1","nodes":[],"edges":[],"truncated":false}"""))
        server.enqueue(json("""{"items":[{"direction":"out"}],"next_after":"x"}"""))
        server.enqueue(json("""{"columns":["n"],"rows":[[1]],"truncated":false}"""))
        val g = client().graph
        g.traverse("n1", maxDepth = 2, edgeTypes = listOf("KNOWS"), includeStart = true)
        val page = g.neighbors("n1", direction = "out", limit = 10)
        assertEquals("x", page.nextAfter)
        g.cypher("MATCH (n) RETURN n", mapOf("a" to 1), limit = 5)
        val t = next().jsonBody()
        assertEquals("2", t["max_depth"]?.jsonPrimitive?.content)
        assertEquals("out", next().requestUrl!!.queryParameter("direction"))
        assertEquals("MATCH (n) RETURN n", next().jsonBody()["query"]?.jsonPrimitive?.content)
    }

    @Test
    fun `sql returns rows as objects`() = blocking {
        server.enqueue(json("""{"kind":"read","columns":["id","name"],"rows":[[1,"a"],[2,null]],"rows_affected":0,"truncated":false,"duration_ms":1}"""))
        val rows = client().sql.query("select id, name from t where id > ?", listOf(0))
        assertEquals(2, rows.size)
        assertEquals("a", rows[0]["name"]?.jsonPrimitive?.content)
        assertEquals(JsonNull, rows[1]["name"])
        val b = next().jsonBody()
        assertEquals(1, b["params"]!!.jsonArray.size)
    }

    @Test
    fun `credits balance and ledger paging`() = blocking {
        server.enqueue(json("""{"balance":10}"""))
        server.enqueue(json("""{"items":[{"id":"1"},{"id":"2"}],"next":2}"""))
        server.enqueue(json("""{"items":[{"id":"3"}],"next":null}"""))
        val c = client().credits
        assertEquals(10, c.balance()["balance"]?.jsonPrimitive?.content?.toInt())
        assertEquals(listOf("1", "2", "3"), c.entries().toList().map { it["id"]?.jsonPrimitive?.content })
    }
}
