package dev.forgebase

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject

/*
 * Read helpers for vector search, the property graph, SQL and credits. Schema changes and administration (creating
 * vector collections and fields, rebuilding indexes, SQL batches that write, crediting accounts) are left to the
 * JS and Rust SDKs; [ForgeBase.request] reaches any route.
 */

/** Options shared by [VectorCollectionApi.search] and [VectorCollectionApi.hybrid]. */
public data class VectorQuery(
    /** Vector field; may be left out when the collection has exactly one. */
    val field: String? = null,
    /** Number of results, 1 to 1000 (default 10). */
    val k: Int? = null,
    /** Metadata filter, e.g. `category = 'news' && year >= {:min}`. */
    val filter: String? = null,
    /** Values of the `{:name}` placeholders of [filter]. */
    val params: Map<String, Any?>? = null,
    /** `auto` (default), `pre` or `post`. */
    val filterMode: String? = null,
    val includeVectors: Boolean? = null,
    /** Return each hit's metadata (server default `true`). */
    val includeMetadata: Boolean? = null,
    val includeText: Boolean? = null,
)

/** Vector search (`/api/v1/vector`): `fb.vector`. */
public class VectorApi internal constructor(private val t: Transport) {
    /** One collection: `fb.vector.collection("docs")`. */
    public fun collection(name: String): VectorCollectionApi = VectorCollectionApi(t, name)

    /** Collections of the caller's tenant. */
    public suspend fun list(): List<JsonObject> = t.request("/api/v1/vector/collections").items().map { it as JsonObject }
}

public class VectorCollectionApi internal constructor(private val t: Transport, public val name: String) {
    private val base = "/api/v1/vector/collections/${seg(name)}"

    public suspend fun get(): JsonObject = t.request(base) as JsonObject

    /** Inserts or replaces items: `{id?, vectors: {field: [..]}, metadata?, text?}`. */
    public suspend fun upsert(items: List<Map<String, Any?>>): JsonObject = t.request("$base/upsert", "POST", body = obj("items" to items)) as JsonObject

    public suspend fun getItem(id: String): JsonObject = t.request("$base/items/${seg(id)}") as JsonObject

    public suspend fun deleteItem(id: String) {
        t.request("$base/items/${seg(id)}", "DELETE")
    }

    /** k-nearest-neighbour search with an optional metadata filter; closest first. [exact] forces brute force. */
    public suspend fun search(vector: List<Float>, q: VectorQuery = VectorQuery(), exact: Boolean? = null): JsonObject = t.request(
        "$base/search", "POST",
        body = obj(
            "field" to q.field, "vector" to vector, "k" to q.k, "filter" to q.filter, "params" to q.params, "filter_mode" to q.filterMode,
            "exact" to exact, "include_vectors" to q.includeVectors, "include_metadata" to q.includeMetadata, "include_text" to q.includeText,
        ),
    ) as JsonObject

    /** Hybrid search: keyword (BM25) and vector rankings fused with Reciprocal Rank Fusion; best first. */
    public suspend fun hybrid(vector: List<Float>? = null, text: String? = null, q: VectorQuery = VectorQuery(), textOperator: String? = null, candidates: Int? = null, rrfK: Int? = null): JsonObject =
        t.request(
            "$base/hybrid", "POST",
            body = obj(
                "field" to q.field, "vector" to vector, "text" to text, "text_operator" to textOperator, "k" to q.k, "candidates" to candidates,
                "rrf_k" to rrfK, "filter" to q.filter, "params" to q.params, "filter_mode" to q.filterMode,
                "include_vectors" to q.includeVectors, "include_metadata" to q.includeMetadata, "include_text" to q.includeText,
            ),
        ) as JsonObject

    public suspend fun stats(): JsonObject = t.request("$base/stats") as JsonObject
}

/** One page of nodes, edges or neighbours. */
public data class GraphPage(val items: List<JsonElement>, /** Pass as `after` to continue; `null` on the last page. */ val nextAfter: String?)

/** Property graph and openCypher (`/api/v1/graph`): `fb.graph`. Reads and traversals; node and edge writes are included. */
public class GraphApi internal constructor(private val t: Transport) {
    private val g = "/api/v1/graph"

    public suspend fun createNode(label: String? = null, properties: Map<String, Any?>? = null, id: String? = null): JsonObject =
        t.request("$g/nodes", "POST", body = obj("id" to id, "label" to label, "properties" to properties)) as JsonObject

    public suspend fun getNode(id: String): JsonObject = t.request("$g/nodes/${seg(id)}") as JsonObject

    public suspend fun nodes(label: String? = null, after: String? = null, limit: Int? = null): GraphPage =
        page(t.request("$g/nodes", query = mapOf("label" to label, "after" to after, "limit" to limit)))

    public suspend fun deleteNode(id: String) {
        t.request("$g/nodes/${seg(id)}", "DELETE")
    }

    public suspend fun createEdge(type: String, source: String, target: String, properties: Map<String, Any?>? = null, directed: Boolean? = null): JsonObject =
        t.request("$g/edges", "POST", body = obj("type" to type, "source" to source, "target" to target, "properties" to properties, "directed" to directed)) as JsonObject

    public suspend fun deleteEdge(id: String) {
        t.request("$g/edges/${seg(id)}", "DELETE")
    }

    /** Neighbours of a node (`direction`: `out`, `in`, `both`). */
    public suspend fun neighbors(id: String, direction: String? = null, edgeType: String? = null, offset: Int? = null, limit: Int? = null): GraphPage =
        page(t.request("$g/nodes/${seg(id)}/neighbors", query = mapOf("direction" to direction, "edge_type" to edgeType, "offset" to offset, "limit" to limit)))

    /** Multi-hop traversal from [start] within [maxDepth] hops (at most 10). */
    public suspend fun traverse(
        start: String, direction: String? = null, edgeTypes: List<String>? = null, maxDepth: Int? = null, limit: Int? = null,
        filter: Map<String, Any?>? = null, includeStart: Boolean? = null, includeEdges: Boolean? = null,
    ): JsonObject = t.request(
        "$g/traverse", "POST",
        body = obj(
            "start" to start, "direction" to direction, "edge_types" to edgeTypes, "max_depth" to maxDepth, "limit" to limit,
            "filter" to filter, "include_start" to includeStart, "include_edges" to includeEdges,
        ),
    ) as JsonObject

    /** Shortest path by fewest hops; `found: false` when none exists within [maxDepth]. */
    public suspend fun shortestPath(from: String, to: String, direction: String? = null, edgeTypes: List<String>? = null, maxDepth: Int? = null): JsonObject =
        t.request("$g/shortest-path", "POST", body = obj("from" to from, "to" to to, "direction" to direction, "edge_types" to edgeTypes, "max_depth" to maxDepth)) as JsonObject

    public suspend fun stats(): JsonObject = t.request("$g/stats") as JsonObject

    /** Read-only openCypher over the derived Apache AGE graph; parameters are `$name`. */
    public suspend fun cypher(query: String, params: Map<String, Any?>? = null, limit: Int? = null, timeoutMs: Long? = null): JsonObject =
        t.request("$g/cypher", "POST", body = obj("query" to query, "params" to params, "limit" to limit, "timeout_ms" to timeoutMs)) as JsonObject

    private fun page(r: JsonElement) = GraphPage(r.items(), r.objOrNull()?.get("next_after").str())
}

/** Result of one SQL statement. */
public data class SqlResult(
    /** `read`, `write`, `schema` or `maintenance`. */
    val kind: String,
    val columns: List<String>,
    /** One list per row, in [columns] order. At most 10 000 rows are returned ([truncated]). */
    val rows: List<List<JsonElement>>,
    val rowsAffected: Long,
    val truncated: Boolean,
    val durationMs: Long,
) {
    /** One object per row, keyed by column name. */
    public fun asObjects(): List<JsonObject> = rows.map { r -> JsonObject(columns.zip(r).toMap()) }
}

/** SQL editor (`/api/v1/sql`): `fb.sql`. Administrators only by default (the data core's authorizer decides). */
public class SqlApi internal constructor(private val t: Transport) {
    /** Runs one parameterised statement (positional `?` / `?1`). Transaction control, `ATTACH`, `VACUUM` and writable pragmas are refused. */
    public suspend fun execute(sql: String, params: List<Any?> = emptyList()): SqlResult {
        val r = t.request("/api/v1/sql", "POST", body = obj("sql" to sql, "params" to params)) as JsonObject
        return SqlResult(
            kind = r["kind"].str() ?: "read",
            columns = r.items("columns").mapNotNull { it.str() },
            rows = r.items("rows").map { (it as? kotlinx.serialization.json.JsonArray)?.toList() ?: emptyList() },
            rowsAffected = r["rows_affected"].long() ?: 0,
            truncated = (r["truncated"] as? kotlinx.serialization.json.JsonPrimitive)?.content == "true",
            durationMs = r["duration_ms"].long() ?: 0,
        )
    }

    /** [execute], then one object per row keyed by column name. */
    public suspend fun query(sql: String, params: List<Any?> = emptyList()): List<JsonObject> = execute(sql, params).asObjects()

    /** Tables and views (without ForgeBase system tables) with columns and indexes. */
    public suspend fun tables(): List<JsonObject> = t.request("/api/v1/sql/tables").items().map { it as JsonObject }

    public suspend fun table(name: String): JsonObject = t.request("/api/v1/sql/tables/${seg(name)}") as JsonObject
}

/** Prepaid credit balances (reads): `fb.credits`. Top-ups, grants, spends and refunds are server-side operations (JS / Rust SDKs). */
public class CreditsApi internal constructor(private val t: Transport) {
    /** Balance and next expiries of [subject] (the caller by default). */
    public suspend fun balance(subject: String? = null): JsonObject = t.request("/api/v1/credits/balance", query = mapOf("subject" to subject)) as JsonObject

    /** What is left of each top-up and grant, oldest first. */
    public suspend fun lots(subject: String? = null, all: Boolean? = null): List<JsonObject> =
        t.request("/api/v1/credits/lots", query = mapOf("subject" to subject, "all" to all)).items().map { it as JsonObject }

    /** One page of ledger entries in `seq` order; pass `next` back as [after]. */
    public suspend fun ledger(subject: String? = null, kind: String? = null, after: Long? = null, limit: Int? = null): JsonObject =
        t.request("/api/v1/credits/ledger", query = mapOf("subject" to subject, "kind" to kind, "after" to after, "limit" to limit)) as JsonObject

    /** Every ledger entry, page after page. */
    public fun entries(subject: String? = null, kind: String? = null): Flow<JsonObject> = flow {
        var after: Long? = null
        while (true) {
            val page = ledger(subject, kind, after)
            val items = page.items()
            items.forEach { emit(it as JsonObject) }
            after = page["next"].long() ?: return@flow
            if (items.isEmpty()) return@flow
        }
    }
}
