package dev.forgebase

import kotlinx.coroutines.flow.toList
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.MockResponse
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

@Serializable
data class Post(val id: String, val title: String, val views: Int = 0)

class RecordsTest : ServerTest() {
    private val page1 = """{"items":[{"id":"1","title":"a"},{"id":"2","title":"b"}],"page":1,"per_page":2,"total_items":3,"total_pages":2}"""

    @Test
    fun `list sends the query parameters`() = blocking {
        server.enqueue(json(page1))
        val res = client().collection("posts").list(
            ListOptions(filter = "views > 1", sort = listOf("-created", "title"), expand = listOf("author"), fields = listOf("id", "title"), perPage = 2, page = 1, skipTotal = true),
        )
        assertEquals(2, res.items.size)
        assertEquals(3L, res.totalItems)
        assertEquals(2, res.totalPages)
        val url = next().requestUrl!!
        assertEquals("/api/v1/collections/posts/records", url.encodedPath)
        assertEquals("views > 1", url.queryParameter("filter"))
        assertEquals("-created,title", url.queryParameter("sort"))
        assertEquals("author", url.queryParameter("expand"))
        assertEquals("id,title", url.queryParameter("fields"))
        assertEquals("2", url.queryParameter("per_page"))
        assertEquals("true", url.queryParameter("skip_total"))
        assertNull(url.queryParameter("cursor"))
    }

    @Test
    fun `list with params uses the typed query endpoint`() = blocking {
        server.enqueue(json(page1))
        client().collection("posts").list(ListOptions(filter = "views > {:min}", params = mapOf("min" to 5), consistency = Consistency.STRONG))
        val r = next()
        assertEquals("POST", r.method)
        assertEquals("/api/v1/collections/posts/records/query", r.path)
        assertEquals("strong", r.getHeader("X-ForgeBase-Consistency"))
        val body = r.jsonBody()
        assertEquals("views > {:min}", body["filter"]?.jsonPrimitive?.content)
        assertEquals(5, (body["params"] as kotlinx.serialization.json.JsonObject)["min"]?.jsonPrimitive?.content?.toInt())
    }

    @Test
    fun `pages follows page numbers and iterate flattens them`() = blocking {
        server.enqueue(json(page1))
        server.enqueue(json("""{"items":[{"id":"3","title":"c"}],"page":2,"per_page":2,"total_items":3,"total_pages":2}"""))
        val ids = client().collection("posts").iterate(ListOptions(perPage = 2)).toList().map { it["id"]?.jsonPrimitive?.content }
        assertEquals(listOf("1", "2", "3"), ids)
        assertEquals("1", next().requestUrl!!.queryParameter("page"))
        assertEquals("2", next().requestUrl!!.queryParameter("page"))
    }

    @Test
    fun `pages follows the cursor when the server gives one`() = blocking {
        server.enqueue(json("""{"items":[{"id":"1"}],"per_page":1,"next_cursor":"c1"}"""))
        server.enqueue(json("""{"items":[{"id":"2"}],"per_page":1}"""))
        val pages = client().collection("posts").pages(ListOptions(perPage = 1)).toList()
        assertEquals(2, pages.size)
        next()
        val second = next().requestUrl!!
        assertEquals("c1", second.queryParameter("cursor"))
        assertNull(second.queryParameter("page"))
    }

    @Test
    fun `first asks for one item without a total`() = blocking {
        server.enqueue(json("""{"items":[{"id":"9","title":"z"}],"per_page":1}"""))
        val r = client().collection("posts").first("title = \"z\"")
        assertEquals("9", r?.get("id")?.jsonPrimitive?.content)
        val url = next().requestUrl!!
        assertEquals("1", url.queryParameter("per_page"))
        assertEquals("true", url.queryParameter("skip_total"))
    }

    @Test
    fun `typed helpers decode with kotlinx serialization`() = blocking {
        server.enqueue(json(page1))
        server.enqueue(json("""{"id":"1","title":"a","views":7,"extra":"ignored"}"""))
        val posts = client().collection("posts")
        val list = posts.listAs<Post>()
        assertEquals(listOf("a", "b"), list.items.map { it.title })
        assertEquals(7, posts.getAs<Post>("1").views)
    }

    @Test
    fun `create, update with a version, replace and delete`() = blocking {
        repeat(4) { server.enqueue(json("""{"id":"1","title":"t","_version":2}""")) }
        server.enqueue(MockResponse().setResponseCode(204))
        val posts = client().collection("posts")
        posts.create(mapOf("title" to "t", "tags" to listOf("a", "b"), "draft" to null))
        posts.createFrom(Post("1", "t"))
        posts.update("1", mapOf("title" to "u"), version = 1)
        posts.replace("1", mapOf("title" to "v"))
        posts.delete("a b")
        val create = next()
        assertEquals("POST", create.method)
        val body = create.jsonBody()
        assertEquals(JsonArray(listOf(JsonPrimitive("a"), JsonPrimitive("b"))), body["tags"])
        assertEquals(kotlinx.serialization.json.JsonNull, body["draft"])
        assertEquals("t", next().jsonBody()["title"]?.jsonPrimitive?.content)
        val upd = next()
        assertEquals("PATCH", upd.method)
        assertEquals("\"1\"", upd.getHeader("If-Match"))
        val rep = next()
        assertEquals("PUT", rep.method)
        assertNull(rep.getHeader("If-Match"))
        assertEquals("/api/v1/collections/posts/records/a%20b", next().path)
    }

    @Test
    fun `a stale version is a ConflictException`() = blocking {
        server.enqueue(json(errorBody("version_conflict", "stale"), 412))
        assertFailsWith<ConflictException> { client().collection("posts").update("1", mapOf("a" to 1), version = 1) }
    }

    @Test
    fun `count, export and import`() = blocking {
        server.enqueue(json("""{"count":42}"""))
        server.enqueue(MockResponse().setBody("id,title\n1,a\n"))
        server.enqueue(json("""{"created":1,"updated":0,"failed":0}"""))
        val posts = client().collection("posts")
        assertEquals(42L, posts.count("views > 1"))
        assertEquals("id,title\n1,a\n", posts.export(TransferFormat.CSV))
        posts.import(TransferFormat.NDJSON, "{\"id\":\"1\"}\n")
        assertEquals("views > 1", next().requestUrl!!.queryParameter("filter"))
        assertEquals("csv", next().requestUrl!!.queryParameter("format"))
        val imp = next()
        assertEquals("ndjson", imp.requestUrl!!.queryParameter("format"))
        assertEquals("application/octet-stream", imp.getHeader("Content-Type"))
    }

    @Test
    fun `collections api lists and fetches definitions`() = blocking {
        server.enqueue(json("""{"items":[{"name":"posts"}]}"""))
        server.enqueue(json("""{"name":"posts"}"""))
        val fb = client()
        assertEquals(1, fb.collections.list().size)
        assertEquals("posts", fb.collections.get("posts")["name"]?.jsonPrimitive?.content)
    }
}
