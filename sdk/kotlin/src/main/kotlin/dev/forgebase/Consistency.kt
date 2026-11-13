package dev.forgebase

/*
 * Read-your-writes with read replicas (ADR-0029 §2.6).
 *
 * The server answers a write with `X-ForgeBase-Commit: <set>/<lsn>[, ...]`. The SDK keeps the highest position per
 * set and sends them back as `X-ForgeBase-Read-After`, so a replica serves a read only once it has replayed the
 * writes. [Consistency.STRONG] sends `X-ForgeBase-Consistency: strong` and the server reads the primary.
 */

/** `SESSION` (default): read your own writes. `STRONG`: every read on the primary. */
public enum class Consistency { SESSION, STRONG }

internal const val COMMIT_HEADER: String = "X-ForgeBase-Commit"
internal const val READ_AFTER_HEADER: String = "X-ForgeBase-Read-After"
internal const val CONSISTENCY_HEADER: String = "X-ForgeBase-Consistency"

private const val MAX_SETS = 64
private val SET_ID = Regex("^[A-Za-z0-9_.-]{1,64}$")
private val HEX = Regex("^[0-9A-Fa-f]{1,8}$")

private fun parseToken(token: String): Pair<String, ULong>? {
    val t = token.trim()
    val slash = t.indexOf('/')
    if (slash <= 0) return null
    val set = t.substring(0, slash)
    val parts = t.substring(slash + 1).split('/')
    if (!SET_ID.matches(set) || parts.size != 2 || !HEX.matches(parts[0]) || !HEX.matches(parts[1])) return null
    return set to ((parts[0].toULong(16) shl 32) or parts[1].toULong(16))
}

private fun formatLsn(lsn: ULong): String = "${(lsn shr 32).toString(16).uppercase()}/${(lsn and 0xffffffffuL).toString(16).uppercase()}"

/** The highest commit position per set this client has seen. Thread-safe. */
public class CommitTokens {
    private val bySet = HashMap<String, ULong>()

    /** Takes the tokens of a header value (`control/16/B374D848, s1/0/9`); malformed ones are ignored. */
    public fun note(header: String?) {
        if (header.isNullOrEmpty()) return
        synchronized(bySet) {
            for (part in header.split(',')) {
                val (set, lsn) = parseToken(part) ?: continue
                val held = bySet[set]
                if (held == null && bySet.size >= MAX_SETS) continue
                if (held == null || lsn > held) bySet[set] = lsn
            }
        }
    }

    /** The position held for one set (`16/B374D848`). */
    public fun get(set: String): String? = synchronized(bySet) { bySet[set]?.let(::formatLsn) }

    /** The `X-ForgeBase-Read-After` value, or `null` when nothing was written yet. */
    public fun header(): String? = synchronized(bySet) {
        if (bySet.isEmpty()) null
        else bySet.entries.sortedBy { it.key }.joinToString(", ") { "${it.key}/${formatLsn(it.value)}" }
    }

    /** Forgets every token (after signing out, for example). */
    public fun clear(): Unit = synchronized(bySet) { bySet.clear() }
}
