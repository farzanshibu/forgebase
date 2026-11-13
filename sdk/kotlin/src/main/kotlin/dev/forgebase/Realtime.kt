package dev.forgebase

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

// ---- protocol (ADR-0011) ------------------------------------------------------------------

public enum class RecordEventType(public val wire: String) {
    CREATED("record.created"), UPDATED("record.updated"), DELETED("record.deleted"),
}

/** The members of a presence set. */
public data class PresenceMember(
    val connectionId: String,
    val actor: String,
    val meta: JsonElement? = null,
    val joinedAt: String = "",
    val instance: String? = null,
)

/** What a subscription delivers. */
public sealed interface RealtimeEvent {
    /** Subscription id. */
    public val id: String

    /** A record of the watched collection changed. */
    public data class Record(
        override val id: String,
        val type: RecordEventType,
        /** Resume cursor: the SDK stores the highest one per subscription. */
        val cursor: Long,
        val collection: String,
        val recordId: String,
        val timestamp: String,
        val data: JsonObject,
        /** The primary's commit position when the event's page was read; the client feeds it to its commit tokens. */
        val commit: String? = null,
    ) : RealtimeEvent

    /** A message on a custom channel. */
    public data class Message(
        override val id: String,
        val channel: String,
        val from: String,
        val data: JsonElement,
        val timestamp: String,
    ) : RealtimeEvent

    public data class Presence(override val id: String, val channel: String, val joined: Boolean, val member: PresenceMember) : RealtimeEvent

    /** The server could not replay from the cursor: reload your state, then continue from [cursor]. */
    public data class Reset(override val id: String, val reason: String, val cursor: Long) : RealtimeEvent
}

/** What to subscribe to. */
public data class SubscribeOptions(
    /** Client-chosen id (1-64 of `[A-Za-z0-9_.:-]`); generated when omitted. */
    val id: String? = null,
    /** Collection to watch (add [record] for one record, or [filter]). */
    val collection: String? = null,
    val record: String? = null,
    val filter: String? = null,
    val params: Map<String, Any?>? = null,
    /** Custom pub/sub channel (instead of [collection]). */
    val channel: String? = null,
    /** Replay every event after this cursor, then go live. Without it the stream starts at the head. */
    val cursor: Long? = null,
)

/** A live subscription handle. */
public interface RealtimeSubscription {
    public val id: String

    /** Highest cursor delivered so far (what a reconnect resumes from). */
    public val cursor: Long?
    public suspend fun unsubscribe()
}

public enum class RealtimeState { IDLE, CONNECTING, OPEN, RECONNECTING, CLOSED }

/** Connection lifecycle notifications. */
public sealed interface ConnectionEvent {
    public data object Open : ConnectionEvent
    public data class Closed(val code: Int, val reason: String) : ConnectionEvent
    public data class Reconnecting(val attempt: Int, val delayMs: Long) : ConnectionEvent
    public data class Error(val error: Throwable) : ConnectionEvent
}

public class ReconnectOptions(
    public val baseDelayMs: Long = 500,
    public val maxDelayMs: Long = 15_000,
    public val maxAttempts: Int = Int.MAX_VALUE,
)

private class Sub(
    val id: String,
    val spec: JsonObject,
    val handler: (RealtimeEvent) -> Unit,
    val onFatal: (Throwable) -> Unit,
    @Volatile var cursor: Long?,
) {
    @Volatile var active = false
    @Volatile var error: Throwable? = null
    val activating = Mutex()
}

/**
 * Realtime client: `fb.realtime`. One WebSocket carries every subscription; it reconnects with jittered backoff and
 * resumes each subscription from its last cursor (events missed while offline are replayed, or a
 * [RealtimeEvent.Reset] says the cursor was too old).
 *
 * Server-sent-events fallback is not implemented: the WebSocket is the only transport.
 */
public class RealtimeClient internal constructor(
    private val t: Transport,
    private val auth: AuthManager,
    private val reconnect: ReconnectOptions,
    private val scope: CoroutineScope,
    private val random: () -> Double,
) {
    private val subs = ConcurrentHashMap<String, Sub>()
    private val pending = ConcurrentHashMap<String, CompletableDeferred<JsonObject>>()
    private val seq = AtomicInteger()
    private val stateFlow = MutableStateFlow(RealtimeState.IDLE)
    private val connEvents = MutableSharedFlow<ConnectionEvent>(extraBufferCapacity = 64)
    private val lifecycle = Mutex()
    private val wake = Channel<Unit>(Channel.CONFLATED)

    @Volatile private var ws: WebSocket? = null
    @Volatile private var loop: Job? = null
    @Volatile private var tokenWatch: Job? = null
    @Volatile private var stopped = false
    @Volatile private var opened = CompletableDeferred<Unit>()

    public val state: StateFlow<RealtimeState> get() = stateFlow.asStateFlow()

    /** Open / close / reconnect / error notifications. */
    public val connectionEvents: SharedFlow<ConnectionEvent> get() = connEvents.asSharedFlow()

    // ---- public API -----------------------------------------------------------------------

    /** Opens the connection (done by the first subscribe). Returns once connected and subscriptions are re-established. */
    public suspend fun connect() {
        val signal = lifecycle.withLock {
            stopped = false
            if (stateFlow.value == RealtimeState.OPEN) return
            if (loop?.isActive != true) {
                if (opened.isCompleted) opened = CompletableDeferred()
                startTokenWatch()
                loop = scope.launch { run() }
            }
            opened
        }
        signal.await()
    }

    /**
     * Subscribes and calls [handler] for every event (from a background thread). Returns when the server accepted the
     * subscription. Prefer the [subscribe] flow in UI code.
     */
    public suspend fun subscribe(options: SubscribeOptions, handler: (RealtimeEvent) -> Unit): RealtimeSubscription =
        subscribeInternal(options, handler) { }

    /**
     * Cold flow of events: collecting subscribes (connecting first), cancelling the collection unsubscribes. A
     * subscription the server refuses fails the flow with a [RealtimeException] or [ApiException].
     */
    public fun subscribe(options: SubscribeOptions): Flow<RealtimeEvent> = callbackFlow {
        val sub = subscribeInternal(options, { trySend(it) }, { close(it) })
        awaitClose { scope.launch { sub.unsubscribe() } }
    }.buffer(Channel.UNLIMITED)

    private suspend fun subscribeInternal(options: SubscribeOptions, handler: (RealtimeEvent) -> Unit, onFatal: (Throwable) -> Unit): RealtimeSubscription {
        val id = options.id ?: "s${seq.incrementAndGet()}_${(random() * 1e6).toLong().toString(36)}"
        val spec = obj(
            "collection" to options.collection, "record" to options.record, "filter" to options.filter,
            "params" to options.params, "channel" to options.channel,
        )
        val sub = Sub(id, spec, handler, onFatal, options.cursor)
        if (subs.putIfAbsent(id, sub) != null) throw RealtimeException("duplicate_subscription", "subscription id $id is already in use")
        try {
            if (stateFlow.value != RealtimeState.OPEN) connect()
            sub.error?.let { throw it }
            activate(sub)
        } catch (e: Throwable) {
            subs.remove(id)
            throw e
        }
        return object : RealtimeSubscription {
            override val id: String = sub.id
            override val cursor: Long? get() = sub.cursor
            override suspend fun unsubscribe() = this@RealtimeClient.unsubscribe(sub.id)
        }
    }

    public suspend fun unsubscribe(id: String) {
        val sub = subs.remove(id) ?: return
        if (ws != null && stateFlow.value == RealtimeState.OPEN && sub.active) {
            runCatching { request(obj("type" to "unsubscribe", "id" to id)) }
        }
    }

    /** Publishes to a channel (over the socket when open, else REST). */
    public suspend fun publish(channel: String, data: Any?) {
        if (ws != null && stateFlow.value == RealtimeState.OPEN) {
            request(obj("type" to "publish", "channel" to channel, "data" to data))
            return
        }
        t.request("/api/v1/realtime/channels/${seg(channel)}/messages", "POST", body = JsonObject(mapOf("data" to toJsonElement(data))))
    }

    /** Joins a channel's presence (the channel must be subscribed). Returns the members. */
    public suspend fun joinPresence(channel: String, meta: Any? = null): List<PresenceMember> {
        val r = request(obj("type" to "presence.join", "channel" to channel, "meta" to meta))
        return r.items("members").mapNotNull { member(it) }
    }

    public suspend fun leavePresence(channel: String) {
        request(obj("type" to "presence.leave", "channel" to channel))
    }

    /** Members currently present (REST, works without a connection). */
    public suspend fun presence(channel: String): List<PresenceMember> =
        t.request("/api/v1/realtime/channels/${seg(channel)}/presence").items("members").mapNotNull { member(it) }

    /** Closes the connection and forgets every subscription. */
    public fun close() {
        stopped = true
        tokenWatch?.cancel()
        tokenWatch = null
        subs.clear()
        fail(RealtimeException("closed", "realtime client closed"))
        ws?.close(1000, "client closed")
        loop?.cancel()
        wake.trySend(Unit)
        stateFlow.value = RealtimeState.CLOSED
    }

    // ---- connection loop ------------------------------------------------------------------

    private fun startTokenWatch() {
        if (tokenWatch?.isActive == true) return
        tokenWatch = scope.launch {
            auth.session.drop(1).collect { s ->
                val token = s?.accessToken
                val socket = ws
                if (token != null && socket != null && stateFlow.value == RealtimeState.OPEN) {
                    socket.send(ForgeJson.encodeToString(JsonElement.serializer(), obj("type" to "auth", "token" to token)))
                }
            }
        }
    }

    private suspend fun run() {
        var attempt = 0
        while (!stopped) {
            stateFlow.value = if (attempt == 0) RealtimeState.CONNECTING else RealtimeState.RECONNECTING
            var welcomed = false
            try {
                welcomed = runSocket()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                if (stopped) break
                connEvents.tryEmit(ConnectionEvent.Error(e))
                // Not worth retrying: bad credentials, forbidden, bad request.
                if (e is ForgeBaseException && e.status in intArrayOf(400, 401, 403, 404) && !tryRefresh(e)) {
                    fail(e)
                    break
                }
            }
            if (stopped) break
            if (welcomed) attempt = 0
            for (s in subs.values) s.active = false
            attempt++
            if (attempt > reconnect.maxAttempts) {
                fail(RealtimeException("reconnect_failed", "gave up after ${reconnect.maxAttempts} reconnect attempts"))
                break
            }
            val ceiling = minOf(reconnect.maxDelayMs.toDouble(), reconnect.baseDelayMs.toDouble() * Math.pow(2.0, (attempt - 1).toDouble()))
            val wait = if (welcomed && attempt == 1) 0L else (random() * ceiling).toLong()
            stateFlow.value = RealtimeState.RECONNECTING
            connEvents.tryEmit(ConnectionEvent.Reconnecting(attempt, wait))
            if (wait > 0) withTimeoutOrNull(wait) { wake.receive() } else delay(0)
        }
        if (stateFlow.value != RealtimeState.CLOSED) stateFlow.value = RealtimeState.CLOSED
    }

    private suspend fun tryRefresh(e: ForgeBaseException): Boolean {
        if (e.status != 401 || auth.apiKey != null) return false
        return try {
            auth.refresh()
            true
        } catch (c: CancellationException) {
            throw c
        } catch (_: Exception) {
            false
        }
    }

    /** Runs one socket; returns whether the server welcomed us. */
    private suspend fun runSocket(): Boolean {
        val done = CompletableDeferred<Boolean>()
        val headers = LinkedHashMap<String, String>()
        headers.putAll(t.defaultHeaders)
        headers.putAll(auth.headers("GET"))
        val req = Request.Builder().url("${t.baseUrl}/api/v1/realtime/ws").apply { headers.forEach { (k, v) -> header(k, v) } }.build()
        val welcomedFlag = java.util.concurrent.atomic.AtomicBoolean(false)
        val lastSeen = java.util.concurrent.atomic.AtomicLong(System.currentTimeMillis())
        val heartbeatJob = java.util.concurrent.atomic.AtomicReference<Job?>(null)

        fun finish(code: Int, reason: String, err: Throwable?) {
            if (done.isCompleted) return
            failPending(RealtimeException("connection_closed", "connection closed ($code)"))
            connEvents.tryEmit(ConnectionEvent.Closed(code, reason))
            when {
                err != null -> done.completeExceptionally(err)
                code == 4001 -> scope.launch {
                    runCatching { if (auth.apiKey == null) auth.refresh() }
                    done.complete(welcomedFlag.get())
                }
                else -> done.complete(welcomedFlag.get())
            }
        }

        val listener = object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                lastSeen.set(System.currentTimeMillis())
                val m = runCatching { ForgeJson.parseToJsonElement(text) as? JsonObject }.getOrNull() ?: return
                if (m["type"].str() == "welcome" && welcomedFlag.compareAndSet(false, true)) {
                    val hb = (m["heartbeat_ms"].long() ?: 25_000L).coerceAtLeast(1)
                    heartbeatJob.set(scope.launch {
                        while (true) {
                            delay(hb)
                            if (System.currentTimeMillis() - lastSeen.get() > hb * 5 / 2) {
                                webSocket.close(4002, "heartbeat timeout")
                                break
                            }
                            webSocket.send("""{"type":"ping"}""")
                        }
                    })
                    scope.launch {
                        resubscribeAll()
                        stateFlow.value = RealtimeState.OPEN
                        connEvents.tryEmit(ConnectionEvent.Open)
                        opened.complete(Unit)
                    }
                    return
                }
                onMessage(m)
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(code, reason)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = finish(code, reason, null)

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                if (welcomedFlag.get()) return finish(1006, t.message ?: "connection lost", null)
                val err: Throwable = if (response != null && response.code >= 400) {
                    val body = runCatching { response.body?.string() }.getOrNull().orEmpty()
                    errorFromResponse(response.code, runCatching { ForgeJson.parseToJsonElement(body) }.getOrNull(), response.headers)
                } else {
                    RealtimeException("connect_failed", t.message ?: "WebSocket connection failed")
                }
                finish(1006, "connect failed", err)
            }
        }
        val socket = t.http.newWebSocket(req, listener)
        ws = socket
        try {
            return done.await()
        } finally {
            heartbeatJob.get()?.cancel()
            if (ws === socket) ws = null
            socket.cancel()
        }
    }

    private suspend fun resubscribeAll() {
        for (s in subs.values.toList()) {
            try {
                activate(s)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                // A subscription the server now refuses (rule changed while offline) is dropped and reported.
                s.error = e
                subs.remove(s.id)
                connEvents.tryEmit(ConnectionEvent.Error(e))
                s.onFatal(e)
            }
        }
    }

    private suspend fun activate(sub: Sub) {
        sub.activating.withLock {
            if (sub.active) return
            val msg = LinkedHashMap<String, JsonElement>()
            msg["type"] = JsonPrimitive("subscribe")
            msg["id"] = JsonPrimitive(sub.id)
            msg.putAll(sub.spec)
            sub.cursor?.let { msg["cursor"] = JsonPrimitive(it) }
            val r = request(JsonObject(msg))
            sub.active = true
            val c = r["cursor"].long()
            if (c != null && sub.cursor == null) sub.cursor = c
        }
    }

    private suspend fun request(msg: JsonObject): JsonObject {
        val socket = ws
        if (socket == null || stateFlow.value == RealtimeState.CLOSED) throw RealtimeException("not_connected", "realtime socket is not open")
        val ref = seq.incrementAndGet().toString()
        val d = CompletableDeferred<JsonObject>()
        pending[ref] = d
        try {
            val withRef = JsonObject(msg + ("ref" to JsonPrimitive(ref)))
            if (!socket.send(ForgeJson.encodeToString(JsonElement.serializer(), withRef))) {
                throw RealtimeException("not_connected", "realtime socket is not open")
            }
            return withTimeout(30_000) { d.await() }
        } finally {
            pending.remove(ref)
        }
    }

    private fun onMessage(m: JsonObject) {
        val ref = (m["ref"] as? JsonPrimitive)?.contentOrNull
        if (ref != null) {
            val p = pending.remove(ref)
            if (p != null) {
                if (m["type"].str() == "error") p.completeExceptionally(RealtimeException(m["code"].str() ?: "error", m["message"].str() ?: "realtime error"))
                else p.complete(m)
                return
            }
        }
        when (val type = m["type"].str()) {
            "record.created", "record.updated", "record.deleted", "message", "presence.join", "presence.leave", "reset" -> {
                val id = m["id"].str() ?: return
                val sub = subs[id] ?: return
                m["commit"].str()?.let { t.commitTokens.note(it) }
                val cursor = m["cursor"].long()
                if (cursor != null) {
                    val cur = sub.cursor
                    if (cur == null || cursor > cur || type == "reset") sub.cursor = cursor
                }
                val ev = toEvent(type, id, m) ?: return
                try {
                    sub.handler(ev)
                } catch (e: Throwable) {
                    connEvents.tryEmit(ConnectionEvent.Error(e))
                }
            }
            "error" -> connEvents.tryEmit(ConnectionEvent.Error(RealtimeException(m["code"].str() ?: "error", m["message"].str() ?: "realtime error")))
            else -> Unit
        }
    }

    private fun toEvent(type: String, id: String, m: JsonObject): RealtimeEvent? = when (type) {
        "record.created", "record.updated", "record.deleted" -> RealtimeEvent.Record(
            id = id,
            type = RecordEventType.entries.first { it.wire == type },
            cursor = m["cursor"].long() ?: 0,
            collection = m["collection"].str() ?: "",
            recordId = m["record_id"].str() ?: "",
            timestamp = m["timestamp"].str() ?: "",
            data = (m["data"] as? JsonObject) ?: JsonObject(emptyMap()),
            commit = m["commit"].str(),
        )
        "message" -> RealtimeEvent.Message(id, m["channel"].str() ?: "", m["from"].str() ?: "", m["data"] ?: JsonNull, m["timestamp"].str() ?: "")
        "presence.join", "presence.leave" -> member(m["member"])?.let { RealtimeEvent.Presence(id, m["channel"].str() ?: "", type == "presence.join", it) }
        "reset" -> RealtimeEvent.Reset(id, m["reason"].str() ?: "", m["cursor"].long() ?: 0)
        else -> null
    }

    private fun member(e: JsonElement?): PresenceMember? {
        val o = e as? JsonObject ?: return null
        return PresenceMember(
            connectionId = o["connection_id"].str() ?: "",
            actor = o["actor"].str() ?: "",
            meta = o["meta"]?.takeIf { it !is JsonNull },
            joinedAt = o["joined_at"].str() ?: "",
            instance = o["instance"].str(),
        )
    }

    private fun failPending(e: Throwable) {
        val all = pending.values.toList()
        pending.clear()
        for (p in all) p.completeExceptionally(e)
    }

    private fun fail(e: Throwable) {
        failPending(e)
        opened.completeExceptionally(e)
        for (s in subs.values.toList()) s.onFatal(e)
    }
}
