package dev.forgebase

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.cancel
import kotlinx.serialization.json.JsonElement
import okhttp3.OkHttpClient
import okhttp3.RequestBody
import java.util.concurrent.TimeUnit

/** Options of [ForgeBase]. */
public class ForgeBaseOptions(
    /** Static API key / service-account key / personal access token: sent as `X-API-Key`. */
    public val apiKey: String? = null,
    /** Static bearer access token (no refresh). */
    public val accessToken: String? = null,
    /** Where a signed-in session is kept between process restarts (default: in memory). */
    public val tokenStore: TokenStore = InMemoryTokenStore(),
    /** Refresh the access token automatically (default true). */
    public val autoRefresh: Boolean = true,
    /** Refresh the access token this long (ms) before it expires (default 30 000). */
    public val refreshSkewMs: Long = 30_000,
    /** Retry policy for 429 / 503 (and idempotent 502 / 504 and network failures). */
    public val retry: RetryOptions = RetryOptions(),
    /** Headers sent with every request (`X-Forgebase-Org`, tenant selection, ...). */
    public val headers: Map<String, String> = emptyMap(),
    /** Read replicas: [Consistency.SESSION] (default) reads your own writes; [Consistency.STRONG] reads the primary. */
    public val consistency: Consistency = Consistency.SESSION,
    /** Your own OkHttp client (proxies, certificate pinning, interceptors); the SDK derives its own from it. */
    public val httpClient: OkHttpClient? = null,
    /** Realtime reconnect policy. */
    public val reconnect: ReconnectOptions = ReconnectOptions(),
    /** `User-Agent` sent with requests (default `forgebase-kotlin/<version>`). */
    public val userAgent: String? = null,
    /** Test hook. */
    public val clock: () -> Long = { System.currentTimeMillis() },
)

/**
 * Entry point: `val fb = ForgeBase("https://api.example.com", ForgeBaseOptions(apiKey = "fb_..."))`.
 *
 * Every call is a `suspend` function (cancellable) and every failure is a [ForgeBaseException]. Call [close] when done
 * (it stops the realtime connection and background work); the OkHttp client is not shut down when you pass your own.
 */
public class ForgeBase(baseUrl: String, options: ForgeBaseOptions = ForgeBaseOptions()) : AutoCloseable {
    private val scope: CoroutineScope = newAuthScope()
    private val mgr: AuthManager

    /** The HTTP layer, for routes the typed APIs do not cover; see also [request]. */
    public val transport: Transport

    public val auth: AuthApi
    public val collections: CollectionsApi
    public val storage: StorageApi
    public val realtime: RealtimeClient
    public val functions: FunctionsApi
    public val kv: KvApi
    public val queues: QueuesApi
    public val jobs: JobsApi
    public val entitlements: EntitlementsApi
    public val billing: BillingApi
    public val vector: VectorApi
    public val graph: GraphApi
    public val sql: SqlApi
    public val credits: CreditsApi

    init {
        val base = (options.httpClient ?: OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .build())
        val http = base.newBuilder().build()
        val headers = LinkedHashMap<String, String>()
        headers["User-Agent"] = options.userAgent ?: "forgebase-kotlin/$VERSION"
        headers.putAll(options.headers)
        transport = Transport(baseUrl, http, options.retry, headers, options.consistency, CommitTokens())
        mgr = AuthManager(
            transport, options.tokenStore, options.apiKey, options.accessToken, options.autoRefresh, options.refreshSkewMs, options.clock, scope,
        )
        transport.credentials = mgr
        auth = AuthApi(transport, mgr)
        collections = CollectionsApi(transport)
        storage = StorageApi(transport)
        realtime = RealtimeClient(transport, mgr, options.reconnect, scope, options.retry.random)
        functions = FunctionsApi(transport)
        kv = KvApi(transport)
        queues = QueuesApi(transport)
        jobs = JobsApi(transport, realtime)
        entitlements = EntitlementsApi(transport)
        billing = BillingApi(transport)
        vector = VectorApi(transport)
        graph = GraphApi(transport)
        sql = SqlApi(transport)
        credits = CreditsApi(transport)
    }

    /** Records of a collection: `fb.collection("posts").list(ListOptions(sort = listOf("-created")))`. */
    public fun collection(name: String): RecordsApi = RecordsApi(transport, name)

    /**
     * Commit tokens of this client's writes, sent as `X-ForgeBase-Read-After`. Realtime events that carry a position
     * feed it automatically.
     */
    public val commitTokens: CommitTokens get() = transport.commitTokens

    /** Typed escape hatch for any route: `fb.request("/api/v1/kv/_info")`. */
    public suspend fun request(
        path: String,
        method: String = "GET",
        query: Map<String, Any?>? = null,
        body: JsonElement? = null,
        rawBody: RequestBody? = null,
        headers: Map<String, String> = emptyMap(),
        anonymous: Boolean = false,
    ): JsonElement = transport.request(path, method, query, body, rawBody, headers, anonymous)

    /** Server version and build info. */
    public suspend fun systemInfo(): JsonElement = transport.request("/api/system/info")

    /** Liveness probe: `true` when the server answers. */
    public suspend fun health(): Boolean = try {
        transport.request("/health", anonymous = true, retry = false)
        true
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (_: Exception) {
        false
    }

    /** Stops the realtime connection and background work. */
    override fun close() {
        realtime.close()
        scope.cancel()
    }

    public companion object {
        /** SDK version. */
        public const val VERSION: String = "0.1.0"
    }
}
