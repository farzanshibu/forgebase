package dev.forgebase

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody

/** Options of [RecordsApi.list] and the paging helpers. */
public data class ListOptions(
    /** Filter-language expression, e.g. `price > {:min} && status = "active"`. */
    val filter: String? = null,
    /** Typed values for `{:name}` placeholders in [filter] (sent with the JSON query endpoint). */
    val params: Map<String, Any?>? = null,
    /** `listOf("-created", "name")`. */
    val sort: List<String>? = null,
    /** Relations to expand inline. */
    val expand: List<String>? = null,
    /** Fields to return. */
    val fields: List<String>? = null,
    /** Full-text search term. */
    val search: String? = null,
    /** 1-based page (offset pagination). */
    val page: Int? = null,
    /** Page size (server maximum 500). */
    val perPage: Int? = null,
    /** Cursor from a previous page's [ListResult.nextCursor]. */
    val cursor: String? = null,
    /** Skip the total count (cheaper on large collections). */
    val skipTotal: Boolean? = null,
    /** [Consistency.STRONG] reads from the primary even when replicas serve reads. */
    val consistency: Consistency? = null,
)

/** One page of records. */
public data class ListResult<T>(
    val items: List<T>,
    val page: Int? = null,
    val perPage: Int,
    val totalItems: Long? = null,
    val totalPages: Int? = null,
    /** Pass as `cursor` for the next page; `null` on the last page. */
    val nextCursor: String? = null,
) {
    /** Maps the items (e.g. to your own types). */
    public fun <R> map(f: (T) -> R): ListResult<R> = ListResult(items.map(f), page, perPage, totalItems, totalPages, nextCursor)
}

/** Decodes every item into `T`. */
public inline fun <reified T> ListResult<Row>.decodeItems(): ListResult<T> = map { it.decode<T>() }

public enum class TransferFormat { JSON, NDJSON, CSV }

/** Records of one collection: `fb.collection("posts")`. */
public class RecordsApi internal constructor(private val t: Transport, public val name: String) {
    private val base = "/api/v1/collections/${seg(name)}"

    /** One page. With [ListOptions.params] the typed JSON query endpoint is used, otherwise a plain GET. */
    public suspend fun list(o: ListOptions = ListOptions()): ListResult<Row> {
        val sort = o.sort?.joinToString(",")
        val r = if (o.params != null) {
            t.request(
                "$base/records/query", "POST", consistency = o.consistency,
                body = obj(
                    "filter" to o.filter, "sort" to sort, "search" to o.search, "page" to o.page, "per_page" to o.perPage,
                    "cursor" to o.cursor, "skip_total" to o.skipTotal, "params" to o.params, "expand" to o.expand, "fields" to o.fields,
                ),
            )
        } else {
            t.request(
                "$base/records", consistency = o.consistency,
                query = mapOf(
                    "filter" to o.filter, "sort" to sort, "search" to o.search, "page" to o.page, "per_page" to o.perPage,
                    "cursor" to o.cursor, "skip_total" to o.skipTotal, "expand" to o.expand, "fields" to o.fields,
                ),
            )
        }
        return fromWire(r)
    }

    /** Typed page: `posts.listAs<Post>()` decodes each record with kotlinx.serialization. */
    public suspend inline fun <reified T> listAs(o: ListOptions = ListOptions()): ListResult<T> = list(o).decodeItems()

    /** Every page, lazily. Follows `nextCursor`, else page numbers. */
    public fun pages(o: ListOptions = ListOptions()): Flow<ListResult<Row>> = flow {
        var cursor = o.cursor
        var page = o.page ?: 1
        while (true) {
            val res = list(o.copy(cursor = cursor, page = if (cursor != null) null else page))
            emit(res)
            val next = res.nextCursor
            when {
                next != null -> cursor = next
                cursor == null && res.page != null && res.totalPages != null && res.page < res.totalPages -> page = res.page + 1
                else -> return@flow
            }
        }
    }

    /** Every record, lazily: `posts.iterate(ListOptions(filter = "published = true")).collect { ... }`. */
    public fun iterate(o: ListOptions = ListOptions()): Flow<Row> = flow { pages(o).collect { p -> p.items.forEach { emit(it) } } }

    /** Collects every record (mind the memory use on big collections). */
    public suspend fun all(o: ListOptions = ListOptions()): List<Row> {
        val out = ArrayList<Row>()
        iterate(o).collect { out.add(it) }
        return out
    }

    /** First record matching [filter], or `null`. */
    public suspend fun first(filter: String, o: ListOptions = ListOptions()): Row? =
        list(o.copy(filter = filter, perPage = 1, page = null, cursor = null, skipTotal = true)).items.firstOrNull()

    public suspend fun get(id: String, expand: List<String>? = null, fields: List<String>? = null, consistency: Consistency? = null): Row =
        t.request("$base/records/${seg(id)}", consistency = consistency, query = mapOf("expand" to expand, "fields" to fields)) as JsonObject

    /** Typed get: `posts.getAs<Post>(id)`. */
    public suspend inline fun <reified T> getAs(id: String): T = get(id).decode()

    public suspend fun create(data: Map<String, Any?>): Row = t.request("$base/records", "POST", body = toJsonElement(data)) as JsonObject

    /** Creates a record from a `@Serializable` value. */
    public suspend inline fun <reified T> createFrom(value: T): Row = create(ForgeJson.encodeToJsonElement(kotlinx.serialization.serializer<T>(), value) as JsonObject)

    /** Partial update (PATCH). With [version] (the record's `_version`) the write fails with [ConflictException] when it is stale. */
    public suspend fun update(id: String, patch: Map<String, Any?>, version: Long? = null): Row =
        t.request("$base/records/${seg(id)}", "PATCH", body = toJsonElement(patch), headers = ifMatch(version)) as JsonObject

    /** Full replace (PUT). */
    public suspend fun replace(id: String, data: Map<String, Any?>, version: Long? = null): Row =
        t.request("$base/records/${seg(id)}", "PUT", body = toJsonElement(data), headers = ifMatch(version)) as JsonObject

    public suspend fun delete(id: String) {
        t.request("$base/records/${seg(id)}", "DELETE")
    }

    public suspend fun count(filter: String? = null): Long = t.request("$base/records/count", query = mapOf("filter" to filter)).objOrNull()?.get("count").long() ?: 0

    /** Downloads the collection as text. */
    public suspend fun export(format: TransferFormat = TransferFormat.JSON, filter: String? = null): String =
        t.requestText("$base/export", query = mapOf("format" to format, "filter" to filter))

    /** Uploads records as text in the given format. */
    public suspend fun import(format: TransferFormat, body: String): JsonObject =
        t.request(
            "$base/import", "POST", query = mapOf("format" to format),
            rawBody = body.toByteArray().toRequestBody("application/octet-stream".toMediaType()),
        ) as JsonObject

    private fun ifMatch(v: Long?): Map<String, String> = if (v == null) emptyMap() else mapOf("If-Match" to "\"$v\"")
}

private fun fromWire(w: JsonElement): ListResult<Row> {
    val o = w as? JsonObject ?: throw InvalidResponseException("list response is not an object")
    val items = ((o["items"] as? JsonArray) ?: JsonArray(emptyList())).map { it as JsonObject }
    return ListResult(
        items = items,
        page = o["page"].int(),
        perPage = o["per_page"].int() ?: items.size,
        totalItems = o["total_items"].long(),
        totalPages = o["total_pages"].int(),
        nextCursor = o["next_cursor"].str(),
    )
}

/** Collection definitions (`fb.collections`). */
public class CollectionsApi internal constructor(private val t: Transport) {
    public suspend fun list(): List<JsonObject> = t.request("/api/v1/collections").items().map { it as JsonObject }
    public suspend fun get(name: String): JsonObject = t.request("/api/v1/collections/${seg(name)}") as JsonObject
    public suspend fun create(def: Map<String, Any?>): JsonObject = t.request("/api/v1/collections", "POST", body = toJsonElement(def)) as JsonObject
    public suspend fun update(name: String, def: Map<String, Any?>): JsonObject =
        t.request("/api/v1/collections/${seg(name)}", "PUT", body = toJsonElement(def)) as JsonObject

    public suspend fun delete(name: String) {
        t.request("/api/v1/collections/${seg(name)}", "DELETE")
    }
}
