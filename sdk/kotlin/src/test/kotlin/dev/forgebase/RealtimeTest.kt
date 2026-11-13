package dev.forgebase

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.RecordedRequest
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.LinkedBlockingQueue
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/** A scripted server side of the ADR-0011 protocol. */
private class FakeSocket(private val onConnected: (WebSocket) -> Unit = {}) : WebSocketListener() {
    val received = CopyOnWriteArrayList<JsonObject>()
    val inbox = LinkedBlockingQueue<JsonObject>()
    @Volatile var socket: WebSocket? = null
    @Volatile var refuse: Set<String> = emptySet()

    override fun onOpen(webSocket: WebSocket, response: Response) {
        socket = webSocket
        webSocket.send("""{"type":"welcome","connection_id":"c1","heartbeat_ms":60000}""")
        onConnected(webSocket)
    }

    override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
        webSocket.close(code, reason)
    }

    override fun onMessage(webSocket: WebSocket, text: String) {
        val m = ForgeJson.parseToJsonElement(text).jsonObject
        received.add(m)
        inbox.add(m)
        val ref = m["ref"]?.jsonPrimitive?.content
        val type = m["type"]?.jsonPrimitive?.content
        val id = m["id"]?.jsonPrimitive?.content
        if (ref == null) return
        when {
            type == "subscribe" && id in refuse -> webSocket.send("""{"type":"error","ref":"$ref","code":"forbidden","message":"not allowed"}""")
            type == "subscribe" -> webSocket.send("""{"type":"subscribed","ref":"$ref","id":"$id","cursor":${m["cursor"]?.jsonPrimitive?.content ?: "0"}}""")
            type == "presence.join" -> webSocket.send("""{"type":"presence.joined","ref":"$ref","members":[{"connection_id":"c9","actor":"user:1","meta":{"n":1},"joined_at":"t"}]}""")
            else -> webSocket.send("""{"type":"ok","ref":"$ref"}""")
        }
    }

    suspend fun await(type: String, timeoutMs: Long = 5000): JsonObject {
        val end = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < end) {
            val m = inbox.poll()
            if (m == null) {
                delay(10)
                continue
            }
            if (m["type"]?.jsonPrimitive?.content == type) return m
        }
        error("no $type message received; got ${received.map { it["type"] }}")
    }
}

class RealtimeTest : ServerTest() {
    private fun record(id: String, cursor: Int, recordId: String = "r$cursor") =
        """{"type":"record.created","id":"$id","cursor":$cursor,"collection":"posts","record_id":"$recordId","timestamp":"t","data":{"id":"$recordId","title":"x"}}"""

    @Test
    fun `subscribe delivers record, message, presence and reset events and tracks the cursor`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        val events = mutableListOf<RealtimeEvent>()
        val collector = launch { fb.realtime.subscribe(SubscribeOptions(id = "s1", collection = "posts")).take(4).toList(events) }
        val sub = fake.await("subscribe")
        assertEquals("posts", sub["collection"]?.jsonPrimitive?.content)
        assertEquals("s1", sub["id"]?.jsonPrimitive?.content)
        val ws = fake.socket!!
        ws.send(record("s1", 5))
        ws.send("""{"type":"message","id":"s1","channel":"chat","from":"user:2","data":{"hi":1},"timestamp":"t"}""")
        ws.send("""{"type":"presence.join","id":"s1","channel":"chat","member":{"connection_id":"c2","actor":"user:2","joined_at":"t"}}""")
        ws.send("""{"type":"reset","id":"s1","reason":"cursor_expired","cursor":9}""")
        collector.join()
        val rec = assertIs<RealtimeEvent.Record>(events[0])
        assertEquals(RecordEventType.CREATED, rec.type)
        assertEquals("r5", rec.recordId)
        assertEquals("x", rec.data["title"]?.jsonPrimitive?.content)
        assertEquals("chat", assertIs<RealtimeEvent.Message>(events[1]).channel)
        assertTrue(assertIs<RealtimeEvent.Presence>(events[2]).joined)
        assertEquals("cursor_expired", assertIs<RealtimeEvent.Reset>(events[3]).reason)
        assertEquals(RealtimeState.OPEN, fb.realtime.state.value)
    }

    @Test
    fun `the handshake carries the credentials in headers, not in the URL`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key", headers = mapOf("X-Forgebase-Org" to "acme"))
        fb.realtime.connect()
        val r: RecordedRequest = next()
        assertEquals("/api/v1/realtime/ws", r.path)
        assertEquals("fb_key", r.getHeader("X-API-Key"))
        assertEquals("acme", r.getHeader("X-Forgebase-Org"))
    }

    @Test
    fun `reconnects after a drop and resumes each subscription from its last cursor`() = blocking {
        val first = FakeSocket()
        val second = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(first))
        server.enqueue(MockResponse().withWebSocketUpgrade(second))
        val fb = client(apiKey = "fb_key")
        val events = mutableListOf<RealtimeEvent>()
        val collector = launch { fb.realtime.subscribe(SubscribeOptions(id = "s1", collection = "posts")).take(2).toList(events) }
        first.await("subscribe")
        first.socket!!.send(record("s1", 7))
        // wait until the client has seen it, then kill the connection abruptly
        while (events.isEmpty()) delay(10)
        first.socket!!.close(1001, "going away")
        val resub = second.await("subscribe")
        assertEquals(7L, resub["cursor"]?.jsonPrimitive?.content?.toLong())
        assertEquals("s1", resub["id"]?.jsonPrimitive?.content)
        second.socket!!.send(record("s1", 8))
        collector.join()
        assertEquals(listOf("r7", "r8"), events.map { (it as RealtimeEvent.Record).recordId })
        assertEquals(2, server.requestCount)
    }

    @Test
    fun `a 4001 close refreshes the token and reconnects`() = blocking {
        server.enqueue(json(sessionJson("at1", "rt1")))
        val first = FakeSocket()
        val second = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(first))
        server.enqueue(json(sessionJson("at2", "rt2")))
        server.enqueue(MockResponse().withWebSocketUpgrade(second))
        val fb = client()
        fb.auth.signIn("a@b.c", "pw")
        fb.realtime.connect()
        assertEquals("/api/auth/login", next().path)
        assertEquals("Bearer at1", next().getHeader("Authorization"))
        first.socket!!.close(4001, "token expired")
        assertEquals("/api/auth/refresh", next().path)
        assertEquals("Bearer at2", next().getHeader("Authorization"))
        withTimeout(5000) { while (fb.realtime.state.value != RealtimeState.OPEN) delay(10) }
    }

    @Test
    fun `a refreshed access token is pushed over the open socket`() = blocking {
        server.enqueue(json(sessionJson("at1", "rt1")))
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        server.enqueue(json(sessionJson("at2", "rt2")))
        val fb = client()
        fb.auth.signIn("a@b.c", "pw")
        fb.realtime.connect()
        fb.auth.refresh()
        val auth = fake.await("auth")
        assertEquals("at2", auth["token"]?.jsonPrimitive?.content)
    }

    @Test
    fun `a refused handshake fails the subscription with the typed error`() = blocking {
        server.enqueue(json(errorBody("forbidden", "no realtime for you"), 403))
        val fb = client(apiKey = "fb_key")
        assertFailsWith<PermissionException> { fb.realtime.subscribe(SubscribeOptions(collection = "posts")).first() }
    }

    @Test
    fun `a subscription the server refuses throws and is not kept`() = blocking {
        val fake = FakeSocket().also { it.refuse = setOf("nope") }
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        val e = assertFailsWith<RealtimeException> { fb.realtime.subscribe(SubscribeOptions(id = "nope", collection = "secret")) { } }
        assertEquals("forbidden", e.code)
        // the id is free again
        fb.realtime.subscribe(SubscribeOptions(id = "nope2", collection = "posts")) { }
    }

    @Test
    fun `duplicate subscription ids are rejected`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        fb.realtime.subscribe(SubscribeOptions(id = "dup", collection = "posts")) { }
        val e = assertFailsWith<RealtimeException> { fb.realtime.subscribe(SubscribeOptions(id = "dup", collection = "posts")) { } }
        assertEquals("duplicate_subscription", e.code)
    }

    @Test
    fun `cancelling the collector unsubscribes`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        val job = launch { fb.realtime.subscribe(SubscribeOptions(id = "s1", collection = "posts")).collect { } }
        fake.await("subscribe")
        job.cancel()
        val un = fake.await("unsubscribe")
        assertEquals("s1", un["id"]?.jsonPrimitive?.content)
    }

    @Test
    fun `publish over the socket, presence join and leave`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        fb.realtime.connect()
        fb.realtime.publish("chat", mapOf("text" to "hi"))
        val pub = fake.await("publish")
        assertEquals("chat", pub["channel"]?.jsonPrimitive?.content)
        assertEquals("hi", pub["data"]?.jsonObject?.get("text")?.jsonPrimitive?.content)
        val members = fb.realtime.joinPresence("chat", mapOf("n" to 1))
        assertEquals("user:1", members.single().actor)
        assertNotNull(members.single().meta)
        fb.realtime.leavePresence("chat")
        fake.await("presence.leave")
    }

    @Test
    fun `publish and presence fall back to REST without a connection`() = blocking {
        server.enqueue(json("{}"))
        server.enqueue(json("""{"members":[{"connection_id":"c1","actor":"user:1","joined_at":"t"}]}"""))
        val fb = client(apiKey = "fb_key")
        fb.realtime.publish("chat", "hello")
        assertEquals("/api/v1/realtime/channels/chat/messages", next().path)
        assertEquals("user:1", fb.realtime.presence("chat").single().actor)
    }

    @Test
    fun `commit positions on record events feed the commit tokens`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        val got = launch { fb.realtime.subscribe(SubscribeOptions(id = "s1", collection = "posts")).first() }
        fake.await("subscribe")
        fake.socket!!.send("""{"type":"record.updated","id":"s1","cursor":1,"collection":"posts","record_id":"r","timestamp":"t","data":{},"commit":"control/16/B374D848"}""")
        got.join()
        assertEquals("control/16/B374D848", fb.commitTokens.header())
    }

    @Test
    fun `close fails nothing silently - state is CLOSED and the socket is shut`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        val fb = client(apiKey = "fb_key")
        fb.realtime.connect()
        fb.realtime.close()
        assertEquals(RealtimeState.CLOSED, fb.realtime.state.value)
    }

    @Test
    fun `jobs progress emits the stored state first, then live updates until the job is final`() = blocking {
        val fake = FakeSocket()
        server.enqueue(MockResponse().withWebSocketUpgrade(fake))
        server.enqueue(json("""{"id":"j1","queue":"q","kind":"k","status":"running","updated_at":"2026-01-01T00:00:01Z","progress":{"percent":10,"updated_at":"2026-01-01T00:00:01Z"}}"""))
        val fb = client(apiKey = "fb_key")
        val updates = mutableListOf<JobUpdate>()
        val collector = launch { fb.jobs.progress("j1").toList(updates) }
        val sub = fake.await("subscribe")
        assertEquals("jobs:j1", sub["channel"]?.jsonPrimitive?.content)
        val id = sub["id"]!!.jsonPrimitive.content
        while (updates.isEmpty()) delay(10)
        fun msg(event: String, status: String, ts: String, pct: Int) =
            """{"type":"message","id":"$id","channel":"jobs:j1","from":"server","timestamp":"t","data":{"event":"$event","job_id":"j1","queue":"q","kind":"k","status":"$status","attempts":1,"updated_at":"$ts","progress":{"percent":$pct,"updated_at":"$ts"}}}"""
        fake.socket!!.send(msg("progress", "running", "2026-01-01T00:00:00Z", 5)) // older than the snapshot: dropped
        fake.socket!!.send(msg("progress", "running", "2026-01-01T00:00:02Z", 60))
        fake.socket!!.send(msg("status", "succeeded", "2026-01-01T00:00:03Z", 100))
        collector.join()
        assertEquals(listOf(10.0, 60.0, 100.0), updates.map { it.progress?.percent })
        assertEquals(JobStatus.SUCCEEDED, updates.last().state)
    }
}
