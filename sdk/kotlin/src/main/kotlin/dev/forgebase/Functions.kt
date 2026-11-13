package dev.forgebase

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/*
 * Server functions (`/api/v1/functions`): `fb.functions`. Invoking is what apps do; deploying and managing functions
 * (versions, secrets, KV rules) is a server-admin task left to the JavaScript and Rust SDKs, the CLI and the dashboard.
 */

/** What to send to an invoked function. */
public sealed interface InvokeBody {
    /** Sent as JSON. */
    public data class Json(val value: JsonElement) : InvokeBody

    /** Sent as is, with [contentType] (default `application/octet-stream`). */
    public class Raw(public val bytes: ByteArray, public val contentType: String? = null) : InvokeBody

    /** Sent as `text/plain`. */
    public data class Text(val value: String) : InvokeBody
}

public data class InvokeOptions(
    /** HTTP method (default `POST`); the function sees it in its input. */
    val method: String = "POST",
    /** Sub-path after `/invoke/` (`"users/42"`); the function sees it as its request path. */
    val path: String? = null,
    /** Query string parameters passed to the function. */
    val query: Map<String, Any?>? = null,
    /** Administrators: run this version instead of the active one (a test run). */
    val version: Int? = null,
    val headers: Map<String, String> = emptyMap(),
    /** Retries are on by default for 429 / 503; turn off for non-idempotent functions. */
    val retry: Boolean = true,
)

/** Server functions: `fb.functions`. */
public class FunctionsApi internal constructor(private val t: Transport) {
    /** Every function, by name. */
    public suspend fun list(): List<JsonObject> = t.request("/api/v1/functions").items().map { it as JsonObject }

    public suspend fun get(name: String): JsonObject = t.request("/api/v1/functions/${seg(name)}") as JsonObject

    /** Recent executions, newest first. */
    public suspend fun logs(name: String, limit: Int? = null): List<JsonObject> =
        t.request("/api/v1/functions/${seg(name)}/logs", query = mapOf("limit" to limit)).items().map { it as JsonObject }

    /**
     * Invokes an HTTP function and returns what it sends back: parsed JSON for a JSON response, a JSON string for
     * text, [kotlinx.serialization.json.JsonNull] for 204 or an empty body. A function that answers with a status of
     * 400 or more raises the matching exception ([NotFoundException], [ValidationException], ...); use [invokeRaw]
     * when you need the exact answer.
     */
    public suspend fun invoke(name: String, input: Any? = null, o: InvokeOptions = InvokeOptions()): JsonElement {
        val res = invokeRaw(name, toBody(input), o)
        if (res.code == 204) {
            res.close()
            return kotlinx.serialization.json.JsonNull
        }
        val ct = res.header("content-type")
        val text = res.readText()
        if (text.isEmpty()) return kotlinx.serialization.json.JsonNull
        if (ct == null || !Regex("(^|[/+])json(\\s*;|$)", RegexOption.IGNORE_CASE).containsMatchIn(ct.trim())) {
            return kotlinx.serialization.json.JsonPrimitive(text)
        }
        return try {
            ForgeJson.parseToJsonElement(text)
        } catch (e: Exception) {
            throw InvalidResponseException("function response was not valid JSON", res.code)
        }
    }

    /** Typed invoke: `fb.functions.invokeAs<Reply>("hello", mapOf("name" to "x"))`. */
    public suspend inline fun <reified T> invokeAs(name: String, input: Any? = null, o: InvokeOptions = InvokeOptions()): T = invoke(name, input, o).decode()

    /** Same as [invoke] but returns the 2xx [Response] (headers, binary bodies); the caller closes it. */
    public suspend fun invokeRaw(name: String, input: InvokeBody?, o: InvokeOptions = InvokeOptions()): Response {
        val sub = (o.path ?: "").trimStart('/')
        val path = "/api/v1/functions/${seg(name)}/invoke" + if (sub.isEmpty()) "" else "/" + sub.split("/").joinToString("/") { seg(it) }
        val query = if (o.version == null) o.query else (o.query ?: emptyMap()) + ("version" to o.version)
        val (json, raw) = when (input) {
            null -> null to null
            is InvokeBody.Json -> input.value to null
            is InvokeBody.Raw -> null to input.bytes.toRequestBody(input.contentType?.toMediaTypeOrNull() ?: "application/octet-stream".toMediaTypeOrNull())
            is InvokeBody.Text -> null to input.value.toByteArray().toRequestBody("text/plain; charset=utf-8".toMediaTypeOrNull())
        }
        return t.send(path, o.method, query, json, raw as RequestBody?, o.headers, retry = o.retry)
    }

    private fun toBody(input: Any?): InvokeBody? = when (input) {
        null -> null
        is InvokeBody -> input
        is String -> InvokeBody.Text(input)
        is ByteArray -> InvokeBody.Raw(input)
        else -> InvokeBody.Json(toJsonElement(input))
    }
}
