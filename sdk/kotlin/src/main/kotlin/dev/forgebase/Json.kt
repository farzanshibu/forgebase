package dev.forgebase

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull

/** The JSON configuration every SDK call uses: lenient about unknown keys, omits null / default fields. */
public val ForgeJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = false
    isLenient = true
}

/** A record as the API returns it: system fields plus the collection's own. */
public typealias Row = JsonObject

/** Decodes this element into `T` (a `@Serializable` class). */
public inline fun <reified T> JsonElement.decode(): T = ForgeJson.decodeFromJsonElement<T>(this)

/** Converts plain Kotlin values (maps, lists, strings, numbers, booleans, `null`, [JsonElement]) to JSON. */
public fun toJsonElement(v: Any?): JsonElement = when (v) {
    null -> JsonNull
    is JsonElement -> v
    is String -> JsonPrimitive(v)
    is Boolean -> JsonPrimitive(v)
    is Number -> JsonPrimitive(v)
    is Enum<*> -> JsonPrimitive(v.name.lowercase())
    is Map<*, *> -> JsonObject(v.entries.associate { (k, x) -> k.toString() to toJsonElement(x) })
    is Iterable<*> -> JsonArray(v.map { toJsonElement(it) })
    is Array<*> -> JsonArray(v.map { toJsonElement(it) })
    is DoubleArray -> JsonArray(v.map { JsonPrimitive(it) })
    is FloatArray -> JsonArray(v.map { JsonPrimitive(it) })
    is IntArray -> JsonArray(v.map { JsonPrimitive(it) })
    is LongArray -> JsonArray(v.map { JsonPrimitive(it) })
    else -> throw IllegalArgumentException("cannot convert ${v::class.qualifiedName} to JSON; pass a JsonElement or a @Serializable value encoded with ForgeJson")
}

/** `jsonObjectOf("a" to 1, "b" to listOf("x"))`: like [toJsonElement], but pairs whose value is `null` are left out. */
public fun jsonObjectOf(vararg pairs: Pair<String, Any?>): JsonObject = obj(*pairs)

internal fun obj(vararg pairs: Pair<String, Any?>): JsonObject {
    val m = LinkedHashMap<String, JsonElement>()
    for ((k, v) in pairs) if (v != null) m[k] = toJsonElement(v)
    return JsonObject(m)
}

/** Copy of this object without `null` members. */
internal fun JsonObject.compact(): JsonObject = JsonObject(filterValues { it !is JsonNull })

internal fun JsonElement?.str(): String? = (this as? JsonPrimitive)?.takeIf { it !is JsonNull }?.contentOrNull
internal fun JsonElement?.int(): Int? = (this as? JsonPrimitive)?.intOrNull
internal fun JsonElement?.long(): Long? = (this as? JsonPrimitive)?.longOrNull
internal fun JsonElement?.objOrNull(): JsonObject? = this as? JsonObject

internal fun JsonElement?.items(key: String = "items"): List<JsonElement> =
    ((this as? JsonObject)?.get(key) as? JsonArray) ?: emptyList()

/** Percent-encodes one path segment. */
internal fun seg(s: String): String = java.net.URLEncoder.encode(s, "UTF-8").replace("+", "%20")
