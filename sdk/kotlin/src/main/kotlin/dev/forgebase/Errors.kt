package dev.forgebase

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import okhttp3.Headers

/** One per-field validation error from `details.fields`. */
public data class FieldError(public val code: String, public val message: String)

/** Base class of everything the SDK throws. */
public open class ForgeBaseException(
    message: String,
    /** HTTP status; `0` when no response was received. */
    public val status: Int = 0,
    /** Stable machine-readable code (`validation_failed`, `network_error`, ...). */
    public val code: String = "error",
    public val requestId: String? = null,
    /** The error's `details` object, when the server sent one. */
    public val details: JsonObject? = null,
    /** Seconds the server asked the client to wait (`Retry-After`). */
    public val retryAfter: Double? = null,
    /** Parsed response body, when there was one. */
    public val body: JsonElement? = null,
    cause: Throwable? = null,
) : RuntimeException(message, cause) {
    override fun toString(): String = "${this::class.simpleName}(status=$status, code=$code, message=$message)"
}

/** The server answered with an error status (anything but 2xx). */
public open class ApiException(
    message: String,
    status: Int,
    code: String,
    requestId: String? = null,
    details: JsonObject? = null,
    retryAfter: Double? = null,
    body: JsonElement? = null,
) : ForgeBaseException(message, status, code, requestId, details, retryAfter, body)

/** 400 / 422 `validation_failed` and other malformed requests. */
public class ValidationException(
    message: String,
    status: Int,
    code: String,
    requestId: String? = null,
    details: JsonObject? = null,
    retryAfter: Double? = null,
    body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body) {
    /** Per-field errors from `details.fields`. */
    public val fields: Map<String, FieldError>
        get() {
            val f = details?.get("fields") as? JsonObject ?: return emptyMap()
            val out = LinkedHashMap<String, FieldError>()
            for ((k, v) in f) {
                if (v is JsonObject) {
                    out[k] = FieldError(
                        (v["code"] as? JsonPrimitive)?.contentOrNull ?: "invalid",
                        (v["message"] as? JsonPrimitive)?.contentOrNull ?: "",
                    )
                }
            }
            return out
        }
}

/** 401: missing, expired or invalid credentials. */
public class AuthenticationException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 403: the credentials are fine but the action is not allowed. */
public class PermissionException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 404. */
public class NotFoundException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 409 (unique violation, version mismatch, ...) and 412. */
public class ConflictException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 429 from rate limiting. Already retried `maxRetries` times when this is thrown. */
public class RateLimitException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 429 because a quota is exhausted (`details` carries the decision). Never retried: waiting will not help. */
public class QuotaExceededException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** 5xx. 503 is retried before this is thrown. */
public class ServerException(
    message: String, status: Int, code: String, requestId: String? = null, details: JsonObject? = null,
    retryAfter: Double? = null, body: JsonElement? = null,
) : ApiException(message, status, code, requestId, details, retryAfter, body)

/** No HTTP response (DNS, refused connection, timeout). */
public class NetworkException(message: String, cause: Throwable? = null) :
    ForgeBaseException(message, 0, "network_error", cause = cause)

/** The response was 2xx but not what the SDK expected. */
public class InvalidResponseException(message: String, status: Int = 200) :
    ForgeBaseException(message, status, "invalid_response")

/** WebSocket failures and refusals. */
public class RealtimeException(code: String, message: String) : ForgeBaseException(message, 0, code)

/** Why a webhook failed verification. */
public enum class WebhookFailure(public val wire: String) {
    MISSING_HEADER("missing_header"),
    MALFORMED_HEADER("malformed_header"),
    TIMESTAMP_OUT_OF_TOLERANCE("timestamp_out_of_tolerance"),
    SIGNATURE_MISMATCH("signature_mismatch"),
    INVALID_PAYLOAD("invalid_payload"),
}

/** A push request failed verification: bad signature, stale timestamp or unreadable payload. */
public class WebhookVerificationException(public val reason: WebhookFailure, message: String) :
    ForgeBaseException(message, 0, "webhook_${reason.wire}")

private val QUOTA_CODES = Regex("quota|limit_exceeded|usage_limit", RegexOption.IGNORE_CASE)

/** Builds the right exception from a failed response. */
internal fun errorFromResponse(status: Int, body: JsonElement?, headers: Headers): ApiException {
    val env = (body as? JsonObject)?.get("error") as? JsonObject
    val code = (env?.get("code") as? JsonPrimitive)?.takeIf { it.isString }?.content ?: "http_error"
    val message = (env?.get("message") as? JsonPrimitive)?.takeIf { it.isString }?.content ?: "HTTP $status"
    val requestId = (env?.get("request_id") as? JsonPrimitive)?.takeIf { it.isString }?.content ?: headers["x-request-id"]
    val details = env?.get("details") as? JsonObject
    val retryAfter = parseRetryAfter(headers)?.let { it / 1000.0 }
    val b = body.takeUnless { it is JsonNull }
    return when {
        status == 429 && QUOTA_CODES.containsMatchIn(code) -> QuotaExceededException(message, status, code, requestId, details, retryAfter, b)
        status == 429 -> RateLimitException(message, status, code, requestId, details, retryAfter, b)
        status == 401 -> AuthenticationException(message, status, code, requestId, details, retryAfter, b)
        status == 403 -> PermissionException(message, status, code, requestId, details, retryAfter, b)
        status == 404 -> NotFoundException(message, status, code, requestId, details, retryAfter, b)
        status == 409 || status == 412 -> ConflictException(message, status, code, requestId, details, retryAfter, b)
        status == 400 || status == 422 -> ValidationException(message, status, code, requestId, details, retryAfter, b)
        status >= 500 -> ServerException(message, status, code, requestId, details, retryAfter, b)
        else -> ApiException(message, status, code, requestId, details, retryAfter, b)
    }
}

/** `Retry-After` as milliseconds (delta-seconds or HTTP-date); `null` when absent or invalid. */
internal fun parseRetryAfter(headers: Headers, nowMs: Long = System.currentTimeMillis()): Long? {
    val raw = headers["retry-after"]?.trim() ?: return null
    raw.toDoubleOrNull()?.let { return if (it < 0) null else Math.round(it * 1000) }
    val at = headers.getDate("retry-after") ?: return null
    return maxOf(0L, at.time - nowMs)
}
