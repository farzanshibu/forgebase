package dev.forgebase

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlin.test.AfterTest

internal fun json(body: String, code: Int = 200): MockResponse =
    MockResponse().setResponseCode(code).setHeader("Content-Type", "application/json").setBody(body)

internal fun errorBody(code: String, message: String, details: String? = null): String =
    """{"error":{"code":"$code","message":"$message","request_id":"req_1"${if (details != null) ""","details":$details""" else ""}}}"""

internal const val SESSION_JSON = """{"token_type":"Bearer","access_token":"at1","expires_in":900,"refresh_token":"rt1","session_id":"s1","session_expires_at":"2026-12-01T00:00:00Z","subject":{"kind":"user","collection":"users","id":"u1"},"user":{"id":"u1","email":"a@b.c"}}"""

internal fun sessionJson(access: String, refresh: String, expiresIn: Int = 900): String =
    """{"token_type":"Bearer","access_token":"$access","expires_in":$expiresIn,"refresh_token":"$refresh","session_id":"s1","session_expires_at":"2026-12-01T00:00:00Z","subject":{"kind":"user","collection":"users","id":"u1"}}"""

/** Runs a suspend test body with a hard timeout. */
internal fun <T> blocking(timeoutMs: Long = 20_000, body: suspend CoroutineScope.() -> T): T = runBlocking { withTimeout(timeoutMs) { body() } }

internal fun RecordedRequest.jsonBody(): JsonObject = ForgeJson.parseToJsonElement(body.readUtf8()).jsonObject

/** A MockWebServer plus a client that never really sleeps between retries. */
open class ServerTest {
    val server = MockWebServer().also { it.start() }
    val sleeps = CopyOnWriteArrayList<Long>()
    private val clients = mutableListOf<ForgeBase>()

    fun client(
        apiKey: String? = null,
        accessToken: String? = null,
        tokenStore: TokenStore = InMemoryTokenStore(),
        consistency: Consistency = Consistency.SESSION,
        maxRetries: Int = 3,
        reconnect: ReconnectOptions = ReconnectOptions(baseDelayMs = 5, maxDelayMs = 20),
        headers: Map<String, String> = emptyMap(),
        clock: () -> Long = { System.currentTimeMillis() },
    ): ForgeBase = ForgeBase(
        server.url("/").toString().trimEnd('/'),
        ForgeBaseOptions(
            apiKey = apiKey, accessToken = accessToken, tokenStore = tokenStore, consistency = consistency, reconnect = reconnect, headers = headers,
            retry = RetryOptions(maxRetries = maxRetries, sleep = { sleeps.add(it) }, random = { 0.5 }), clock = clock,
        ),
    ).also { clients.add(it) }

    fun next(): RecordedRequest = server.takeRequest(5, TimeUnit.SECONDS) ?: error("no request received")

    @AfterTest
    fun shutdown() {
        clients.forEach { it.close() }
        runCatching { server.shutdown() }
    }
}
