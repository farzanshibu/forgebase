package dev.forgebase

import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.RecordedRequest
import okio.Buffer
import java.io.File
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class StorageTest : ServerTest() {
    private fun obj(key: String, size: Long = 5) =
        """{"bucket":"avatars","key":"$key","size":$size,"content_type":"image/png","created_at":"t","created_by":"u","delete_marker":false,"is_latest":true,"version_id":"v1","metadata":{"owner":"me"}}"""

    @Test
    fun `put sends bytes with content type, metadata and checksum`() = blocking {
        server.enqueue(json(obj("u1/me.png")))
        val data = byteArrayOf(1, 2, 3, 4, 5)
        val o = client().storage.from("avatars").put("u1/me.png", data, PutOptions("image/png", mapOf("owner" to "me"), sha256Hex(data)))
        assertEquals("v1", o.versionId)
        assertEquals("me", o.metadata["owner"])
        val r = next()
        assertEquals("PUT", r.method)
        assertEquals("/api/v1/storage/buckets/avatars/objects/u1/me.png", r.path)
        assertEquals("image/png", r.getHeader("Content-Type"))
        assertEquals("me", r.getHeader("X-Forgebase-Meta-owner"))
        assertEquals(sha256Hex(data), r.getHeader("X-Forgebase-Checksum-Sha256"))
        assertContentEquals(data, r.body.readByteArray())
    }

    @Test
    fun `keys are percent encoded per segment`() = blocking {
        server.enqueue(json(obj("a b/c#d.txt")))
        client().storage.from("my bucket").put("a b/c#d.txt", "hi")
        assertEquals("/api/v1/storage/buckets/my%20bucket/objects/a%20b/c%23d.txt", next().path)
    }

    @Test
    fun `sha256 of known input`() {
        assertEquals("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", sha256Hex("abc".toByteArray()))
    }

    @Test
    fun `download returns the response, with range, and helpers read it`() = blocking {
        server.enqueue(MockResponse().setResponseCode(206).setBody("ello"))
        server.enqueue(MockResponse().setBody("hello"))
        server.enqueue(MockResponse().setBody("hello"))
        val bucket = client().storage.from("avatars")
        val res = bucket.download("k", DownloadOptions(version = "v9", downloadAs = "x.txt", range = ByteRange.Range(1, 4)))
        assertEquals(206, res.code)
        assertEquals("ello", res.readText())
        val r = next()
        assertEquals("bytes=1-4", r.getHeader("Range"))
        assertEquals("v9", r.requestUrl!!.queryParameter("version"))
        assertEquals("x.txt", r.requestUrl!!.queryParameter("download"))
        assertEquals("hello", bucket.text("k"))
        val f = File.createTempFile("fbtest", ".bin")
        try {
            assertEquals(5L, bucket.downloadTo("k", f))
            assertEquals("hello", f.readText())
        } finally {
            f.delete()
        }
        assertEquals("bytes=-100", ByteRange.Suffix(100).header())
    }

    @Test
    fun `missing object is a NotFoundException`() = blocking {
        server.enqueue(json(errorBody("not_found", "no such object"), 404))
        assertFailsWith<NotFoundException> { client().storage.from("avatars").bytes("nope") }
    }

    @Test
    fun `list, iterate, meta, delete, copy, move, versions, restore`() = blocking {
        server.enqueue(json("""{"objects":[${obj("a")}],"prefixes":["d/"],"next_cursor":"c1"}"""))
        server.enqueue(json("""{"objects":[${obj("b")}],"prefixes":[]}"""))
        server.enqueue(json(obj("a")))
        server.enqueue(json("""{"delete_marker":true,"version_id":"v2"}"""))
        server.enqueue(json(obj("a2")))
        server.enqueue(json(obj("a3")))
        server.enqueue(json("""{"items":[${obj("a")}]}"""))
        server.enqueue(json(obj("a")))
        val b = client().storage.from("avatars")
        val keys = mutableListOf<String>()
        b.iterate(ListObjectsOptions(prefix = "", delimiter = "/", limit = 1)).collect { keys.add(it.key) }
        assertEquals(listOf("a", "b"), keys)
        assertEquals("a", b.meta("a").key)
        assertTrue(b.delete("a").deleteMarker)
        b.copy("a", "a2", destBucket = "other")
        b.move("a2", "a3")
        assertEquals(1, b.versions("a").size)
        b.restore("a", "v1")
        val first = next()
        assertEquals("/", first.requestUrl!!.queryParameter("delimiter"))
        assertNull(first.requestUrl!!.queryParameter("cursor"))
        assertEquals("c1", next().requestUrl!!.queryParameter("cursor"))
        assertEquals("/api/v1/storage/buckets/avatars/meta/a", next().path)
        assertEquals("DELETE", next().method)
        assertEquals("other", next().jsonBody()["dest_bucket"].toString().trim('"'))
        assertEquals("/api/v1/storage/buckets/avatars/move", next().path)
        assertEquals("/api/v1/storage/buckets/avatars/versions/a", next().path)
        assertEquals("/api/v1/storage/buckets/avatars/restore", next().path)
    }

    @Test
    fun `signed urls resolve against the server and are used without credentials`() = blocking {
        server.enqueue(json("""{"url":"/api/v1/storage/signed/abc?sig=1","method":"GET","expires_at":"t"}"""))
        server.enqueue(MockResponse().setBody("hello"))
        server.enqueue(json(obj("k")))
        val fb = client(apiKey = "fb_key")
        val s = fb.storage.from("avatars").createSignedUrl("k", expiresIn = 600, downloadAs = "x.png")
        val sign = next().jsonBody()
        assertEquals("600", sign["expires_in"].toString())
        assertEquals("GET", sign["method"].toString().trim('"'))
        assertEquals("${server.url("/").toString().trimEnd('/')}/api/v1/storage/signed/abc?sig=1", fb.storage.signed.resolve(s))
        assertEquals("hello", fb.storage.signed.download(s).readText())
        val dl = next()
        assertNull(dl.getHeader("X-API-Key"))
        assertEquals("/api/v1/storage/signed/abc?sig=1", dl.path)
        fb.storage.signed.upload(SignedUrl("/api/v1/storage/signed/up?sig=2", "PUT"), byteArrayOf(1))
        val up = next()
        assertEquals("PUT", up.method)
        assertNull(up.getHeader("X-API-Key"))
    }

    @Test
    fun `multipart upload sends numbered parts then completes, and aborts on failure`() = blocking {
        val parts = mutableListOf<Pair<String, Int>>()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val p = request.path!!
                return when {
                    p.endsWith("/uploads") -> json("""{"id":"up1","bucket":"b","key":"k","kind":"multipart","parts":[]}""")
                    p.contains("/parts/") -> {
                        val n = p.substringAfterLast('/').toInt()
                        parts.add(request.body.readUtf8() to n)
                        json("""{"part_number":$n,"size":${request.bodySize},"checksum_sha256":"x"}""")
                    }
                    p.endsWith("/complete") -> json(obj("k", 10))
                    else -> MockResponse().setResponseCode(500)
                }
            }
        }
        val progress = mutableListOf<UploadProgress>()
        val o = client().storage.uploads.upload("b", "k", UploadSource.string("0123456789"), partSize = 4, onProgress = { progress.add(it) })
        assertEquals("k", o.key)
        assertEquals(listOf("0123" to 1, "4567" to 2, "89" to 3), parts)
        assertEquals(listOf(4L, 8L, 10L), progress.map { it.uploadedBytes })
    }

    @Test
    fun `multipart upload aborts the session when a part fails`() = blocking {
        val aborted = java.util.concurrent.atomic.AtomicBoolean()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = when {
                request.path!!.endsWith("/uploads") -> json("""{"id":"up1","bucket":"b","key":"k","kind":"multipart","parts":[]}""")
                request.path!!.contains("/parts/") -> json(errorBody("checksum_mismatch", "bad part"), 422)
                request.method == "DELETE" -> MockResponse().setResponseCode(204).also { aborted.set(true) }
                else -> MockResponse().setResponseCode(500)
            }
        }
        assertFailsWith<ValidationException> { client().storage.uploads.upload("b", "k", UploadSource.string("0123456789"), partSize = 4) }
        assertTrue(aborted.get())
    }

    @Test
    fun `file sources slice by range`() = blocking {
        val f = File.createTempFile("fbtest", ".bin")
        try {
            f.writeText("0123456789")
            val src = UploadSource.file(f)
            assertEquals(10L, src.size)
            val buf = Buffer()
            src.body(3, 7, "application/octet-stream").writeTo(buf)
            assertEquals("3456", buf.readUtf8())
            assertEquals(4L, src.body(3, 7, null).contentLength())
        } finally {
            f.delete()
        }
    }

    @Test
    fun `resumable upload continues from the server offset`() = blocking {
        val appended = mutableListOf<Pair<Long, String>>()
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse = when (request.method) {
                "GET" -> json("""{"id":"up1","bucket":"b","key":"k","kind":"resumable","declared_size":10,"received_bytes":4,"parts":[]}""")
                "PATCH" -> {
                    val off = request.getHeader("Upload-Offset")!!.toLong()
                    val chunk = request.body.readUtf8()
                    appended.add(off to chunk)
                    val next = off + chunk.length
                    if (next >= 10) json(obj("k", 10), 201).setHeader("Upload-Offset", next.toString())
                    else json("""{"id":"up1","bucket":"b","key":"k","kind":"resumable","declared_size":10,"received_bytes":$next,"parts":[]}""").setHeader("Upload-Offset", next.toString())
                }
                else -> MockResponse().setResponseCode(500)
            }
        }
        val o = client().storage.uploads.resume("up1", UploadSource.string("0123456789"), chunkSize = 3)
        assertEquals(10L, o.size)
        assertEquals(listOf(4L to "456", 7L to "789"), appended)
    }

    @Test
    fun `a wrong resumable offset is a ConflictException with the server offset`() = blocking {
        server.enqueue(json(errorBody("offset_mismatch", "wrong offset", """{"upload_offset":6}"""), 409))
        val e = assertFailsWith<ConflictException> { client().storage.uploads.append("up1", 3, UploadSource.string("abc")) }
        assertEquals("offset_mismatch", e.code)
        assertNotNull(e.details?.get("upload_offset"))
    }

    @Test
    fun `buckets api`() = blocking {
        server.enqueue(json("""{"items":[{"name":"avatars","visibility":"private","versioning":false,"owner":"u","created_at":"t","updated_at":"t"}]}"""))
        server.enqueue(json("""{"name":"docs","visibility":"public","versioning":true,"allowed_content_types":["image/*"],"max_object_bytes":1000}"""))
        server.enqueue(MockResponse().setResponseCode(204))
        val fb = client()
        assertEquals("avatars", fb.storage.buckets.list().single().name)
        val b = fb.storage.buckets.create(CreateBucketInput("docs", visibility = "public", versioning = true, allowedContentTypes = listOf("image/*"), maxObjectBytes = 1000))
        assertEquals(1000L, b.maxObjectBytes)
        fb.storage.buckets.delete("docs", force = true)
        next()
        assertEquals("public", next().jsonBody()["visibility"].toString().trim('"'))
        assertEquals("true", next().requestUrl!!.queryParameter("force"))
    }
}
