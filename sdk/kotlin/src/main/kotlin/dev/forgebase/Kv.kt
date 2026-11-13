package dev.forgebase

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import java.util.Base64

/*
 * Key-value engine (`/api/v1/kv`): `fb.kv`.
 * Strings, hashes, lists, sets, sorted sets, streams, TTLs, generic commands and pub/sub, all inside a namespace:
 * `val cache = fb.kv.ns("cache"); cache.set("k", "v", KvSetOptions(ttlMs = 60_000))`.
 */

/** A value read from the engine: UTF-8 [text], or [bytes] when the stored bytes are not valid UTF-8. */
public sealed interface KvBin {
    public data class Text(val value: String) : KvBin
    public data class Bytes(val value: ByteArray) : KvBin {
        override fun equals(other: Any?): Boolean = other is Bytes && value.contentEquals(other.value)
        override fun hashCode(): Int = value.contentHashCode()
    }

    /** The bytes (text is UTF-8 encoded). */
    public fun toBytes(): ByteArray = when (this) {
        is Text -> value.toByteArray(Charsets.UTF_8)
        is Bytes -> value
    }

    /** The text (bytes are decoded as UTF-8, lossy). */
    public fun toText(): String = when (this) {
        is Text -> value
        is Bytes -> String(value, Charsets.UTF_8)
    }
}

internal fun kvBin(e: JsonElement?): KvBin? = when (e) {
    null, JsonNull -> null
    is JsonPrimitive -> KvBin.Text(e.content)
    is JsonObject -> e["base64"].str()?.let { KvBin.Bytes(Base64.getDecoder().decode(it)) }
    else -> null
}

/** A value to write: text, raw bytes (sent as base64), numbers (sent in decimal) or an already wire-shaped element. */
private fun wireValue(v: Any?): JsonElement = when (v) {
    is String -> JsonPrimitive(v)
    is Number -> JsonPrimitive(v.toString())
    is ByteArray -> JsonObject(mapOf("base64" to JsonPrimitive(Base64.getEncoder().encodeToString(v))))
    is KvBin -> wireValue(if (v is KvBin.Text) v.value else v.toBytes())
    is JsonElement -> v
    else -> throw IllegalArgumentException("KV values are String, ByteArray, Number or KvBin, not ${v?.let { it::class.simpleName }}")
}

/** One reply of a generic command ([KvNamespace.exec] / [KvNamespace.batch]): a JSON value (`null`, text, integer, `{base64}` or an array of those). */
public typealias KvReply = JsonElement

/** The type, TTL, size and value of a key (`value` is shaped by `type`). */
public data class KvKey(
    val key: String,
    val type: String,
    val ttlMs: Long?,
    val size: Long,
    val truncated: Boolean,
    val value: JsonElement,
)

public data class KvScanPage(val cursor: String, val keys: List<String>)

public data class KvScanOptions(
    /** Cursor of the previous page (`"0"` or absent starts). */
    val cursor: String? = null,
    /** Glob pattern (`user:*`). */
    val match: String? = null,
    /** Keys per page (1 to 1000). */
    val count: Int? = null,
    /** Only keys of this type. */
    val type: String? = null,
)

public data class KvSetOptions(
    /** Expire after this many milliseconds. */
    val ttlMs: Long? = null,
    /** Only set when the key does not exist. */
    val nx: Boolean? = null,
    /** Only set when the key exists. */
    val xx: Boolean? = null,
    /** Keep the key's current TTL. */
    val keepTtl: Boolean? = null,
)

public data class KvStreamEntry(val id: String, val fields: Map<String, KvBin>)

public data class KvMessage(val channel: String, val message: KvBin)

public data class KvChannel(val name: String, val subscribers: Long)

/** One result of [KvNamespace.batch]: a [result], or an [error] for a command that failed (the rest still ran). */
public data class KvBatchResult(val result: KvReply?, val errorCode: String? = null, val errorMessage: String? = null) {
    public val isError: Boolean get() = errorCode != null
}

private const val K = "/api/v1/kv"

/** Namespaces, engine statistics and generic access: `fb.kv`. Key operations live on [ns]. */
public class KvApi internal constructor(private val t: Transport) {
    /** A handle on one namespace; every key operation goes through it. Cheap: create as many as you like. */
    public fun ns(name: String): KvNamespace = KvNamespace(t, name)

    /** Engine statistics: memory, hit rate, evictions, unsaved keys (administrators). */
    public suspend fun info(): JsonObject = t.request("$K/_info") as JsonObject

    /** Namespaces of the caller's tenant that hold keys. */
    public suspend fun namespaces(): List<JsonElement> = t.request("$K/_namespaces").items("namespaces")

    /** Deletes every key of a namespace; returns the number of keys removed. */
    public suspend fun flush(namespace: String): Long = t.request("$K/${seg(namespace)}", "DELETE").objOrNull()?.get("deleted").long() ?: 0
}

/** The key operations of one namespace. */
public class KvNamespace internal constructor(private val t: Transport, public val name: String) {
    private fun path(rest: String = "") = "$K/${seg(name)}$rest"
    private fun keyPath(kind: String, key: String, rest: String = "") = path("/$kind/${seg(key)}$rest")
    private fun long(e: JsonElement, field: String): Long = e.objOrNull()?.get(field).long() ?: 0

    // ------------------------------------------------------------------ keys

    /** One page of keys (`SCAN`): by key order, tolerant of concurrent changes. */
    public suspend fun scan(o: KvScanOptions = KvScanOptions()): KvScanPage {
        val r = t.request(path("/keys"), query = mapOf("cursor" to o.cursor, "match" to o.match, "count" to o.count, "type" to o.type))
        return KvScanPage(r.objOrNull()?.get("cursor").str() ?: "0", r.items("keys").mapNotNull { it.str() })
    }

    /** Every key matching the options, page after page. */
    public fun keys(o: KvScanOptions = KvScanOptions()): Flow<String> = flow {
        var cursor = o.cursor
        do {
            val page = scan(o.copy(cursor = cursor))
            page.keys.forEach { emit(it) }
            cursor = page.cursor
        } while (cursor != "0" && cursor.isNotEmpty())
    }

    /** Type, TTL, size and value of a key; at most [limit] items of a collection. A missing key is a [NotFoundException]. */
    public suspend fun get(key: String, limit: Int? = null): KvKey {
        val r = t.request(path("/keys/${seg(key)}"), query = mapOf("limit" to limit)) as JsonObject
        return KvKey(
            key = r["key"].str() ?: key, type = r["type"].str() ?: "", ttlMs = r["ttl_ms"].long(),
            size = r["size"].long() ?: 0, truncated = (r["truncated"] as? JsonPrimitive)?.content == "true",
            value = r["value"] ?: JsonNull,
        )
    }

    /** Deletes a key of any type; `false` when there was none. */
    public suspend fun delete(key: String): Boolean =
        (t.request(path("/keys/${seg(key)}"), "DELETE").objOrNull()?.get("deleted") as? JsonPrimitive)?.content == "true"

    /** Makes the key expire in [ttlMs] milliseconds. A missing key is a [NotFoundException]. */
    public suspend fun expire(key: String, ttlMs: Long) {
        t.request(path("/keys/${seg(key)}/ttl"), "PUT", body = obj("ttl_ms" to ttlMs))
    }

    /** Removes the TTL; `false` when the key had none. */
    public suspend fun persist(key: String): Boolean =
        (t.request(path("/keys/${seg(key)}/ttl"), "DELETE").objOrNull()?.get("persisted") as? JsonPrimitive)?.content == "true"

    // --------------------------------------------------------------- strings

    /** Sets a string ([value]: String, ByteArray or Number). Returns `false` when `nx` / `xx` prevented the write. */
    public suspend fun set(key: String, value: Any, o: KvSetOptions = KvSetOptions()): Boolean {
        val r = t.request(
            keyPath("strings", key), "PUT",
            body = obj("value" to wireValue(value), "ttl_ms" to o.ttlMs, "nx" to o.nx, "xx" to o.xx, "keepttl" to o.keepTtl),
        )
        return (r.objOrNull()?.get("stored") as? JsonPrimitive)?.content == "true"
    }

    /** The string at [key] as text, or `null` when there is none (or the key holds another type). */
    public suspend fun text(key: String): String? = mget(listOf(key)).firstOrNull()?.toText()

    /** Adds [by] (default 1; negative decrements) to the integer in a string; returns the new value. */
    public suspend fun incr(key: String, by: Long? = null): Long = long(t.request(keyPath("strings", key, "/incr"), "POST", body = obj("by" to by)), "value")

    /** Reads several strings at once, in key order; `null` for a missing key or one of another type. */
    public suspend fun mget(keys: List<String>): List<KvBin?> =
        t.request(path("/mget"), "POST", body = obj("keys" to keys)).items("values").map { kvBin(it) }

    /** Writes several strings in one step; returns how many. */
    public suspend fun mset(entries: Map<String, Any>): Long = long(
        t.request(path("/mset"), "POST", body = obj("entries" to entries.map { (k, v) -> obj("key" to k, "value" to wireValue(v)) })),
        "stored",
    )

    // ---------------------------------------------------------------- hashes

    /** Sets hash fields; returns the number of new fields. */
    public suspend fun hashSet(key: String, fields: Map<String, Any>): Long =
        long(t.request(keyPath("hashes", key, "/set"), "POST", body = obj("fields" to fields.mapValues { wireValue(it.value) })), "added")

    public suspend fun hashDelete(key: String, fields: List<String>): Long =
        long(t.request(keyPath("hashes", key, "/delete"), "POST", body = obj("fields" to fields)), "removed")

    public suspend fun hashIncr(key: String, field: String, by: Long? = null): Long =
        long(t.request(keyPath("hashes", key, "/incr"), "POST", body = obj("field" to field, "by" to by)), "value")

    // ----------------------------------------------------------------- lists

    /** Pushes values onto the head (`"left"`) or tail (`"right"`); returns the new length. */
    public suspend fun listPush(key: String, side: String, values: List<Any>): Long =
        long(t.request(keyPath("lists", key, "/push"), "POST", body = obj("side" to side, "values" to values.map(::wireValue))), "length")

    /** Pops [count] (default 1) values from one end; an empty list gives `[]`. */
    public suspend fun listPop(key: String, side: String, count: Int? = null): List<KvBin> =
        t.request(keyPath("lists", key, "/pop"), "POST", body = obj("side" to side, "count" to count)).items("values").mapNotNull { kvBin(it) }

    public suspend fun listRemove(key: String, value: Any, count: Int? = null): Long =
        long(t.request(keyPath("lists", key, "/remove"), "POST", body = obj("value" to wireValue(value), "count" to count)), "removed")

    public suspend fun listSet(key: String, index: Int, value: Any) {
        t.request(keyPath("lists", key, "/set"), "POST", body = obj("index" to index, "value" to wireValue(value)))
    }

    // ------------------------------------------------------------------ sets

    public suspend fun setAdd(key: String, members: List<Any>): Long =
        long(t.request(keyPath("sets", key, "/add"), "POST", body = obj("members" to members.map(::wireValue))), "added")

    public suspend fun setRemove(key: String, members: List<Any>): Long =
        long(t.request(keyPath("sets", key, "/remove"), "POST", body = obj("members" to members.map(::wireValue))), "removed")

    // ----------------------------------------------------------- sorted sets

    /** Adds members or re-scores them (scores must be finite); returns the number of new members. */
    public suspend fun zsetAdd(key: String, members: List<Pair<Any, Double>>): Long = long(
        t.request(keyPath("zsets", key, "/add"), "POST", body = obj("members" to members.map { (m, s) -> obj("member" to wireValue(m), "score" to s) })),
        "added",
    )

    public suspend fun zsetRemove(key: String, members: List<Any>): Long =
        long(t.request(keyPath("zsets", key, "/remove"), "POST", body = obj("members" to members.map(::wireValue))), "removed")

    // --------------------------------------------------------------- streams

    /** Appends an entry; returns its id. */
    public suspend fun streamAdd(key: String, fields: Map<String, Any>, id: String? = null, maxLen: Long? = null): String =
        t.request(keyPath("streams", key, "/add"), "POST", body = obj("id" to id, "fields" to fields.mapValues { wireValue(it.value) }, "maxlen" to maxLen))
            .objOrNull()?.get("id").str() ?: throw InvalidResponseException("stream add returned no id")

    public suspend fun streamDelete(key: String, ids: List<String>): Long =
        long(t.request(keyPath("streams", key, "/delete"), "POST", body = obj("ids" to ids)), "removed")

    /** Entries between two ids (`-` and `+` are the ends). */
    public suspend fun streamRange(key: String, start: String? = null, end: String? = null, count: Int? = null, rev: Boolean? = null): List<KvStreamEntry> =
        entries(t.request(keyPath("streams", key, "/range"), query = mapOf("start" to start, "end" to end, "count" to count, "rev" to rev)))

    /** Entries after an id, without waiting for new ones. */
    public suspend fun streamRead(key: String, after: String? = null, count: Int? = null): List<KvStreamEntry> =
        entries(t.request(keyPath("streams", key, "/read"), query = mapOf("after" to after, "count" to count)))

    private fun entries(r: JsonElement): List<KvStreamEntry> = r.items("entries").mapNotNull { e ->
        val o = e as? JsonObject ?: return@mapNotNull null
        KvStreamEntry(o["id"].str() ?: "", (o["fields"] as? JsonObject)?.mapNotNull { (k, v) -> kvBin(v)?.let { k to it } }?.toMap() ?: emptyMap())
    }

    // ------------------------------------------------------- generic command

    /** Runs one command given as a list: `exec(listOf("ZRANGE", "board", 0, -1, "WITHSCORES"))`. */
    public suspend fun exec(command: List<Any>): KvReply =
        t.request(path("/exec"), "POST", body = obj("command" to command.map(::wireValue))).objOrNull()?.get("result") ?: JsonNull

    /** Runs 1 to 1000 commands in order with no other command in between. A failing command reports an error and the rest still run. */
    public suspend fun batch(commands: List<List<Any>>): List<KvBatchResult> =
        t.request(path("/batch"), "POST", body = obj("commands" to commands.map { c -> c.map(::wireValue) })).items("results").map { r ->
            val o = r as? JsonObject
            val err = o?.get("error") as? JsonObject
            if (err != null) KvBatchResult(null, err["code"].str() ?: "error", err["message"].str() ?: "") else KvBatchResult(o?.get("result"))
        }

    // --------------------------------------------------------------- pub/sub

    /** Publishes to the subscribers connected now (messages are not stored); returns how many received it. */
    public suspend fun publish(channel: String, message: Any): Long =
        long(t.request(path("/publish/${seg(channel)}"), "POST", body = obj("message" to wireValue(message))), "receivers")

    /** Live channels of the namespace (those with at least one subscriber). */
    public suspend fun channels(): List<KvChannel> = t.request(path("/channels")).items("channels").map {
        KvChannel(it.objOrNull()?.get("name").str() ?: "", it.objOrNull()?.get("subscribers").long() ?: 0)
    }

    /**
     * Subscribes to a channel (server-sent events): the flow emits every message published after the stream opens;
     * cancelling the collector unsubscribes. A dropped stream reconnects with backoff (messages published meanwhile
     * are lost: the engine does not store them); a refusal (missing permission) fails the flow.
     */
    public fun subscribe(channel: String, reconnect: Boolean = true, onLagged: ((Long) -> Unit)? = null): Flow<KvMessage> = flow {
        var wait = 500L
        while (true) {
            var opened = false
            try {
                val res = t.send(path("/subscribe/${seg(channel)}"), headers = mapOf("Accept" to "text/event-stream"), retry = false, streaming = true)
                opened = true
                sseEvents(res).collect { ev ->
                    when (ev.event) {
                        "lagged" -> onLagged?.invoke(ev.data.trim().toLongOrNull() ?: 0)
                        "error" -> throw RealtimeException("stream_error", ev.data.ifEmpty { "stream error" })
                        "message" -> if (ev.data.isNotEmpty()) {
                            val o = runCatching { ForgeJson.parseToJsonElement(ev.data) as? JsonObject }.getOrNull()
                            val msg = kvBin(o?.get("message"))
                            if (o != null && msg != null) emit(KvMessage(o["channel"].str() ?: channel, msg))
                        }
                    }
                }
                if (!reconnect) return@flow
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val final = e is RealtimeException || (e is ApiException && e.status in 400..499 && e.status != 429 && e.status != 408)
                if (final || !reconnect) throw e
            }
            if (opened) wait = 500
            delay(wait)
            wait = minOf(wait * 2, 10_000)
        }
    }
}
