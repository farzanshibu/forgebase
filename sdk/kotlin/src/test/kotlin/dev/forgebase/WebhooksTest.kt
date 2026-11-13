package dev.forgebase

import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue

/** Same vectors and cases as sdk/js/test/webhooks.test.ts. */
class WebhooksTest {
    /** Independent reference: what the server's `sign` (crates/forgebase-queue/src/push.rs) computes. */
    private fun ref(secret: String, ts: Long, body: ByteArray): String {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(secret.toByteArray(), "HmacSHA256"))
        mac.update("$ts.".toByteArray())
        mac.update(body)
        return mac.doFinal().joinToString("") { "%02x".format(it) }
    }

    private val secret = "whsec_4f1c9d2a7b"
    private val t = 1_700_000_000L
    private val now = (t + 10) * 1000
    private val envelope = """{"subscription":"orders-push","delivery_attempt":2,"message":{"id":"msg_1","topic":"orders","seq":7,"data":{"order":42,"note":"héllo ✓"},"attributes":{"kind":"created"},"publisher":"usr_1","bytes":31,"published_at":"2026-01-01T00:00:00Z"}}"""
    private val body = envelope.toByteArray()
    private fun header(s: String = secret, ts: Long = t, b: ByteArray = body) = "t=$ts,v1=${ref(s, ts, b)}"
    private fun at(secs: Long) = (t + secs) * 1000

    @Test
    fun `matches a vector computed with openssl (key is the whole secret string, prefix included)`() {
        // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac 'whsec_test'
        val h = "t=1700000000,v1=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789"
        assertTrue(Webhooks.verify("whsec_test", h, """{"a":1}""", nowMillis = now))
        assertFalse(Webhooks.verify("test", h, """{"a":1}""", nowMillis = now))
        assertEquals(h, Webhooks.sign("whsec_test", """{"a":1}""", 1_700_000_000))
    }

    @Test
    fun `accepts a valid signature and signs the exact UTF-8 bytes`() {
        assertTrue(Webhooks.verify(secret, header(), body, nowMillis = now))
        assertTrue(Webhooks.verify(secret, header(), envelope, nowMillis = now))
    }

    @Test
    fun `rejects a tampered body`() {
        val h = header()
        for (b in listOf(envelope.replace("42", "43"), "$envelope ", "")) {
            assertFalse(Webhooks.verify(secret, h, b, nowMillis = now), b)
        }
    }

    @Test
    fun `rejects a tampered timestamp`() {
        val sig = ref(secret, t, body)
        assertFalse(Webhooks.verify(secret, "t=${t + 1},v1=$sig", body, nowMillis = now))
        assertFalse(Webhooks.verify(secret, "t=${t - 1},v1=$sig", body, nowMillis = now))
    }

    @Test
    fun `rejects the wrong secret and a flipped or resized signature`() {
        assertFalse(Webhooks.verify("whsec_other", header(), body, nowMillis = now))
        val sig = ref(secret, t, body)
        val flipped = (if (sig[0] == '0') "1" else "0") + sig.drop(1)
        assertFalse(Webhooks.verify(secret, "t=$t,v1=$flipped", body, nowMillis = now))
        assertFalse(Webhooks.verify(secret, "t=$t,v1=${sig.dropLast(2)}", body, nowMillis = now))
        assertFalse(Webhooks.verify(secret, "t=$t,v1=${sig}00", body, nowMillis = now))
    }

    @Test
    fun `accepts uppercase hex`() {
        assertTrue(Webhooks.verify(secret, "t=$t,v1=${ref(secret, t, body).uppercase()}", body, nowMillis = now))
    }

    @Test
    fun `tolerance window defaults to 300 seconds in both directions`() {
        val h = header()
        assertTrue(Webhooks.verify(secret, h, body, nowMillis = at(300)))
        assertFalse(Webhooks.verify(secret, h, body, nowMillis = at(301)))
        assertTrue(Webhooks.verify(secret, h, body, nowMillis = at(-300)))
        assertFalse(Webhooks.verify(secret, h, body, nowMillis = at(-301)))
    }

    @Test
    fun `honours toleranceSeconds`() {
        val h = header()
        assertTrue(Webhooks.verify(secret, h, body, 60, at(60)))
        assertFalse(Webhooks.verify(secret, h, body, 60, at(61)))
        assertTrue(Webhooks.verify(secret, h, body, 0, at(0)))
        assertFalse(Webhooks.verify(secret, h, body, 0, at(1)))
        assertTrue(Webhooks.verify(secret, h, body, Long.MAX_VALUE, at(10_000_000)))
        assertFailsWith<IllegalArgumentException> { Webhooks.verify(secret, h, body, -1, now) }
    }

    @Test
    fun `uses the clock by default`() {
        assertTrue(Webhooks.verify(secret, Webhooks.sign(secret, body), body))
        assertFalse(Webhooks.verify(secret, header(), body))
    }

    @Test
    fun `secret rotation - several v1 values and several secrets`() {
        val old = "whsec_old"
        val new = "whsec_new"
        val both = "t=$t,v1=${ref(new, t, body)},v1=${ref(old, t, body)}"
        assertTrue(Webhooks.verify(new, both, body, nowMillis = now))
        assertTrue(Webhooks.verify(old, both, body, nowMillis = now))
        assertFalse(Webhooks.verify("whsec_third", both, body, nowMillis = now))
        assertTrue(Webhooks.verify(listOf(old, new), header(new), body, nowMillis = now))
        assertTrue(Webhooks.verify(listOf(old, new), header(old), body, nowMillis = now))
        assertFalse(Webhooks.verify(listOf("whsec_a", "whsec_b"), both, body, nowMillis = now))
        assertFalse(Webhooks.verify(emptyList(), both, body, nowMillis = now))
        // still checks the timestamp when one v1 matches
        assertFalse(Webhooks.verify(new, both, body, nowMillis = at(400)))
    }

    @Test
    fun `skips undecodable v1 values and unknown schemes`() {
        val h = "t=$t,v1=nothex,v1=,v2=abcd,v1=${ref(secret, t, body)}"
        assertTrue(Webhooks.verify(secret, h, body, nowMillis = now))
    }

    @Test
    fun `rejects missing and malformed headers without throwing`() {
        val sig = ref(secret, t, body)
        val bad = listOf(
            null, "", "   ", "garbage", "v1=$sig", "t=$t", "t=$t,v2=$sig", "t=abc,v1=$sig", "t=-5,v1=$sig", "t=$t.5,v1=$sig",
            "t=$t,t=$t,v1=$sig", "t=99999999999999999999,v1=$sig",
        )
        for (h in bad) assertFalse(Webhooks.verify(secret, h, body, nowMillis = now), h.toString())
    }

    @Test
    fun `tolerates whitespace around the parts`() {
        assertTrue(Webhooks.verify(secret, " t=$t , v1=${ref(secret, t, body)} ", body, nowMillis = now))
    }

    @Test
    fun `sign rejects a negative timestamp`() {
        assertFailsWith<IllegalArgumentException> { Webhooks.sign(secret, body, -1) }
        assertEquals(header(), Webhooks.sign(secret, body, t))
    }

    @Test
    fun `constructEvent returns the typed envelope after verifying`() {
        val ev = Webhooks.constructEvent(secret, header(), body, nowMillis = now)
        assertEquals("orders-push", ev.subscription)
        assertEquals(2, ev.deliveryAttempt)
        assertEquals("msg_1", ev.message.id)
        assertEquals(7, ev.message.seq)
        assertEquals("héllo ✓", ev.message.data.decode<Map<String, kotlinx.serialization.json.JsonElement>>()["note"]?.let { (it as kotlinx.serialization.json.JsonPrimitive).content })
    }

    private fun reasonOf(h: String?, b: ByteArray = body, now: Long = this.now, s: String = secret): WebhookFailure? = try {
        Webhooks.constructEvent(s, h, b, nowMillis = now)
        null
    } catch (e: WebhookVerificationException) {
        e.reason
    }

    @Test
    fun `constructEvent throws WebhookVerificationException with the reason`() {
        assertEquals(WebhookFailure.MISSING_HEADER, reasonOf(null))
        assertEquals(WebhookFailure.MALFORMED_HEADER, reasonOf("nope"))
        assertEquals(WebhookFailure.TIMESTAMP_OUT_OF_TOLERANCE, reasonOf(header(), now = at(1000)))
        assertEquals(WebhookFailure.SIGNATURE_MISMATCH, reasonOf(header(), "${envelope}x".toByteArray()))
        assertEquals(WebhookFailure.SIGNATURE_MISMATCH, reasonOf(header(), s = "whsec_other"))
        val err = assertFailsWith<WebhookVerificationException> { Webhooks.constructEvent(secret, "x", body, nowMillis = now) }
        assertEquals("webhook_malformed_header", err.code)
        assertEquals(0, err.status)
        assertIs<ForgeBaseException>(err)
    }

    @Test
    fun `constructEvent rejects a correctly signed body that is not the envelope`() {
        for (b in listOf("not json", "[]", "null", """{"subscription":"s"}""", """{"subscription":"s","delivery_attempt":"1","message":{}}""")) {
            val bytes = b.toByteArray()
            assertEquals(WebhookFailure.INVALID_PAYLOAD, reasonOf(header(b = bytes), bytes), b)
        }
        val invalidUtf8 = byteArrayOf(0x7b, 0xff.toByte(), 0x7d)
        assertEquals(WebhookFailure.INVALID_PAYLOAD, reasonOf(header(b = invalidUtf8), invalidUtf8))
    }

    @Test
    fun `constructEvent never parses a body whose signature fails`() {
        assertEquals(WebhookFailure.SIGNATURE_MISMATCH, reasonOf(header(), "not json".toByteArray()))
    }
}
