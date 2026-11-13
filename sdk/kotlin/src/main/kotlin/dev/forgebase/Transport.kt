package dev.forgebase

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** Retry policy for 429 / 503 (and idempotent 502 / 504 and network failures). */
public class RetryOptions(
    /** Retries after the first attempt (default 3). `0` disables retrying. */
    public val maxRetries: Int = 3,
    /** First backoff delay in ms (default 250); doubles each attempt with full jitter. */
    public val baseDelayMs: Long = 250,
    /** Upper bound for one backoff delay in ms (default 10 000). */
    public val maxDelayMs: Long = 10_000,
    /** Longest `Retry-After` the SDK will sleep for, in ms (default 60 000); longer asks fail at once. */
    public val maxRetryAfterMs: Long = 60_000,
    /** Statuses retried for every method (default 429, 503). 502 and 504 are retried for idempotent methods only. */
    public val statuses: Set<Int> = setOf(429, 503),
    /** Test hook. */
    public val sleep: suspend (Long) -> Unit = { delay(it) },
    /** Test hook: returns a value in [0, 1). */
    public val random: () -> Double = { Math.random() },
) {
    public companion object {
        /** No retries. */
        public val NONE: RetryOptions = RetryOptions(maxRetries = 0)
    }
}

/** What the transport needs from the auth layer. */
internal interface Credentials {
    /** Headers for one request (may refresh the access token first). */
    suspend fun headers(method: String): Map<String, String>

    /** Called after a 401 with the headers that were sent: `true` when a retry can succeed. */
    suspend fun onUnauthorized(sent: Map<String, String>): Boolean
}

private val IDEMPOTENT = setOf("GET", "HEAD", "OPTIONS", "PUT", "DELETE")
private val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()

/** Awaits an OkHttp call without blocking a thread; cancelling the coroutine cancels the call. */
internal suspend fun Call.await(): Response = suspendCancellableCoroutine { cont ->
    cont.invokeOnCancellation { runCatching { cancel() } }
    enqueue(object : Callback {
        override fun onFailure(call: Call, e: IOException) {
            if (cont.isActive) cont.resumeWithException(e)
        }

        override fun onResponse(call: Call, response: Response) {
            if (cont.isActive) cont.resume(response) else response.close()
        }
    })
}

/** Reads the body as text (off the caller's thread) and closes the response. */
public suspend fun Response.readText(): String = withContext(Dispatchers.IO) { use { it.body?.string().orEmpty() } }

/** Reads the body as bytes (off the caller's thread) and closes the response. */
public suspend fun Response.readBytes(): ByteArray = withContext(Dispatchers.IO) { use { it.body?.bytes() ?: ByteArray(0) } }

/**
 * The HTTP layer: retries with backoff, credentials, 401 refresh, commit tokens and error mapping.
 * Most code uses the typed APIs; [ForgeBase.request] is the escape hatch for any route.
 */
public class Transport internal constructor(
    baseUrl: String,
    internal val http: okhttp3.OkHttpClient,
    private val retry: RetryOptions,
    internal val defaultHeaders: Map<String, String>,
    @Volatile public var consistency: Consistency,
    /** Commit tokens seen in responses, sent back as `X-ForgeBase-Read-After`. */
    public val commitTokens: CommitTokens,
) {
    public val baseUrl: String = baseUrl.trimEnd('/')

    private val streamingHttp: okhttp3.OkHttpClient by lazy {
        http.newBuilder().readTimeout(java.time.Duration.ofSeconds(90)).callTimeout(java.time.Duration.ZERO).build()
    }

    @Volatile
    internal var credentials: Credentials? = null

    internal fun url(path: String, query: Map<String, Any?>? = null): HttpUrl {
        val b = (baseUrl + path).toHttpUrl().newBuilder()
        query?.forEach { (k, v) -> if (v != null) b.addQueryParameter(k, queryValue(v)) }
        return b.build()
    }

    private fun queryValue(v: Any): String = when (v) {
        is Enum<*> -> v.name.lowercase()
        is Iterable<*> -> v.joinToString(",") { queryValue(it ?: "") }
        else -> v.toString()
    }

    /** Delay before retry number `attempt` (1-based): `Retry-After` when sent, else exponential backoff with full jitter. */
    internal fun backoff(attempt: Int, retryAfterMs: Long?, cfg: RetryOptions): Long {
        if (retryAfterMs != null) return retryAfterMs
        val ceiling = minOf(cfg.maxDelayMs.toDouble(), cfg.baseDelayMs.toDouble() * Math.pow(2.0, (attempt - 1).toDouble()))
        return Math.floor(cfg.random() * ceiling).toLong()
    }

    /**
     * Sends with retries; returns a 2xx [Response] (the caller closes it, e.g. via [readText]) and throws a typed
     * [ForgeBaseException] otherwise.
     */
    public suspend fun send(
        path: String,
        method: String = "GET",
        query: Map<String, Any?>? = null,
        body: JsonElement? = null,
        rawBody: RequestBody? = null,
        headers: Map<String, String> = emptyMap(),
        anonymous: Boolean = false,
        retry: Boolean = true,
        consistency: Consistency? = null,
        retryOptions: RetryOptions? = null,
        /** Long-lived responses (server-sent events): no read timeout beyond the server's keep-alives. */
        streaming: Boolean = false,
    ): Response {
        val m = method.uppercase()
        val cfg = if (!retry) RetryOptions.NONE else retryOptions ?: this.retry
        val url = url(path, query)
        var refreshed = false
        var attempt = 0
        while (true) {
            val h = LinkedHashMap<String, String>()
            h["Accept"] = "application/json"
            h.putAll(defaultHeaders)
            val reqBody: RequestBody? = when {
                rawBody != null -> rawBody
                body != null -> ForgeJson.encodeToString(JsonElement.serializer(), body).toRequestBody(JSON_MEDIA)
                m == "POST" || m == "PUT" || m == "PATCH" -> ByteArray(0).toRequestBody(null)
                else -> null
            }
            if (!anonymous) credentials?.let { h.putAll(it.headers(m)) }
            commitTokens.header()?.let { h[READ_AFTER_HEADER] = it }
            if ((consistency ?: this.consistency) == Consistency.STRONG) h[CONSISTENCY_HEADER] = "strong"
            h.putAll(headers)
            val canResend = reqBody == null || !reqBody.isOneShot()

            val req = Request.Builder().url(url).method(m, reqBody).apply { h.forEach { (k, v) -> header(k, v) } }.build()
            val res: Response = try {
                (if (streaming) streamingHttp else http).newCall(req).await()
            } catch (e: CancellationException) {
                throw e
            } catch (e: IOException) {
                if (attempt < cfg.maxRetries && m in IDEMPOTENT && canResend) {
                    attempt++
                    cfg.sleep(backoff(attempt, null, cfg))
                    continue
                }
                throw NetworkException(e.message ?: "network request failed", e)
            }
            // A failed request may still have committed (a batch, a partial import).
            commitTokens.note(res.header(COMMIT_HEADER))
            if (res.isSuccessful) return res

            if (res.code == 401 && !anonymous && !refreshed && canResend && credentials?.onUnauthorized(h) == true) {
                refreshed = true
                res.close()
                continue
            }

            val idem = m in IDEMPOTENT
            val retriable = res.code in cfg.statuses || ((res.code == 502 || res.code == 504) && idem)
            if (retriable && attempt < cfg.maxRetries && canResend) {
                val ra = parseRetryAfter(res.headers)
                if (ra == null || ra <= cfg.maxRetryAfterMs) {
                    attempt++
                    res.close()
                    cfg.sleep(backoff(attempt, ra, cfg))
                    continue
                }
            }
            val status = res.code
            val hdrs = res.headers
            val text = runCatching { res.readText() }.getOrDefault("")
            val parsed = if (text.isEmpty()) null else runCatching { ForgeJson.parseToJsonElement(text) }.getOrNull()
            throw errorFromResponse(status, parsed, hdrs)
        }
    }

    /** Sends and parses the JSON answer; [JsonNull] when there is none (204 or an empty body). */
    public suspend fun request(
        path: String,
        method: String = "GET",
        query: Map<String, Any?>? = null,
        body: JsonElement? = null,
        rawBody: RequestBody? = null,
        headers: Map<String, String> = emptyMap(),
        anonymous: Boolean = false,
        retry: Boolean = true,
        consistency: Consistency? = null,
    ): JsonElement {
        val res = send(path, method, query, body, rawBody, headers, anonymous, retry, consistency)
        return parseJson(res)
    }

    /** Sends and returns the response text. */
    public suspend fun requestText(
        path: String,
        method: String = "GET",
        query: Map<String, Any?>? = null,
        body: JsonElement? = null,
        rawBody: RequestBody? = null,
        headers: Map<String, String> = emptyMap(),
        anonymous: Boolean = false,
        retry: Boolean = true,
    ): String = send(path, method, query, body, rawBody, headers, anonymous, retry).readText()

    internal suspend fun parseJson(res: Response): JsonElement {
        val status = res.code
        if (status == 204) {
            res.close()
            return JsonNull
        }
        val text = res.readText()
        if (text.isEmpty()) return JsonNull
        return try {
            ForgeJson.parseToJsonElement(text)
        } catch (e: Exception) {
            throw InvalidResponseException("response was not valid JSON", status)
        }
    }
}
