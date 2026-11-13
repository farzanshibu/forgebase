package dev.forgebase

import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.SocketPolicy
import kotlinx.serialization.json.JsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class TransportTest : ServerTest() {
    @Test
    fun `sends accept, user agent and default headers, api key goes in X-API-Key`() = blocking {
        server.enqueue(json("""{"ok":true}"""))
        client(apiKey = "fb_key", headers = mapOf("X-Forgebase-Org" to "acme")).request("/api/x")
        val r = next()
        assertEquals("fb_key", r.getHeader("X-API-Key"))
        assertEquals("acme", r.getHeader("X-Forgebase-Org"))
        assertEquals("application/json", r.getHeader("Accept"))
        assertTrue(r.getHeader("User-Agent")!!.startsWith("forgebase-kotlin/"))
        assertNull(r.getHeader("Authorization"))
    }

    @Test
    fun `access token goes in Authorization`() = blocking {
        server.enqueue(json("{}"))
        client(accessToken = "tok").request("/api/x")
        assertEquals("Bearer tok", next().getHeader("Authorization"))
    }

    @Test
    fun `maps every error status to a typed exception with the server envelope`() = blocking {
        val fb = client(maxRetries = 0)
        val cases = listOf(
            400 to ValidationException::class, 422 to ValidationException::class, 401 to AuthenticationException::class,
            403 to PermissionException::class, 404 to NotFoundException::class, 409 to ConflictException::class,
            412 to ConflictException::class, 429 to RateLimitException::class, 500 to ServerException::class, 418 to ApiException::class,
        )
        for ((status, type) in cases) {
            server.enqueue(json(errorBody("some_code", "boom"), status))
            val e = assertFailsWith<ApiException> { fb.request("/api/x", anonymous = true) }
            assertEquals(type, e::class, "status $status")
            assertEquals(status, e.status)
            assertEquals("some_code", e.code)
            assertEquals("boom", e.message)
            assertEquals("req_1", e.requestId)
        }
    }

    @Test
    fun `quota 429 is a QuotaExceededException and is never retried`() = blocking {
        server.enqueue(json(errorBody("quota_exceeded", "no more", """{"feature":"x"}"""), 429))
        val e = assertFailsWith<QuotaExceededException> { client().request("/api/x") }
        assertEquals("x", (e.details as JsonObject)["feature"]?.toString()?.trim('"'))
        assertEquals(1, server.requestCount)
    }

    @Test
    fun `validation exceptions expose per-field errors`() = blocking {
        server.enqueue(json(errorBody("validation_failed", "bad", """{"fields":{"email":{"code":"invalid","message":"not an email"}}}"""), 400))
        val e = assertFailsWith<ValidationException> { client().request("/api/x") }
        assertEquals("not an email", e.fields["email"]?.message)
    }

    @Test
    fun `a non JSON error body still produces an exception`() = blocking {
        server.enqueue(MockResponse().setResponseCode(502).setBody("<html>bad gateway</html>"))
        server.enqueue(MockResponse().setResponseCode(502).setBody("<html>bad gateway</html>"))
        val e = assertFailsWith<ServerException> { client(maxRetries = 1).request("/api/x") }
        assertEquals("http_error", e.code)
        assertEquals("HTTP 502", e.message)
    }

    @Test
    fun `retries 503 with Retry-After and then succeeds`() = blocking {
        server.enqueue(json(errorBody("unavailable", "later"), 503).setHeader("Retry-After", "2"))
        server.enqueue(json("""{"ok":true}"""))
        val r = client().request("/api/x", method = "POST", body = JsonObject(emptyMap()))
        assertIs<JsonObject>(r)
        assertEquals(2, server.requestCount)
        assertEquals(listOf(2000L), sleeps.toList())
    }

    @Test
    fun `backs off with jitter when there is no Retry-After`() = blocking {
        repeat(3) { server.enqueue(json(errorBody("rate_limited", "slow"), 429)) }
        server.enqueue(json("{}"))
        client().request("/api/x")
        // random() = 0.5: floor(0.5 * min(10000, 250 * 2^(n-1)))
        assertEquals(listOf(125L, 250L, 500L), sleeps.toList())
    }

    @Test
    fun `gives up after maxRetries and throws RateLimitException with retryAfter`() = blocking {
        repeat(4) { server.enqueue(json(errorBody("rate_limited", "slow"), 429).setHeader("Retry-After", "1")) }
        val e = assertFailsWith<RateLimitException> { client().request("/api/x") }
        assertEquals(1.0, e.retryAfter)
        assertEquals(4, server.requestCount)
    }

    @Test
    fun `a Retry-After longer than the cap fails at once`() = blocking {
        server.enqueue(json(errorBody("rate_limited", "slow"), 429).setHeader("Retry-After", "3600"))
        assertFailsWith<RateLimitException> { client().request("/api/x") }
        assertEquals(1, server.requestCount)
        assertTrue(sleeps.isEmpty())
    }

    @Test
    fun `502 is retried for GET but not for POST`() = blocking {
        server.enqueue(json("{}", 502))
        server.enqueue(json("{}"))
        client().request("/api/x")
        assertEquals(2, server.requestCount)
        server.enqueue(json(errorBody("bad_gateway", "x"), 502))
        assertFailsWith<ServerException> { client().request("/api/y", method = "POST", body = JsonObject(emptyMap())) }
        assertEquals(3, server.requestCount)
    }

    @Test
    fun `network failures are retried for idempotent methods only`() = blocking {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AT_START))
        server.enqueue(json("""{"a":1}"""))
        client().request("/api/x")
        assertEquals(2, server.requestCount)

        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AT_START))
        assertFailsWith<NetworkException> { client().request("/api/x", method = "POST", body = JsonObject(emptyMap())) }
        assertEquals(3, server.requestCount)
    }

    @Test
    fun `an unreachable server is a NetworkException`() = blocking {
        val fb = client(maxRetries = 0)
        server.shutdown()
        assertFailsWith<NetworkException> { fb.request("/api/x") }
    }

    @Test
    fun `204 and empty bodies give JsonNull, invalid JSON is an InvalidResponseException`() = blocking {
        val fb = client()
        server.enqueue(MockResponse().setResponseCode(204))
        assertEquals(kotlinx.serialization.json.JsonNull, fb.request("/api/x", method = "DELETE"))
        server.enqueue(MockResponse().setResponseCode(200).setBody("not json"))
        assertFailsWith<InvalidResponseException> { fb.request("/api/x") }
    }

    @Test
    fun `commit tokens from responses are sent back as Read-After and strong consistency is a header`() = blocking {
        val fb = client()
        server.enqueue(json("{}").setHeader("X-ForgeBase-Commit", "control/16/B374D848"))
        fb.request("/api/x", method = "POST", body = JsonObject(emptyMap()))
        server.enqueue(json("{}"))
        fb.request("/api/y")
        next()
        val second = next()
        assertEquals("control/16/B374D848", second.getHeader("X-ForgeBase-Read-After"))
        assertNull(second.getHeader("X-ForgeBase-Consistency"))

        val strong = client(consistency = Consistency.STRONG)
        server.enqueue(json("{}"))
        strong.request("/api/z")
        assertEquals("strong", next().getHeader("X-ForgeBase-Consistency"))
    }

    @Test
    fun `tokens of a failed write are kept too`() = blocking {
        val fb = client(maxRetries = 0)
        server.enqueue(json(errorBody("conflict", "x"), 409).setHeader("X-ForgeBase-Commit", "s1/0/9"))
        assertFailsWith<ConflictException> { fb.request("/api/x", method = "POST", body = JsonObject(emptyMap())) }
        assertNotNull(fb.commitTokens.header())
    }

    @Test
    fun `health is false when the server errors`() = blocking {
        val fb = client()
        server.enqueue(MockResponse().setResponseCode(200).setBody("ok"))
        assertTrue(fb.health())
        server.enqueue(MockResponse().setResponseCode(500))
        assertEquals(false, fb.health())
    }
}
