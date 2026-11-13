package dev.forgebase

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class ConsistencyTest {
    @Test
    fun `keeps the highest position per set and sorts the header`() {
        val c = CommitTokens()
        assertNull(c.header())
        c.note("control/16/B374D848, s1/0/9")
        c.note("control/16/B374D000")
        c.note("control/17/1")
        assertEquals("control/17/1, s1/0/9", c.header())
        assertEquals("17/1", c.get("control"))
        c.note("s1/0/A")
        assertEquals("0/A", c.get("s1"))
    }

    @Test
    fun `ignores malformed tokens`() {
        val c = CommitTokens()
        c.note("nonsense, /1/2, a/xyz/1, b/1, c/1/2/3, d/123456789/1")
        assertNull(c.header())
        c.note(null)
        c.note("")
        assertNull(c.header())
    }

    @Test
    fun `clear forgets everything`() {
        val c = CommitTokens()
        c.note("a/1/2")
        c.clear()
        assertNull(c.header())
    }
}
