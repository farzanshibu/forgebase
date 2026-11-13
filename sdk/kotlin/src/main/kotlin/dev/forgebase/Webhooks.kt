package dev.forgebase

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.nio.ByteBuffer
import java.security.MessageDigest
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/*
 * Webhook verification for push subscriptions (ADR-0022 §13), for JVM servers.
 *
 * The server POSTs `{subscription, delivery_attempt, message}` as JSON with
 *
 *   X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>
 *
 * The key is the UTF-8 text of the subscription's secret (`whsec_...`, prefix included). While a secret is being
 * rotated the header may carry several `v1=` values, one per secret that is still valid. Pass the body bytes exactly
 * as received, never a parsed and re-serialised object.
 */
public object Webhooks {
    /** Header the server signs push requests with (HTTP header names are case-insensitive). */
    public const val SIGNATURE_HEADER: String = "X-Forgebase-Signature"

    /** Default accepted clock distance between the signed timestamp and now, in seconds. */
    public const val DEFAULT_TOLERANCE_SECONDS: Long = 300

    /** The push envelope: the body of the POST. */
    public data class Event(
        /** Subscription name (without the tenant prefix). */
        val subscription: String,
        /** 1 on the first delivery, then counts up with every retry. */
        val deliveryAttempt: Int,
        val message: QueueMessage,
    )

    /** `true` when the signature is valid. */
    public fun verify(
        secret: String,
        header: String?,
        body: ByteArray,
        toleranceSeconds: Long = DEFAULT_TOLERANCE_SECONDS,
        nowMillis: Long = System.currentTimeMillis(),
    ): Boolean = verify(listOf(secret), header, body, toleranceSeconds, nowMillis)

    /** Accepts any of several [secrets], e.g. the new and the old one while you roll the secret over. */
    public fun verify(
        secrets: List<String>,
        header: String?,
        body: ByteArray,
        toleranceSeconds: Long = DEFAULT_TOLERANCE_SECONDS,
        nowMillis: Long = System.currentTimeMillis(),
    ): Boolean = check(secrets, header, body, toleranceSeconds, nowMillis) == null

    public fun verify(secret: String, header: String?, body: String, toleranceSeconds: Long = DEFAULT_TOLERANCE_SECONDS, nowMillis: Long = System.currentTimeMillis()): Boolean =
        verify(secret, header, body.toByteArray(Charsets.UTF_8), toleranceSeconds, nowMillis)

    /**
     * Verifies the request, then parses the body into the push envelope. Throws [WebhookVerificationException]
     * ([WebhookVerificationException.reason] says why); answer such a request with a 4xx so the server does not count
     * it as delivered.
     */
    public fun constructEvent(
        secrets: List<String>,
        header: String?,
        body: ByteArray,
        toleranceSeconds: Long = DEFAULT_TOLERANCE_SECONDS,
        nowMillis: Long = System.currentTimeMillis(),
    ): Event {
        check(secrets, header, body, toleranceSeconds, nowMillis)?.let { throw it }
        val parsed = try {
            val strict = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
            ForgeJson.parseToJsonElement(strict.decode(ByteBuffer.wrap(body)).toString())
        } catch (e: CharacterCodingException) {
            throw WebhookVerificationException(WebhookFailure.INVALID_PAYLOAD, "the webhook body is not valid JSON")
        } catch (e: Exception) {
            throw WebhookVerificationException(WebhookFailure.INVALID_PAYLOAD, "the webhook body is not valid JSON")
        }
        val o = parsed as? JsonObject
        val sub = (o?.get("subscription") as? JsonPrimitive)?.takeIf { it.isString }?.content
        val attempt = (o?.get("delivery_attempt") as? JsonPrimitive)?.takeIf { !it.isString }?.content?.toIntOrNull()
        val msg = o?.get("message") as? JsonObject
        if (sub == null || attempt == null || msg == null) {
            throw WebhookVerificationException(WebhookFailure.INVALID_PAYLOAD, "the webhook body is not a {subscription, delivery_attempt, message} envelope")
        }
        return Event(sub, attempt, msg.decode())
    }

    public fun constructEvent(secret: String, header: String?, body: ByteArray, toleranceSeconds: Long = DEFAULT_TOLERANCE_SECONDS, nowMillis: Long = System.currentTimeMillis()): Event =
        constructEvent(listOf(secret), header, body, toleranceSeconds, nowMillis)

    /**
     * Builds the header value the server would send: `t=<timestamp>,v1=<hex>`. For tests of your own handler, or to
     * drive it from a script.
     */
    public fun sign(secret: String, body: ByteArray, timestampSeconds: Long = System.currentTimeMillis() / 1000): String {
        require(timestampSeconds >= 0) { "timestamp must be a non-negative integer (Unix seconds)" }
        return "t=$timestampSeconds,v1=${mac(secret, timestampSeconds, body).toHex()}"
    }

    public fun sign(secret: String, body: String, timestampSeconds: Long = System.currentTimeMillis() / 1000): String =
        sign(secret, body.toByteArray(Charsets.UTF_8), timestampSeconds)

    private class Parsed(val timestamp: Long, val signatures: List<ByteArray>)

    private fun check(secrets: List<String>, header: String?, body: ByteArray, toleranceSeconds: Long, nowMillis: Long): WebhookVerificationException? {
        if (header == null || header.isBlank()) {
            return WebhookVerificationException(WebhookFailure.MISSING_HEADER, "the $SIGNATURE_HEADER header is missing")
        }
        val parsed = parseHeader(header)
            ?: return WebhookVerificationException(WebhookFailure.MALFORMED_HEADER, "the $SIGNATURE_HEADER header is malformed")
        require(toleranceSeconds >= 0) { "toleranceSeconds must be zero or more" }
        val nowSecs = Math.floorDiv(nowMillis, 1000L)
        if (Math.abs(nowSecs - parsed.timestamp) > toleranceSeconds) {
            return WebhookVerificationException(WebhookFailure.TIMESTAMP_OUT_OF_TOLERANCE, "the webhook timestamp is outside the tolerance window")
        }
        var ok = false
        // Every secret and every candidate is compared, whatever matched first, so timing says nothing about which one did.
        for (secret in secrets) {
            val expected = mac(secret, parsed.timestamp, body)
            for (candidate in parsed.signatures) ok = MessageDigest.isEqual(expected, candidate) or ok
        }
        return if (ok) null else WebhookVerificationException(WebhookFailure.SIGNATURE_MISMATCH, "no signature matches the payload and secret")
    }

    /** `t=<secs>,v1=<hex>[,v1=<hex>...]`; unknown keys (future schemes) are ignored; undecodable `v1` values never match. */
    private fun parseHeader(header: String): Parsed? {
        var timestamp: Long? = null
        val signatures = ArrayList<ByteArray>()
        var sawV1 = false
        for (part in header.split(',')) {
            val eq = part.indexOf('=')
            if (eq < 0) continue
            val key = part.substring(0, eq).trim()
            val value = part.substring(eq + 1).trim()
            if (key == "t") {
                if (timestamp != null || !Regex("^\\d{1,15}$").matches(value)) return null
                timestamp = value.toLong()
            } else if (key == "v1") {
                sawV1 = true
                fromHex(value)?.let { signatures.add(it) }
            }
        }
        if (timestamp == null || !sawV1) return null
        return Parsed(timestamp, signatures)
    }

    /** HMAC-SHA256(secret, "<ts>.<body>"). */
    private fun mac(secret: String, ts: Long, body: ByteArray): ByteArray {
        val m = Mac.getInstance("HmacSHA256")
        // An empty key is rejected by SecretKeySpec; the server never signs with one.
        m.init(SecretKeySpec(secret.toByteArray(Charsets.UTF_8).let { if (it.isEmpty()) ByteArray(1) else it }, "HmacSHA256"))
        m.update("$ts.".toByteArray(Charsets.UTF_8))
        m.update(body)
        return m.doFinal()
    }

    private fun fromHex(hex: String): ByteArray? {
        if (hex.isEmpty() || hex.length % 2 != 0 || !hex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }) return null
        return ByteArray(hex.length / 2) { i -> hex.substring(2 * i, 2 * i + 2).toInt(16).toByte() }
    }
}
