package dev.forgebase

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

/*
 * Durable topics and queues (`/api/v1/queues`, ADR-0022): `fb.queues`.
 * Publishing, pulling, acknowledging and streaming pull are here; topic and subscription administration is
 * available too, with server shapes returned as JSON.
 */

@Serializable
public data class QueueMessage(
    val id: String,
    val topic: String,
    val seq: Long,
    val data: JsonElement = JsonNull,
    val attributes: Map<String, String> = emptyMap(),
    @SerialName("ordering_key") val orderingKey: String? = null,
    @SerialName("dedup_id") val dedupId: String? = null,
    val publisher: String = "",
    val bytes: Long = 0,
    @SerialName("published_at") val publishedAt: String = "",
)

@Serializable
public data class Published(
    @SerialName("message_id") val messageId: String,
    val seq: Long,
    /** The dedup id was seen inside the window: nothing was stored, [messageId] is the first message's. */
    val duplicate: Boolean = false,
)

/** One delivery of a message, as the server sends it. */
@Serializable
public data class ReceivedMessage(
    @SerialName("ack_id") val ackId: String,
    @SerialName("delivery_attempt") val deliveryAttempt: Int = 1,
    @SerialName("ack_deadline") val ackDeadline: String = "",
    val message: QueueMessage,
) {
    /** Decodes the message data into `T`. */
    public inline fun <reified T> data(): T = message.data.decode()
}

@Serializable
public data class AckResult(
    val count: Int = 0,
    /** Ack ids the server no longer knows (expired and delivered again, or already settled). */
    val unknown: List<String> = emptyList(),
)

public data class PublishOptions(
    val attributes: Map<String, String>? = null,
    /** Messages with one ordering key reach an ordered subscription in publish order. */
    val orderingKey: String? = null,
    /** A repeat inside the topic's dedup window is accepted once. */
    val dedupId: String? = null,
)

public data class PublishMessage(val data: Any?, val options: PublishOptions = PublishOptions())

public data class PullOptions(
    /** 1-1000, default 10. */
    val maxMessages: Int? = null,
    /** Long-poll up to this many milliseconds when nothing is ready. */
    val waitMs: Long? = null,
    val consumer: String? = null,
)

public data class CreateTopicOptions(
    val retentionMs: Long? = null,
    val retentionBytes: Long? = null,
    val maxMessageBytes: Long? = null,
    val dedupWindowMs: Long? = null,
    /** JSON Schema (documented subset) that publishes are validated against. */
    val schema: JsonElement? = null,
    val labels: Map<String, String>? = null,
)

public data class CreateSubscriptionOptions(
    /** Attribute filter, e.g. `{ kind: "rush" }`. */
    val filter: JsonElement? = null,
    val ackDeadlineMs: Long? = null,
    val maxDeliveryAttempts: Int? = null,
    val backoffBaseMs: Long? = null,
    val backoffMaxMs: Long? = null,
    val deadLetterTopic: String? = null,
    val ordering: Boolean? = null,
    /** `{kind: "webhook"|"function", endpoint: ...}`. */
    val push: JsonObject? = null,
    /** `"latest"` (default), `"earliest"` or `{time: ...}`. */
    val start: JsonElement? = null,
    val labels: Map<String, String>? = null,
)

public data class QueueStreamOptions(
    /** Most unacknowledged messages this stream holds (flow control, default 100). */
    val maxMessages: Int? = null,
    /** Most unacknowledged bytes this stream holds (default 10 MiB). */
    val maxBytes: Long? = null,
    /** Ack when the handler returns and nack when it throws (default true). Turn off to settle by hand. */
    val autoAck: Boolean = true,
    /** Reconnect after a dropped stream (default true), with exponential backoff. */
    val reconnect: Boolean = true,
    /** Called with errors that do not end the subscription (a handler that threw, a dropped stream). */
    val onError: ((Throwable) -> Unit)? = null,
)

/** A delivery handed to a [QueuesApi.subscribe] handler. */
public class Delivery internal constructor(
    private val api: QueuesApi,
    private val subscription: String,
    private val received: ReceivedMessage,
) {
    public val message: QueueMessage get() = received.message
    public val data: JsonElement get() = received.message.data
    public val attributes: Map<String, String> get() = received.message.attributes

    /** 1 on the first delivery. */
    public val attempt: Int get() = received.deliveryAttempt
    public val ackId: String get() = received.ackId

    @Volatile
    internal var settled: Boolean = false

    /** Decodes the data into `T`. */
    public inline fun <reified T> decode(): T = data.decode()

    /** Acknowledges now (returning normally from the handler does it for you unless `autoAck` is off). */
    public suspend fun ack() {
        if (settled) return
        settled = true
        api.ack(subscription, listOf(ackId))
    }

    /** Negative acknowledgement: the message is redelivered after the subscription's backoff. */
    public suspend fun nack() {
        if (settled) return
        settled = true
        api.nack(subscription, listOf(ackId))
    }

    /** Gives the handler more time (milliseconds from now, 1 s to 10 min). */
    public suspend fun extend(ackDeadlineMs: Long) {
        api.modifyDeadline(subscription, listOf(ackId), ackDeadlineMs)
    }
}

private const val Q = "/api/v1/queues"

/** Topics, subscriptions, publish, pull and streaming pull: `fb.queues`. */
public class QueuesApi internal constructor(private val t: Transport) {
    // ----------------------------------------------------------------- topics

    public suspend fun listTopics(): List<JsonObject> = t.request("$Q/topics").items().map { it as JsonObject }
    public suspend fun getTopic(name: String): JsonObject = t.request("$Q/topics/${seg(name)}") as JsonObject

    public suspend fun createTopic(name: String, o: CreateTopicOptions = CreateTopicOptions()): JsonObject = t.request(
        "$Q/topics", "POST",
        body = obj(
            "name" to name, "retention_ms" to o.retentionMs, "retention_bytes" to o.retentionBytes,
            "max_message_bytes" to o.maxMessageBytes, "dedup_window_ms" to o.dedupWindowMs, "schema" to o.schema, "labels" to o.labels,
        ),
    ) as JsonObject

    public suspend fun deleteTopic(name: String, force: Boolean = false) {
        t.request("$Q/topics/${seg(name)}", "DELETE", query = mapOf("force" to if (force) true else null))
    }

    public suspend fun topicMetrics(name: String): JsonObject = t.request("$Q/topics/${seg(name)}/metrics") as JsonObject

    // ---------------------------------------------------------------- publish

    /** Publishes one message ([data]: any JSON-able value). Returns its id and sequence number. */
    public suspend fun publish(topic: String, data: Any?, o: PublishOptions = PublishOptions()): Published =
        publishBatch(topic, listOf(PublishMessage(data, o))).firstOrNull() ?: throw InvalidResponseException("publish returned no result")

    /** Publishes a `@Serializable` value as the message data. */
    public suspend inline fun <reified T> publishAs(topic: String, value: T, o: PublishOptions = PublishOptions()): Published =
        publish(topic, ForgeJson.encodeToJsonElement(kotlinx.serialization.serializer<T>(), value), o)

    /** Publishes up to 1000 messages in one request; results are in the order sent. */
    public suspend fun publishBatch(topic: String, messages: List<PublishMessage>): List<Published> {
        val body = obj(
            "messages" to messages.map {
                obj("data" to it.data, "attributes" to it.options.attributes, "ordering_key" to it.options.orderingKey, "dedup_id" to it.options.dedupId)
                    .let { o -> if ("data" in o) o else JsonObject(o + ("data" to JsonNull)) }
            },
        )
        // A dedup id makes a repeat harmless, so those publishes are safe to retry after a network failure.
        val retry = messages.isNotEmpty() && messages.all { it.options.dedupId != null }
        return t.request("$Q/topics/${seg(topic)}/publish", "POST", body = body, retry = retry).items("results").map { it.decode() }
    }

    // ---------------------------------------------------------- subscriptions

    public suspend fun listSubscriptions(topic: String? = null): List<JsonObject> =
        t.request("$Q/subscriptions", query = mapOf("topic" to topic)).items().map { it as JsonObject }

    public suspend fun getSubscription(name: String): JsonObject = t.request("$Q/subscriptions/${seg(name)}") as JsonObject

    public suspend fun createSubscription(name: String, topic: String, o: CreateSubscriptionOptions = CreateSubscriptionOptions()): JsonObject = t.request(
        "$Q/subscriptions", "POST",
        body = obj(
            "name" to name, "topic" to topic, "filter" to o.filter, "ack_deadline_ms" to o.ackDeadlineMs,
            "max_delivery_attempts" to o.maxDeliveryAttempts, "backoff_base_ms" to o.backoffBaseMs, "backoff_max_ms" to o.backoffMaxMs,
            "dead_letter_topic" to o.deadLetterTopic, "ordering" to o.ordering, "push" to o.push, "start" to o.start, "labels" to o.labels,
        ),
    ) as JsonObject

    public suspend fun deleteSubscription(name: String) {
        t.request("$Q/subscriptions/${seg(name)}", "DELETE")
    }

    public suspend fun subscriptionMetrics(name: String): JsonObject = t.request("$Q/subscriptions/${seg(name)}/metrics") as JsonObject

    /** Messages that used up their delivery attempts, newest first. */
    public suspend fun deadLetters(subscription: String, before: Long? = null, limit: Int? = null): List<JsonObject> =
        t.request("$Q/subscriptions/${seg(subscription)}/dead-letters", query = mapOf("before" to before, "limit" to limit)).items().map { it as JsonObject }

    // ------------------------------------------------------------ pull and ack

    /** Takes up to `maxMessages` ready messages (leasing them for the subscription's ack deadline). Acknowledge each with [ack]. */
    public suspend fun pull(subscription: String, o: PullOptions = PullOptions()): List<ReceivedMessage> =
        // Pulling leases messages: a blind retry could lease them twice, so it is not retried.
        t.request(
            "$Q/subscriptions/${seg(subscription)}/pull", "POST", retry = false,
            body = obj("max_messages" to o.maxMessages, "wait_ms" to o.waitMs, "consumer" to o.consumer),
        ).items("messages").map { it.decode() }

    public suspend fun ack(subscription: String, ackIds: List<String>): AckResult =
        t.request("$Q/subscriptions/${seg(subscription)}/ack", "POST", body = obj("ack_ids" to ackIds)).decode()

    public suspend fun nack(subscription: String, ackIds: List<String>): AckResult =
        t.request("$Q/subscriptions/${seg(subscription)}/nack", "POST", body = obj("ack_ids" to ackIds)).decode()

    /** Gives deliveries more (or less) time before they return to the queue. */
    public suspend fun modifyDeadline(subscription: String, ackIds: List<String>, ackDeadlineMs: Long): AckResult =
        t.request("$Q/subscriptions/${seg(subscription)}/modify-deadline", "POST", body = obj("ack_ids" to ackIds, "ack_deadline_ms" to ackDeadlineMs)).decode()

    // --------------------------------------------------------- streaming pull

    /**
     * Streaming pull with acknowledgement: [handler] runs for every delivery as the server pushes it (server-sent
     * events, flow-controlled by `maxMessages` / `maxBytes`). When the handler returns the message is acked; when it
     * throws it is nacked and redelivered after the subscription's backoff (`autoAck = false` leaves both to the handler).
     * Suspends until the coroutine is cancelled; a dropped stream reconnects with backoff, a refusal (missing
     * permission, push subscription, no such subscription) throws.
     */
    public suspend fun subscribe(subscription: String, o: QueueStreamOptions = QueueStreamOptions(), handler: suspend (Delivery) -> Unit) {
        var wait = 500L
        while (true) {
            var opened = false
            try {
                coroutineScope {
                    val res = t.send(
                        "$Q/subscriptions/${seg(subscription)}/events", headers = mapOf("Accept" to "text/event-stream"),
                        query = mapOf("max_messages" to o.maxMessages, "max_bytes" to o.maxBytes), retry = false, streaming = true,
                    )
                    opened = true
                    sseEvents(res).collect { ev ->
                        if (ev.event == "error") {
                            val j = runCatching { ForgeJson.parseToJsonElement(ev.data) as? JsonObject }.getOrNull()
                            throw RealtimeException(j?.get("code").str() ?: "stream_error", j?.get("message").str() ?: ev.data)
                        }
                        if (ev.event != "message" || ev.data.isEmpty()) return@collect
                        val r = runCatching { ForgeJson.decodeFromString(ReceivedMessage.serializer(), ev.data) }.getOrNull() ?: return@collect
                        launch { deliver(subscription, r, o, handler) }
                    }
                }
                if (!o.reconnect) return
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val final = e is RealtimeException || (e is ApiException && e.status in 400..499 && e.status != 429 && e.status != 408)
                if (final || !o.reconnect) throw e
                o.onError?.invoke(e)
            }
            if (opened) wait = 500
            delay(wait)
            wait = minOf(wait * 2, 10_000)
        }
    }

    private suspend fun deliver(subscription: String, r: ReceivedMessage, o: QueueStreamOptions, handler: suspend (Delivery) -> Unit) {
        val d = Delivery(this, subscription, r)
        try {
            handler(d)
            if (o.autoAck) settle(o) { d.ack() }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            o.onError?.invoke(e)
            if (o.autoAck) settle(o) { d.nack() }
        }
    }

    private suspend fun settle(o: QueueStreamOptions, op: suspend () -> Unit) {
        try {
            op()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            o.onError?.invoke(e)
        }
    }
}
