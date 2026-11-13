package dev.forgebase

import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.coroutineScope
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.RecordedRequest
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AuthTest : ServerTest() {
    @Test
    fun `signUp creates a user without signing in`() = blocking {
        server.enqueue(json("""{"user":{"id":"u1","email":"a@b.c"}}""", 201))
        val fb = client()
        val user = fb.auth.signUp("a@b.c", "pw12345678", data = mapOf("name" to "A"))
        assertEquals("u1", user["id"]?.jsonPrimitive?.content)
        val r = next()
        assertEquals("/api/auth/signup", r.path)
        assertEquals("A", (r.jsonBody()["data"] as kotlinx.serialization.json.JsonObject)["name"]?.jsonPrimitive?.content)
        assertNull(fb.auth.getSession())
    }

    @Test
    fun `signIn stores the session and requests carry the bearer token`() = blocking {
        server.enqueue(json(SESSION_JSON))
        server.enqueue(json("{}"))
        val fb = client()
        val s = fb.auth.signIn("a@b.c", "pw", SignInOptions(collection = "members", deviceName = "pixel"))
        assertIs<Session>(s)
        assertEquals("at1", s.accessToken)
        assertEquals("rt1", s.refreshToken)
        val login = next()
        val body = login.jsonBody()
        assertEquals("members", body["collection"]?.jsonPrimitive?.content)
        assertEquals("pixel", body["device_name"]?.jsonPrimitive?.content)
        assertNull(login.getHeader("Authorization"))
        fb.request("/api/auth/me")
        assertEquals("Bearer at1", next().getHeader("Authorization"))
        assertEquals("s1", fb.auth.sessionFlow.value?.sessionId)
    }

    @Test
    fun `MFA challenge then verifyMfa`() = blocking {
        server.enqueue(json("""{"mfa_required":true,"mfa_token":"mt_1","expires_in":300}"""))
        server.enqueue(json(SESSION_JSON))
        val fb = client()
        val c = fb.auth.signIn("a@b.c", "pw")
        assertIs<MfaChallenge>(c)
        assertEquals("mt_1", c.mfaToken)
        assertEquals(300, c.expiresIn)
        assertNull(fb.auth.getSession())
        val s = fb.auth.verifyMfa(c.mfaToken, "123456")
        assertEquals("at1", s.accessToken)
        next()
        val v = next()
        assertEquals("/api/auth/mfa/verify", v.path)
        assertEquals("123456", v.jsonBody()["code"]?.jsonPrimitive?.content)
    }

    @Test
    fun `an expiring access token is refreshed before the request`() = blocking {
        var now = 1_000_000L
        server.enqueue(json(sessionJson("at1", "rt1", 100)))
        server.enqueue(json(sessionJson("at2", "rt2", 900)))
        server.enqueue(json("{}"))
        val fb = client(clock = { now })
        fb.auth.signIn("a@b.c", "pw")
        now += 80_000 // 20 s left, inside the 30 s skew
        fb.request("/api/x")
        next()
        val refresh = next()
        assertEquals("/api/auth/refresh", refresh.path)
        assertEquals("rt1", refresh.jsonBody()["refresh_token"]?.jsonPrimitive?.content)
        assertEquals("Bearer at2", next().getHeader("Authorization"))
        assertEquals("rt2", fb.auth.getSession()?.refreshToken)
    }

    @Test
    fun `concurrent refreshes share one request (single flight)`() = blocking {
        val refreshes = AtomicInteger()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                "/api/auth/login" -> json(sessionJson("at1", "rt1", 1))
                "/api/auth/refresh" -> {
                    refreshes.incrementAndGet()
                    Thread.sleep(150)
                    json(sessionJson("at2", "rt2", 900))
                }
                else -> json("{}")
            }
        }
        val fb = client()
        fb.auth.signIn("a@b.c", "pw")
        val results = (1..8).map { async { fb.auth.refresh() } }.awaitAll()
        assertEquals(1, refreshes.get())
        assertTrue(results.all { it.accessToken == "at2" })
        // five more requests while the token is stale also share one refresh
        val fb2 = client()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                "/api/auth/login" -> json(sessionJson("a1", "r1", 1))
                "/api/auth/refresh" -> {
                    refreshes.incrementAndGet()
                    Thread.sleep(150)
                    json(sessionJson("a2", "r2", 900))
                }
                else -> json("{}")
            }
        }
        refreshes.set(0)
        fb2.auth.signIn("a@b.c", "pw")
        (1..5).map { async { fb2.request("/api/x") } }.awaitAll()
        assertEquals(1, refreshes.get())
    }

    @Test
    fun `a 401 refreshes once and retries the request`() = blocking {
        server.enqueue(json(SESSION_JSON))
        server.enqueue(json(errorBody("unauthorized", "expired"), 401))
        server.enqueue(json(sessionJson("at2", "rt2")))
        server.enqueue(json("""{"ok":1}"""))
        val fb = client()
        fb.auth.signIn("a@b.c", "pw")
        fb.request("/api/x")
        next()
        assertEquals("Bearer at1", next().getHeader("Authorization"))
        assertEquals("/api/auth/refresh", next().path)
        assertEquals("Bearer at2", next().getHeader("Authorization"))
    }

    @Test
    fun `a rejected refresh token ends the session, a transient failure keeps it`() = blocking {
        server.enqueue(json(SESSION_JSON))
        server.enqueue(json(errorBody("unauthorized", "x"), 401))
        server.enqueue(json(errorBody("invalid_token", "revoked"), 401))
        val fb = client()
        fb.auth.signIn("a@b.c", "pw")
        assertFailsWith<AuthenticationException> { fb.request("/api/x") }
        assertNull(fb.auth.getSession())

        server.enqueue(json(SESSION_JSON))
        server.enqueue(json(errorBody("boom", "x"), 500))
        server.enqueue(json(errorBody("boom", "x"), 500))
        val fb2 = client(maxRetries = 0)
        fb2.auth.signIn("a@b.c", "pw")
        assertFailsWith<ServerException> { fb2.auth.refresh() }
        assertNotNull(fb2.auth.getSession())
    }

    @Test
    fun `the session is restored from the token store`() = blocking {
        val store = InMemoryTokenStore()
        server.enqueue(json(SESSION_JSON))
        client(tokenStore = store).auth.signIn("a@b.c", "pw")
        assertTrue(store.load()!!.contains("rt1"))

        server.enqueue(json("{}"))
        val again = client(tokenStore = store)
        again.request("/api/x")
        next()
        assertEquals("Bearer at1", next().getHeader("Authorization"))
        assertEquals("a@b.c", again.auth.getSession()?.user?.get("email")?.jsonPrimitive?.content)
    }

    @Test
    fun `an unreadable token store starts signed out`() = blocking {
        val store = object : TokenStore {
            override suspend fun load(): String = "{broken"
            override suspend fun save(value: String) = throw IllegalStateException("disk full")
            override suspend fun clear() = Unit
        }
        server.enqueue(json(SESSION_JSON))
        val fb = client(tokenStore = store)
        assertNull(fb.auth.getSession())
        // a failing save does not fail the sign-in
        assertIs<Session>(fb.auth.signIn("a@b.c", "pw"))
    }

    @Test
    fun `signOut revokes on the server and forgets the session and commit tokens`() = blocking {
        val store = InMemoryTokenStore()
        server.enqueue(json(SESSION_JSON))
        server.enqueue(json("{}").setHeader("X-ForgeBase-Commit", "s1/0/9"))
        val fb = client(tokenStore = store)
        fb.auth.signIn("a@b.c", "pw")
        val events = mutableListOf<AuthEvent>()
        val job = launchCollect(fb, events)
        fb.auth.signOut()
        next()
        assertEquals("/api/auth/logout", next().path)
        assertNull(store.load())
        assertNull(fb.commitTokens.header())
        assertNull(fb.auth.sessionFlow.value)
        delay(100)
        job.cancel()
        assertTrue(AuthEvent.SIGNED_OUT in events)
    }

    private fun launchCollect(fb: ForgeBase, into: MutableList<AuthEvent>) =
        kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.Default).launch { fb.auth.events.collect { into.add(it) } }.also { Thread.sleep(50) }

    @Test
    fun `refresh events are emitted`() = blocking {
        server.enqueue(json(sessionJson("at1", "rt1")))
        server.enqueue(json(sessionJson("at2", "rt2")))
        val fb = client()
        coroutineScope {
            val ev = async { fb.auth.events.first { it == AuthEvent.TOKEN_REFRESHED } }
            delay(50)
            fb.auth.signIn("a@b.c", "pw")
            fb.auth.refresh()
            assertEquals(AuthEvent.TOKEN_REFRESHED, ev.await())
        }
    }

    @Test
    fun `api key clients never refresh`() = blocking {
        server.enqueue(json(errorBody("unauthorized", "bad key"), 401))
        assertFailsWith<AuthenticationException> { client(apiKey = "fb_bad").request("/api/x") }
        assertEquals(1, server.requestCount)
    }

    @Test
    fun `magic link, otp, password reset and change password hit the right routes`() = blocking {
        repeat(6) { server.enqueue(json("{}")) }
        server.enqueue(json(SESSION_JSON))
        val fb = client()
        fb.auth.requestMagicLink("a@b.c")
        fb.auth.requestOtp("a@b.c")
        fb.auth.requestPasswordReset("a@b.c")
        fb.auth.confirmPasswordReset("tok", "newpw")
        fb.auth.requestEmailVerification("a@b.c")
        fb.auth.confirmEmailVerification("tok")
        assertIs<Session>(fb.auth.verifyOtp("a@b.c", "123456"))
        assertEquals(
            listOf(
                "/api/auth/magic-link/request", "/api/auth/otp/request", "/api/auth/password-reset/request", "/api/auth/password-reset/confirm",
                "/api/auth/verify-email/request", "/api/auth/verify-email/confirm", "/api/auth/otp/verify",
            ),
            (1..7).map { next().path },
        )
    }

    @Test
    fun `oauth start returns the provider url and complete signs in`() = blocking {
        server.enqueue(json("""{"authorization_url":"https://idp.example/auth?x=1","state":"st_1"}"""))
        server.enqueue(json(SESSION_JSON))
        val fb = client()
        val start = fb.auth.oauth.start("google", SignInOptions(deviceName = "pixel"))
        assertEquals("https://idp.example/auth?x=1", start.authorizationUrl)
        assertEquals("st_1", start.state)
        val s = fb.auth.oauth.completeFromUrl("google", "myapp://oauth/callback?code=abc&state=st_1".replace("myapp://", "https://app.example/"))
        assertIs<Session>(s)
        assertEquals("/api/auth/oauth/google/start", next().path)
        assertEquals("/api/auth/oauth/google/callback?code=abc&state=st_1", next().path)
    }

    @Test
    fun `oauth redirect with an error or without a code is rejected`() = blocking {
        val fb = client()
        assertFailsWith<InvalidResponseException> { fb.auth.oauth.completeFromUrl("google", "https://app.example/cb?error=access_denied") }
        assertFailsWith<InvalidResponseException> { fb.auth.oauth.completeFromUrl("google", "https://app.example/cb?code=1") }
        assertEquals(0, server.requestCount)
    }

    @Test
    fun `pkce matches the RFC 7636 appendix B vector and generates distinct verifiers`() {
        val p = Pkce.of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
        assertEquals("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", p.challenge)
        assertEquals("S256", p.method)
        val a = Pkce.generate()
        val b = Pkce.generate()
        assertNotEquals(a.verifier, b.verifier)
        assertTrue(a.verifier.length in 43..128)
        assertEquals(Pkce.of(a.verifier).challenge, a.challenge)
    }
}
