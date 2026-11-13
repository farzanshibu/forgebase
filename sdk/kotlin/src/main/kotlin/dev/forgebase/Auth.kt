package dev.forgebase

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.security.MessageDigest
import java.security.SecureRandom
import okio.ByteString.Companion.toByteString

/** Result of a sign-in: a [Session], or an [MfaChallenge] when a second factor is needed. */
public sealed interface SignInResult

/** A signed-in session. Serialised as JSON into the [TokenStore]. */
@Serializable
public data class Session(
    /** Bearer token. */
    public val accessToken: String? = null,
    /** Rotating refresh token. */
    public val refreshToken: String? = null,
    /** Access token expiry, epoch milliseconds. */
    public val expiresAt: Long,
    public val sessionId: String,
    /** Absolute session expiry (RFC 3339). */
    public val sessionExpiresAt: String? = null,
    /** Who signed in (`{kind:"user",collection,id}` and friends). */
    public val subject: JsonElement? = null,
    /** The user record, for user sign-ins. */
    public val user: JsonObject? = null,
) : SignInResult

/** A second factor is required: call [AuthApi.verifyMfa]. */
public data class MfaChallenge(public val mfaToken: String, /** Seconds the challenge is valid. */ public val expiresIn: Long) : SignInResult

/** Options of the sign-in calls. */
public data class SignInOptions(
    /** User collection (server default `users`). */
    public val collection: String? = null,
    /** Name shown in the device list. */
    public val deviceName: String? = null,
)

/**
 * Where the session is kept between process restarts. The value is a JSON string holding the tokens: store it
 * encrypted (see the README for an `EncryptedSharedPreferences` / DataStore adapter).
 */
public interface TokenStore {
    public suspend fun load(): String?
    public suspend fun save(value: String)
    public suspend fun clear()
}

/** The default store: keeps the session for the life of the client only. */
public class InMemoryTokenStore : TokenStore {
    @Volatile
    private var value: String? = null
    override suspend fun load(): String? = value
    override suspend fun save(value: String) {
        this.value = value
    }

    override suspend fun clear() {
        value = null
    }
}

/** Auth state changes, see [AuthApi.events]. */
public enum class AuthEvent { SIGNED_IN, TOKEN_REFRESHED, SIGNED_OUT }

@Serializable
internal class WireSession(
    val access_token: String? = null,
    val expires_in: Long = 0,
    val refresh_token: String? = null,
    val session_id: String? = null,
    val session_expires_at: String? = null,
    val subject: JsonElement? = null,
    val user: JsonObject? = null,
)

private fun toSession(w: WireSession, now: Long): Session {
    if (w.session_id == null) throw InvalidResponseException("sign-in response carries no session")
    return Session(
        accessToken = w.access_token,
        refreshToken = w.refresh_token,
        expiresAt = now + w.expires_in * 1000,
        sessionId = w.session_id,
        sessionExpiresAt = w.session_expires_at,
        subject = w.subject?.takeIf { it !is JsonNull },
        user = w.user,
    )
}

/** Holds the session and implements the transport's credentials (headers, single-flight refresh, retry after 401). */
internal class AuthManager(
    private val transport: Transport,
    private val store: TokenStore,
    @Volatile var apiKey: String?,
    @Volatile var staticToken: String?,
    private val autoRefresh: Boolean,
    private val refreshSkewMs: Long,
    private val now: () -> Long,
    private val scope: CoroutineScope,
) : Credentials {
    private val state = MutableStateFlow<Session?>(null)
    private val events = MutableSharedFlow<AuthEvent>(extraBufferCapacity = 16)
    private val lock = Mutex()
    private var loaded = false
    private var refreshing: Deferred<Session>? = null

    val session: StateFlow<Session?> = state.asStateFlow()
    val authEvents: SharedFlow<AuthEvent> = events.asSharedFlow()

    private suspend fun load() {
        if (loaded) return
        lock.withLock {
            if (loaded) return
            loaded = true
            try {
                val raw = store.load()
                if (raw != null && state.value == null) state.value = ForgeJson.decodeFromString(Session.serializer(), raw)
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                // unreadable storage: start signed out
            }
        }
    }

    suspend fun getSession(): Session? {
        load()
        return state.value
    }

    suspend fun getAccessToken(): String? {
        staticToken?.let { return it }
        load()
        val s = state.value ?: return null
        val token = s.accessToken ?: return null
        if (autoRefresh && s.refreshToken != null && s.expiresAt - refreshSkewMs <= now()) {
            return try {
                refresh().accessToken
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                token
            }
        }
        return token
    }

    suspend fun setSession(s: Session?, event: AuthEvent = AuthEvent.SIGNED_IN) {
        lock.withLock { loaded = true }
        state.value = s
        try {
            if (s != null) store.save(ForgeJson.encodeToString(Session.serializer(), s)) else store.clear()
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            // storage is best effort
        }
        events.tryEmit(if (s != null) event else AuthEvent.SIGNED_OUT)
    }

    /** Rotates the refresh token. Concurrent callers share one request. */
    suspend fun refresh(): Session {
        val d = lock.withLock {
            refreshing ?: scope.async {
                try {
                    doRefresh()
                } finally {
                    lock.withLock { refreshing = null }
                }
            }.also { refreshing = it }
        }
        return d.await()
    }

    private suspend fun doRefresh(): Session {
        load()
        val rt = state.value?.refreshToken ?: throw InvalidResponseException("no refresh token: sign in first", 401)
        try {
            val w = transport.request("/api/auth/refresh", method = "POST", body = obj("refresh_token" to rt), anonymous = true)
            val next = toSession(w.decode<WireSession>(), now())
            val withUser = if (next.user == null) next.copy(user = state.value?.user) else next
            setSession(withUser, AuthEvent.TOKEN_REFRESHED)
            return withUser
        } catch (e: ForgeBaseException) {
            // A rejected refresh token (revoked, reused) ends the session; transient failures keep it.
            if (e.status == 401) setSession(null)
            throw e
        }
    }

    override suspend fun headers(method: String): Map<String, String> {
        apiKey?.let { return mapOf("X-API-Key" to it) }
        val token = getAccessToken()
        return if (token != null) mapOf("Authorization" to "Bearer $token") else emptyMap()
    }

    override suspend fun onUnauthorized(sent: Map<String, String>): Boolean {
        if (apiKey != null || !autoRefresh || sent["Authorization"] == null) return false
        load()
        val s = state.value ?: return false
        if (s.refreshToken == null) return false
        // Someone else already rotated the token since this request was built: just retry.
        if (s.accessToken != null && sent["Authorization"] != "Bearer ${s.accessToken}") return true
        return try {
            refresh()
            true
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            false
        }
    }

    /** Converts a sign-in style response and stores the session. */
    suspend fun accept(body: JsonElement): SignInResult {
        val o = body as? JsonObject
        if (o?.get("mfa_required")?.jsonPrimitive?.boolean == true) {
            return MfaChallenge(o["mfa_token"].str() ?: "", o["expires_in"].long() ?: 0)
        }
        val s = toSession(body.decode<WireSession>(), now())
        setSession(s)
        return s
    }
}

/** `client.auth`: sign-up, sign-in flows, refresh and account helpers. */
public class AuthApi internal constructor(private val t: Transport, private val mgr: AuthManager) {
    /** Social / OIDC sign-in. */
    public val oauth: OAuthApi = OAuthApi(t, mgr)

    /** The current session (`null` when signed out); emits on sign-in, refresh and sign-out. Collect it from UI. */
    public val sessionFlow: StateFlow<Session?> get() = mgr.session

    /** Sign-in, refresh and sign-out events. */
    public val events: SharedFlow<AuthEvent> get() = mgr.authEvents

    /** The current session after restoring it from the [TokenStore]. */
    public suspend fun getSession(): Session? = mgr.getSession()

    /** Current access token, refreshed first when it is about to expire. */
    public suspend fun getAccessToken(): String? = mgr.getAccessToken()

    /** Adopts tokens obtained elsewhere. */
    public suspend fun setSession(s: Session?): Unit = mgr.setSession(s)

    /** Uses an API key (`fb_...`) for every request, or `null` to go back to the session. */
    public fun setApiKey(key: String?) {
        mgr.apiKey = key
    }

    /** Creates a user. Does not sign in. */
    public suspend fun signUp(email: String, password: String, collection: String? = null, data: Map<String, Any?>? = null): JsonObject {
        val r = t.request("/api/auth/signup", "POST", body = obj("email" to email, "password" to password, "collection" to collection, "data" to data), anonymous = true)
        return (r as? JsonObject)?.get("user") as? JsonObject ?: throw InvalidResponseException("sign-up response carries no user")
    }

    /** Email + password. Returns a [Session], or an [MfaChallenge] when a second factor is needed. */
    public suspend fun signIn(email: String, password: String, options: SignInOptions = SignInOptions()): SignInResult =
        mgr.accept(post("/api/auth/login", wire(options, "email" to email, "password" to password)))

    /** Administrator sign-in (`/api/auth/admins/login`). */
    public suspend fun signInAdmin(email: String, password: String, deviceName: String? = null): SignInResult =
        mgr.accept(post("/api/auth/admins/login", obj("email" to email, "password" to password, "device_name" to deviceName)))

    /** Second step after [signIn] returned an [MfaChallenge]: a TOTP or recovery code. */
    public suspend fun verifyMfa(mfaToken: String, code: String): Session {
        val r = mgr.accept(post("/api/auth/mfa/verify", obj("mfa_token" to mfaToken, "code" to code)))
        return r as? Session ?: throw InvalidResponseException("MFA verification did not return a session")
    }

    /** Emails a magic link; pass the token from its URL to [confirmMagicLink]. */
    public suspend fun requestMagicLink(email: String, collection: String? = null) {
        post("/api/auth/magic-link/request", obj("email" to email, "collection" to collection))
    }

    public suspend fun confirmMagicLink(token: String, deviceName: String? = null): SignInResult =
        mgr.accept(post("/api/auth/magic-link/confirm", obj("token" to token, "device_name" to deviceName)))

    /** Emails a one-time code. */
    public suspend fun requestOtp(email: String, collection: String? = null) {
        post("/api/auth/otp/request", obj("email" to email, "collection" to collection))
    }

    public suspend fun verifyOtp(email: String, code: String, options: SignInOptions = SignInOptions()): SignInResult =
        mgr.accept(post("/api/auth/otp/verify", wire(options, "email" to email, "code" to code)))

    /** Rotates the refresh token and returns the new session (also done automatically, one request at a time). */
    public suspend fun refresh(): Session = mgr.refresh()

    /** Revokes the session on the server and forgets it locally. */
    public suspend fun signOut() {
        try {
            if (mgr.getSession() != null) t.request("/api/auth/logout", "POST")
        } finally {
            mgr.setSession(null)
            t.commitTokens.clear()
        }
    }

    /** The caller as the server sees it (`subject`, `credential`, `mfa`, scopes...). */
    public suspend fun me(): JsonObject = t.request("/api/auth/me") as? JsonObject ?: JsonObject(emptyMap())

    public suspend fun requestPasswordReset(email: String, collection: String? = null) {
        post("/api/auth/password-reset/request", obj("email" to email, "collection" to collection))
    }

    public suspend fun confirmPasswordReset(token: String, password: String) {
        post("/api/auth/password-reset/confirm", obj("token" to token, "password" to password))
    }

    public suspend fun requestEmailVerification(email: String, collection: String? = null) {
        post("/api/auth/verify-email/request", obj("email" to email, "collection" to collection))
    }

    public suspend fun confirmEmailVerification(token: String) {
        post("/api/auth/verify-email/confirm", obj("token" to token))
    }

    public suspend fun changePassword(current: String, new: String) {
        t.request("/api/auth/password/change", "POST", body = obj("current_password" to current, "new_password" to new))
    }

    /** Personal access token: the secret is only returned here. */
    public suspend fun createToken(name: String, scopes: List<String>, expiresInDays: Int? = null): JsonObject =
        t.request("/api/auth/tokens", "POST", body = obj("name" to name, "scopes" to scopes, "expires_in_days" to expiresInDays)) as JsonObject

    public suspend fun listSessions(): JsonElement = t.request("/api/auth/sessions")

    public suspend fun revokeSession(id: String) {
        t.request("/api/auth/sessions/${seg(id)}", "DELETE")
    }

    private suspend fun post(path: String, body: JsonObject): JsonElement = t.request(path, "POST", body = body, anonymous = true)

    private fun wire(o: SignInOptions, vararg base: Pair<String, Any?>): JsonObject =
        obj(*base, "collection" to o.collection, "device_name" to o.deviceName)
}

/** A PKCE pair (RFC 7636, `S256`). */
public class Pkce(public val verifier: String, public val challenge: String) {
    public val method: String get() = "S256"

    public companion object {
        /** A fresh random verifier (32 bytes, base64url) and its challenge. */
        public fun generate(random: SecureRandom = SecureRandom()): Pkce {
            val bytes = ByteArray(32).also(random::nextBytes)
            return of(bytes.toByteString().base64Url().trimEnd('='))
        }

        /** The challenge of a given verifier: `base64url(sha256(verifier))`. */
        public fun of(verifier: String): Pkce {
            val digest = MessageDigest.getInstance("SHA-256").digest(verifier.toByteArray(Charsets.US_ASCII))
            return Pkce(verifier, digest.toByteString().base64Url().trimEnd('='))
        }
    }
}

/** Where to send the user and the `state` to check when the provider redirects back. */
public data class OAuthStart(public val authorizationUrl: String, public val state: String)

/**
 * Social / OIDC sign-in (ADR-0009). The server runs the authorization-code flow with PKCE (`S256`), state and nonce
 * against the provider; the app opens [OAuthStart.authorizationUrl] (an Android Custom Tab) and hands the `code` and
 * `state` the redirect carries back to [complete].
 */
public class OAuthApi internal constructor(private val t: Transport, private val mgr: AuthManager) {
    /** Providers configured on the server. */
    public suspend fun providers(): List<JsonElement> = t.request("/api/auth/oauth/providers", anonymous = true).items("providers")

    /** Starts a sign-in: returns the provider URL and the `state` (single use, 10 minutes). */
    public suspend fun start(provider: String, options: SignInOptions = SignInOptions()): OAuthStart {
        val r = t.request(
            "/api/auth/oauth/${seg(provider)}/start", "POST",
            body = obj("collection" to options.collection, "device_name" to options.deviceName), anonymous = true,
        )
        return OAuthStart(
            r.objOrNull()?.get("authorization_url").str() ?: throw InvalidResponseException("no authorization_url"),
            r.objOrNull()?.get("state").str() ?: throw InvalidResponseException("no state"),
        )
    }

    /** Finishes the flow with the `code` and `state` the provider sent to your redirect URI. */
    public suspend fun complete(provider: String, code: String, state: String): SignInResult {
        val r = t.request("/api/auth/oauth/${seg(provider)}/callback", query = mapOf("code" to code, "state" to state), anonymous = true)
        return mgr.accept(r)
    }

    /** Parses `?code=...&state=...` (or `?error=...`) from the redirect URL and completes the flow. */
    public suspend fun completeFromUrl(provider: String, redirectUrl: String): SignInResult {
        val u = redirectUrl.toHttpUrl()
        val error = u.queryParameter("error")
        val code = u.queryParameter("code")
        val state = u.queryParameter("state")
        if (error != null || code == null || state == null) {
            throw InvalidResponseException(if (error != null) "provider error: $error" else "redirect URL has no code/state", 400)
        }
        return complete(provider, code, state)
    }

    /** Starts linking a provider to the signed-in user. */
    public suspend fun link(provider: String): OAuthStart {
        val r = t.request("/api/auth/oauth/${seg(provider)}/link", "POST")
        return OAuthStart(
            r.objOrNull()?.get("authorization_url").str() ?: throw InvalidResponseException("no authorization_url"),
            r.objOrNull()?.get("state").str() ?: throw InvalidResponseException("no state"),
        )
    }

    public suspend fun identities(): JsonElement = t.request("/api/auth/oauth/identities")

    public suspend fun unlink(id: String) {
        t.request("/api/auth/oauth/identities/${seg(id)}", "DELETE")
    }
}

internal fun newAuthScope(): CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
