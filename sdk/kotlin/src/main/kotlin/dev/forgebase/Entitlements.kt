package dev.forgebase

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/** The answer to "may this subject use this feature right now, and how much is left". */
@Serializable
public data class QuotaDecision(
    val allowed: Boolean,
    val feature: String = "",
    val limit: Long? = null,
    @SerialName("overage_units") val overageUnits: Long = 0,
    @SerialName("pack_remaining") val packRemaining: Long = 0,
    @SerialName("pack_units") val packUnits: Long = 0,
    val reason: String? = null,
    val remaining: Long? = null,
    val reserved: Long = 0,
    @SerialName("scope_key") val scopeKey: String = "",
    val subject: String = "",
    val units: Long = 0,
    val used: Long = 0,
    @SerialName("window_end") val windowEnd: String? = null,
    @SerialName("window_start") val windowStart: String = "",
)

@Serializable
public data class Consumption(
    val decision: QuotaDecision,
    @SerialName("event_id") val eventId: String = "",
    val replayed: Boolean = false,
)

@Serializable
public data class Reservation(
    val id: String,
    val feature: String = "",
    val status: String = "",
    val units: Long = 0,
    @SerialName("committed_units") val committedUnits: Long? = null,
    @SerialName("overage_units") val overageUnits: Long = 0,
    @SerialName("pack_units") val packUnits: Long = 0,
    @SerialName("scope_key") val scopeKey: String = "",
    val subject: String = "",
    val replayed: Boolean = false,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("expires_at") val expiresAt: String = "",
)

@Serializable
public data class Lease(
    val id: String,
    val feature: String = "",
    val released: Boolean = false,
    val replayed: Boolean = false,
    @SerialName("scope_key") val scopeKey: String = "",
    val subject: String = "",
    @SerialName("expires_at") val expiresAt: String = "",
)

/** The effective plan and grants of a subject. */
@Serializable
public data class EntitlementSet(
    val entitled: Boolean = false,
    val subject: String = "",
    val plan: String? = null,
    val status: String? = null,
    val member: String? = null,
    @SerialName("trial_ends_at") val trialEndsAt: String? = null,
    /** Feature key to its grant. */
    val features: Map<String, JsonElement> = emptyMap(),
)

@Serializable
public data class UsageSummary(
    val feature: String,
    val kind: String = "",
    val enabled: Boolean = false,
    val enforcement: String = "",
    val period: String = "",
    val limit: Long? = null,
    val used: Long = 0,
    val remaining: Long? = null,
    val reserved: Long = 0,
    val percent: Double? = null,
    @SerialName("scope_key") val scopeKey: String = "",
    @SerialName("window_start") val windowStart: String = "",
    @SerialName("window_end") val windowEnd: String? = null,
)

public data class HasResult(val has: Boolean, val reason: String? = null)

/** A quota question. */
public data class QuotaRequest(
    val feature: String,
    /** Units (default 1). */
    val units: Long? = null,
    /** Counter for per-user / team / resource quotas. */
    val scopeKey: String? = null,
    /** Billed subject (default: the caller's). */
    val subject: String? = null,
    val metadata: Map<String, Any?>? = null,
)

private fun QuotaRequest.wire(idempotencyKey: String? = null, ttlSecs: Long? = null): JsonObject = obj(
    "feature" to feature, "units" to units, "scope_key" to scopeKey, "subject" to subject, "metadata" to metadata,
    "idempotency_key" to idempotencyKey, "ttl_secs" to ttlSecs,
)

/** Entitlement and quota checks and metered usage: `fb.entitlements`. Catalog administration is left to the JS and Rust SDKs. */
public class EntitlementsApi internal constructor(private val t: Transport) {
    /** Does the caller (or [subject]) have this feature? */
    public suspend fun hasFeature(feature: String, subject: String? = null): Boolean = hasDetailed(mapOf("feature" to feature, "subject" to subject)).has

    public suspend fun hasPermission(permission: String): Boolean = hasDetailed(mapOf("permission" to permission)).has

    public suspend fun hasPlan(plan: String, subject: String? = null): Boolean = hasDetailed(mapOf("plan" to plan, "subject" to subject)).has

    private suspend fun hasDetailed(query: Map<String, Any?>): HasResult {
        val r = t.request("/api/v1/entitlements/has", query = query).objOrNull()
        return HasResult((r?.get("has") as? kotlinx.serialization.json.JsonPrimitive)?.content == "true", r?.get("reason").str())
    }

    /** Effective plan and grants of the caller (or of [subject], e.g. `org:<id>`). */
    public suspend fun get(subject: String? = null): EntitlementSet = t.request("/api/v1/entitlements", query = mapOf("subject" to subject)).decode()

    /** Usage of every metered feature. */
    public suspend fun usage(subject: String? = null, scopeKey: String? = null): List<UsageSummary> =
        t.request("/api/v1/usage", query = mapOf("subject" to subject, "scope_key" to scopeKey)).items().map { it.decode() }

    /** Would these units be granted right now? No side effects. */
    public suspend fun checkQuota(r: QuotaRequest): QuotaDecision = t.request("/api/v1/quotas/check", "POST", body = r.wire()).decode()

    /** Records usage at once. A replayed key returns the first result with `replayed = true`. Throws [QuotaExceededException] past a hard limit. */
    public suspend fun consume(r: QuotaRequest, idempotencyKey: String): Consumption =
        t.request("/api/v1/usage/consume", "POST", body = r.wire(idempotencyKey)).decode()

    /** Holds units until [commitReservation] or [releaseReservation] (or [ttlSecs]). */
    public suspend fun reserve(r: QuotaRequest, idempotencyKey: String, ttlSecs: Long? = null): Reservation =
        t.request("/api/v1/usage/reservations", "POST", body = r.wire(idempotencyKey, ttlSecs)).decode()

    public suspend fun commitReservation(id: String, units: Long? = null): Reservation =
        t.request("/api/v1/usage/reservations/${seg(id)}/commit", "POST", body = obj("units" to units)).decode()

    public suspend fun releaseReservation(id: String): Reservation =
        t.request("/api/v1/usage/reservations/${seg(id)}/release", "POST", body = JsonObject(emptyMap())).decode()

    /** Reserve, run [block], then commit; release the hold when [block] throws. */
    public suspend fun <R> withReservation(r: QuotaRequest, idempotencyKey: String, ttlSecs: Long? = null, block: suspend (Reservation) -> R): R {
        val res = reserve(r, idempotencyKey, ttlSecs)
        try {
            val out = block(res)
            commitReservation(res.id)
            return out
        } catch (e: Throwable) {
            runCatching { releaseReservation(res.id) }
            throw e
        }
    }

    /** Concurrency lease (e.g. "at most 5 running jobs"). */
    public suspend fun acquireLease(r: QuotaRequest, idempotencyKey: String, ttlSecs: Long? = null): Lease =
        t.request("/api/v1/usage/leases", "POST", body = r.wire(idempotencyKey, ttlSecs)).decode()

    public suspend fun renewLease(id: String, ttlSecs: Long? = null): Lease =
        t.request("/api/v1/usage/leases/${seg(id)}/renew", "POST", body = obj("ttl_secs" to ttlSecs)).decode()

    public suspend fun releaseLease(id: String) {
        t.request("/api/v1/usage/leases/${seg(id)}", "DELETE")
    }

    /** The subscription of [subject] (default: the caller's). No subscription is a [NotFoundException]. */
    public suspend fun subscription(subject: String? = null): JsonObject =
        t.request("/api/v1/entitlements/subscription", query = mapOf("subject" to subject)) as JsonObject

    /** Usage alerts (70 / 90 / 100 %) of [subject] (default: the caller's), newest first. */
    public suspend fun usageAlerts(subject: String? = null): List<JsonElement> = t.request("/api/v1/usage/alerts", query = mapOf("subject" to subject)).items()
}
