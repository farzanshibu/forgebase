package dev.forgebase

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okio.BufferedSink
import okio.source
import java.io.File
import java.io.RandomAccessFile
import java.security.MessageDigest

/*
 * Object storage (`/api/v1/storage`): `fb.storage`.
 *
 *   val avatars = fb.storage.from("avatars")
 *   avatars.put("u1/me.png", bytes, PutOptions(contentType = "image/png"))
 *   val url = avatars.createSignedUrl("u1/me.png", expiresIn = 600)
 */

@Serializable
public data class Bucket(
    val name: String,
    val visibility: String,
    val versioning: Boolean = false,
    val owner: String = "",
    val tenant: String? = null,
    @SerialName("allowed_content_types") val allowedContentTypes: List<String>? = null,
    @SerialName("max_object_bytes") val maxObjectBytes: Long? = null,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("updated_at") val updatedAt: String = "",
)

@Serializable
public data class StorageObject(
    val bucket: String,
    val key: String,
    val size: Long,
    @SerialName("content_type") val contentType: String = "",
    @SerialName("checksum_sha256") val checksumSha256: String? = null,
    @SerialName("created_at") val createdAt: String = "",
    @SerialName("created_by") val createdBy: String = "",
    @SerialName("delete_marker") val deleteMarker: Boolean = false,
    @SerialName("is_latest") val isLatest: Boolean = true,
    @SerialName("version_id") val versionId: String = "",
    val metadata: Map<String, String> = emptyMap(),
)

@Serializable
public data class ObjectPage(
    val objects: List<StorageObject>,
    val prefixes: List<String> = emptyList(),
    @SerialName("next_cursor") val nextCursor: String? = null,
)

@Serializable
public data class DeleteResult(
    @SerialName("delete_marker") val deleteMarker: Boolean = false,
    @SerialName("version_id") val versionId: String = "",
)

@Serializable
public data class UploadPart(
    @SerialName("part_number") val partNumber: Int,
    val size: Long,
    @SerialName("checksum_sha256") val checksumSha256: String = "",
)

@Serializable
public data class Upload(
    val id: String,
    val bucket: String,
    val key: String,
    val kind: String,
    @SerialName("content_type") val contentType: String = "",
    @SerialName("declared_size") val declaredSize: Long? = null,
    @SerialName("declared_sha256") val declaredSha256: String? = null,
    @SerialName("received_bytes") val receivedBytes: Long = 0,
    @SerialName("expires_at") val expiresAt: String = "",
    val parts: List<UploadPart> = emptyList(),
)

@Serializable
public data class SignedUrl(
    /** Relative to the server root; see [SignedApi.resolve]. */
    val url: String,
    val method: String = "GET",
    @SerialName("expires_at") val expiresAt: String = "",
)

/** Content to upload: bytes, a string or a file, readable in byte ranges (multipart parts, resumable chunks). */
public interface UploadSource {
    public val size: Long

    /** A request body for bytes `[from, to)`. */
    public fun body(from: Long, to: Long, contentType: String?): RequestBody

    public companion object {
        public fun bytes(data: ByteArray): UploadSource = object : UploadSource {
            override val size: Long = data.size.toLong()
            override fun body(from: Long, to: Long, contentType: String?): RequestBody =
                data.toRequestBody(contentType?.toMediaTypeOrNull(), from.toInt(), (to - from).toInt())
        }

        public fun string(data: String): UploadSource = bytes(data.toByteArray(Charsets.UTF_8))

        public fun file(file: File): UploadSource = object : UploadSource {
            override val size: Long = file.length()
            override fun body(from: Long, to: Long, contentType: String?): RequestBody = object : RequestBody() {
                override fun contentType() = contentType?.toMediaTypeOrNull()
                override fun contentLength(): Long = to - from
                override fun writeTo(sink: BufferedSink) {
                    // A fresh handle per send keeps retries and parallel parts independent.
                    RandomAccessFile(file, "r").use { raf ->
                        raf.seek(from)
                        val buf = ByteArray(64 * 1024)
                        var left = to - from
                        while (left > 0) {
                            val n = raf.read(buf, 0, minOf(buf.size.toLong(), left).toInt())
                            if (n < 0) break
                            sink.write(buf, 0, n)
                            left -= n
                        }
                    }
                }
            }
        }
    }
}

/** Hex SHA-256 of [data], for `checksumSha256`. */
public fun sha256Hex(data: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(data).toHex()

/** Hex SHA-256 of a file, streamed. */
public suspend fun sha256Hex(file: File): String = withContext(Dispatchers.IO) {
    val md = MessageDigest.getInstance("SHA-256")
    file.source().use { src ->
        val buf = okio.Buffer()
        while (src.read(buf, 64 * 1024) >= 0) md.update(buf.readByteArray())
    }
    md.digest().toHex()
}

internal fun ByteArray.toHex(): String {
    val sb = StringBuilder(size * 2)
    for (b in this) sb.append(HEX_CHARS[(b.toInt() shr 4) and 0xf]).append(HEX_CHARS[b.toInt() and 0xf])
    return sb.toString()
}

private const val HEX_CHARS = "0123456789abcdef"

public data class CreateBucketInput(
    val name: String,
    /** `private` (default), `public` or `authenticated`. */
    val visibility: String? = null,
    val versioning: Boolean? = null,
    val allowedContentTypes: List<String>? = null,
    val maxObjectBytes: Long? = null,
)

public data class PutOptions(
    val contentType: String? = null,
    /** User metadata, stored with the object and returned on download as `X-ForgeBase-Meta-<name>`. */
    val metadata: Map<String, String> = emptyMap(),
    /** Hex SHA-256 of the content; the server rejects the upload on a mismatch (`422 checksum_mismatch`). */
    val checksumSha256: String? = null,
)

public data class ListObjectsOptions(
    val prefix: String? = null,
    /** Group keys on this character (`"/"`) into `prefixes`. */
    val delimiter: String? = null,
    val limit: Int? = null,
    val cursor: String? = null,
)

/** A byte range of a download. */
public sealed interface ByteRange {
    public fun header(): String

    /** `bytes=start-end` (inclusive end, open when `null`). */
    public data class Range(val start: Long, val end: Long? = null) : ByteRange {
        override fun header(): String = "bytes=$start-${end ?: ""}"
    }

    /** The last [count] bytes. */
    public data class Suffix(val count: Long) : ByteRange {
        override fun header(): String = "bytes=-$count"
    }

    public data class Raw(val value: String) : ByteRange {
        override fun header(): String = value
    }
}

public data class DownloadOptions(
    /** A specific version. */
    val version: String? = null,
    /** Offer this file name (`Content-Disposition: attachment`). */
    val downloadAs: String? = null,
    val range: ByteRange? = null,
)

public data class CreateUploadOptions(
    /** `multipart` (numbered parts, any order; default) or `resumable` (sequential offsets). */
    val kind: String? = null,
    val contentType: String? = null,
    val metadata: Map<String, String>? = null,
    /** Total size; resumable uploads complete themselves when it is reached. */
    val size: Long? = null,
    /** SHA-256 of the whole object, verified on completion. */
    val checksumSha256: String? = null,
)

public data class UploadProgress(val uploadedBytes: Long, val totalBytes: Long)

/** Result of appending a chunk to a resumable upload. */
public data class AppendResult(
    /** Where the next chunk starts. */
    val offset: Long,
    /** The session, while the upload is still open. */
    val upload: Upload? = null,
    /** The finished object, once the declared size was reached. */
    val storageObject: StorageObject? = null,
)

private const val DEFAULT_PART: Long = 8L * 1024 * 1024
private const val META = "X-Forgebase-Meta-"
private const val OCTET = "application/octet-stream"

private fun keyPath(key: String): String = key.split("/").joinToString("/") { seg(it) }

private fun uploadHeaders(checksum: String?, metadata: Map<String, String>): Map<String, String> {
    val h = LinkedHashMap<String, String>()
    if (checksum != null) h["X-Forgebase-Checksum-Sha256"] = checksum
    for ((k, v) in metadata) h["$META$k"] = v
    return h
}

/** Buckets: `fb.storage.buckets`. */
public class BucketsApi internal constructor(private val t: Transport) {
    public suspend fun list(): List<Bucket> = t.request("/api/v1/storage/buckets").items().map { it.decode() }
    public suspend fun get(name: String): Bucket = t.request("/api/v1/storage/buckets/${seg(name)}").decode()

    public suspend fun create(input: CreateBucketInput): Bucket = t.request(
        "/api/v1/storage/buckets", "POST",
        body = obj(
            "name" to input.name, "visibility" to input.visibility, "versioning" to input.versioning,
            "allowed_content_types" to input.allowedContentTypes, "max_object_bytes" to input.maxObjectBytes,
        ),
    ).decode()

    /** Deletes a bucket; `force = true` removes its objects too. */
    public suspend fun delete(name: String, force: Boolean = false) {
        t.request("/api/v1/storage/buckets/${seg(name)}", "DELETE", query = mapOf("force" to if (force) true else null))
    }

    public suspend fun usage(name: String): kotlinx.serialization.json.JsonObject =
        t.request("/api/v1/storage/buckets/${seg(name)}/usage") as kotlinx.serialization.json.JsonObject
}

/** The objects of one bucket: `fb.storage.from("avatars")`. */
public class BucketApi internal constructor(private val t: Transport, public val bucket: String) {
    private fun path(suffix: String = "") = "/api/v1/storage/buckets/${seg(bucket)}$suffix"

    /** Current objects under a prefix, one page. */
    public suspend fun list(o: ListObjectsOptions = ListObjectsOptions()): ObjectPage =
        t.request(path("/objects"), query = mapOf("prefix" to o.prefix, "delimiter" to o.delimiter, "limit" to o.limit, "cursor" to o.cursor)).decode()

    /** Every object under a prefix, lazily, following the cursor. */
    public fun iterate(o: ListObjectsOptions = ListObjectsOptions()): Flow<StorageObject> = flow {
        var cursor: String? = null
        while (true) {
            val page = list(o.copy(cursor = cursor))
            page.objects.forEach { emit(it) }
            cursor = page.nextCursor ?: return@flow
        }
    }

    /** Uploads an object in one request (`PUT`). Overwrites the key (a new version when the bucket is versioned). */
    public suspend fun put(key: String, source: UploadSource, o: PutOptions = PutOptions()): StorageObject {
        val ct = o.contentType ?: OCTET
        return t.request(
            path("/objects/${keyPath(key)}"), "PUT",
            rawBody = source.body(0, source.size, ct), headers = uploadHeaders(o.checksumSha256, o.metadata),
        ).decode()
    }

    public suspend fun put(key: String, data: ByteArray, o: PutOptions = PutOptions()): StorageObject = put(key, UploadSource.bytes(data), o)
    public suspend fun put(key: String, text: String, o: PutOptions = PutOptions(contentType = "text/plain")): StorageObject = put(key, UploadSource.string(text), o)
    public suspend fun put(key: String, file: File, o: PutOptions = PutOptions()): StorageObject = put(key, UploadSource.file(file), o)

    /** Downloads an object: the raw [Response] so the body can be streamed (close it). A range answers `206`. */
    public suspend fun download(key: String, o: DownloadOptions = DownloadOptions()): Response = t.send(
        path("/objects/${keyPath(key)}"), query = mapOf("version" to o.version, "download" to o.downloadAs),
        headers = o.range?.let { mapOf("Range" to it.header()) } ?: emptyMap(),
    )

    public suspend fun text(key: String, o: DownloadOptions = DownloadOptions()): String = download(key, o).readText()
    public suspend fun bytes(key: String, o: DownloadOptions = DownloadOptions()): ByteArray = download(key, o).readBytes()

    /** Streams an object into [file]; resolves with the number of bytes written. */
    public suspend fun downloadTo(key: String, file: File, o: DownloadOptions = DownloadOptions()): Long {
        val res = download(key, o)
        return withContext(Dispatchers.IO) {
            res.use { r ->
                val body = r.body ?: return@use 0L
                file.outputStream().use { out -> body.byteStream().copyTo(out) }
            }
        }
    }

    /** Metadata of an object (or of one version) without reading its bytes. */
    public suspend fun meta(key: String, version: String? = null): StorageObject =
        t.request(path("/meta/${keyPath(key)}"), query = mapOf("version" to version)).decode()

    /** Deletes an object (a delete marker in a versioned bucket), or one version. */
    public suspend fun delete(key: String, version: String? = null): DeleteResult =
        t.request(path("/objects/${keyPath(key)}"), "DELETE", query = mapOf("version" to version)).decode()

    /** Copies an object (no bytes move). */
    public suspend fun copy(key: String, destKey: String, destBucket: String? = null, versionId: String? = null): StorageObject =
        t.request(path("/copy"), "POST", body = obj("key" to key, "dest_key" to destKey, "dest_bucket" to destBucket, "version_id" to versionId)).decode()

    /** Moves (renames) an object. */
    public suspend fun move(key: String, destKey: String, destBucket: String? = null): StorageObject =
        t.request(path("/move"), "POST", body = obj("key" to key, "dest_key" to destKey, "dest_bucket" to destBucket)).decode()

    /** Every version of a key, newest first. */
    public suspend fun versions(key: String): List<StorageObject> = t.request(path("/versions/${keyPath(key)}")).items().map { it.decode() }

    /** Restores an earlier version as a new current version. */
    public suspend fun restore(key: String, versionId: String): StorageObject =
        t.request(path("/restore"), "POST", body = obj("key" to key, "version_id" to versionId)).decode()

    /** A signed URL; [method] is `GET` (default) or `PUT`. */
    public suspend fun sign(
        key: String, method: String? = null, expiresIn: Int? = null, contentType: String? = null,
        downloadAs: String? = null, versionId: String? = null,
    ): SignedUrl = t.request(
        path("/sign"), "POST",
        body = obj(
            "key" to key, "method" to method, "expires_in" to expiresIn, "content_type" to contentType,
            "download_as" to downloadAs, "version_id" to versionId,
        ),
    ).decode()

    /** A download link anyone holding it can use until it expires. */
    public suspend fun createSignedUrl(key: String, expiresIn: Int? = null, downloadAs: String? = null, versionId: String? = null): SignedUrl =
        sign(key, "GET", expiresIn, null, downloadAs, versionId)

    /** An upload link: `PUT` the content to it ([SignedApi.upload]). */
    public suspend fun createSignedUploadUrl(key: String, expiresIn: Int? = null, contentType: String? = null): SignedUrl =
        sign(key, "PUT", expiresIn, contentType)
}

/** Multipart and resumable upload sessions: `fb.storage.uploads`. */
public class UploadsApi internal constructor(private val t: Transport) {
    /** Opens a session for [key]. */
    public suspend fun create(bucket: String, key: String, o: CreateUploadOptions = CreateUploadOptions()): Upload = t.request(
        "/api/v1/storage/buckets/${seg(bucket)}/uploads", "POST",
        body = obj(
            "key" to key, "kind" to o.kind, "content_type" to o.contentType, "metadata" to o.metadata,
            "size" to o.size, "checksum_sha256" to o.checksumSha256,
        ),
    ).decode()

    /** State of a session: [Upload.receivedBytes] is where a resumable client continues. */
    public suspend fun get(id: String): Upload = t.request("/api/v1/storage/uploads/${seg(id)}").decode()

    /** Cancels a session. */
    public suspend fun abort(id: String) {
        t.request("/api/v1/storage/uploads/${seg(id)}", "DELETE")
    }

    /** Uploads one numbered part (1 to 10000) of a multipart session. A part can be sent again. */
    public suspend fun putPart(id: String, partNumber: Int, body: UploadSource, from: Long = 0, to: Long = body.size, checksumSha256: String? = null): UploadPart =
        t.request(
            "/api/v1/storage/uploads/${seg(id)}/parts/$partNumber", "PUT",
            rawBody = body.body(from, to, OCTET), headers = uploadHeaders(checksumSha256, emptyMap()),
        ).decode()

    /** Assembles the parts into the object ([parts]: part numbers in order; default every received part). */
    public suspend fun complete(id: String, parts: List<Int>? = null, checksumSha256: String? = null): StorageObject = t.request(
        "/api/v1/storage/uploads/${seg(id)}/complete", "POST",
        body = obj("parts" to parts?.map { obj("part_number" to it) }, "checksum_sha256" to checksumSha256),
    ).decode()

    /**
     * Appends a chunk (bytes `[from, to)` of [data]) to a resumable session at [offset] (a wrong offset is a
     * [ConflictException] with code `offset_mismatch` and `details.upload_offset`).
     */
    public suspend fun append(id: String, offset: Long, data: UploadSource, from: Long = 0, to: Long = data.size, checksumSha256: String? = null): AppendResult {
        val res = t.send(
            "/api/v1/storage/uploads/${seg(id)}", "PATCH",
            rawBody = data.body(from, to, OCTET),
            headers = uploadHeaders(checksumSha256, emptyMap()) + ("Upload-Offset" to offset.toString()),
        )
        val status = res.code
        val next = res.header("upload-offset")?.toLongOrNull()
        val body = t.parseJson(res)
        return if (status == 201) {
            AppendResult(next ?: (offset + (to - from)), storageObject = body.decode())
        } else {
            val up: Upload = body.decode()
            AppendResult(next ?: up.receivedBytes, upload = up)
        }
    }

    /**
     * Uploads [data] through a multipart session in parts of [partSize] bytes, then completes it. Aborts the session
     * when a part fails. For a file that fits in one request use [BucketApi.put].
     */
    public suspend fun upload(
        bucket: String, key: String, data: UploadSource, contentType: String? = null, metadata: Map<String, String>? = null,
        checksumSha256: String? = null, partSize: Long = DEFAULT_PART, onProgress: ((UploadProgress) -> Unit)? = null,
    ): StorageObject {
        val size = maxOf(1L, partSize)
        val total = data.size
        val session = create(bucket, key, CreateUploadOptions("multipart", contentType, metadata, null, checksumSha256))
        try {
            var n = 0
            var from = 0L
            while (from < total || n == 0) {
                val to = minOf(total, from + size)
                putPart(session.id, ++n, data, from, to)
                onProgress?.invoke(UploadProgress(to, total))
                from += size
            }
            return complete(session.id)
        } catch (e: Throwable) {
            runCatching { abort(session.id) }
            throw e
        }
    }

    public suspend fun upload(bucket: String, key: String, file: File, contentType: String? = null, onProgress: ((UploadProgress) -> Unit)? = null): StorageObject =
        upload(bucket, key, UploadSource.file(file), contentType, onProgress = onProgress)

    /**
     * Sends what a resumable session is still missing: reads the server's offset, then uploads [data] from there
     * in chunks. Call it again after a failure to continue where the server stopped.
     */
    public suspend fun resume(id: String, data: UploadSource, chunkSize: Long = DEFAULT_PART, onProgress: ((UploadProgress) -> Unit)? = null): StorageObject {
        val chunk = maxOf(1L, chunkSize)
        val total = data.size
        var offset = get(id).receivedBytes
        while (true) {
            val to = minOf(total, offset + chunk)
            val r = append(id, offset, data, offset, to)
            offset = r.offset
            onProgress?.invoke(UploadProgress(offset, total))
            r.storageObject?.let { return it }
            if (offset >= total) {
                throw InvalidResponseException("upload $id received all $total bytes but did not complete: declare the size when creating it")
            }
        }
    }
}

/** Signed URLs, used without credentials: `fb.storage.signed`. */
public class SignedApi internal constructor(private val t: Transport) {
    /** The absolute URL of a [SignedUrl] (or its relative path). */
    public fun resolve(url: SignedUrl): String = resolve(url.url)
    public fun resolve(path: String): String = if (Regex("^https?://", RegexOption.IGNORE_CASE).containsMatchIn(path)) path else "${t.baseUrl}$path"

    /** Downloads through a signed URL (no credentials sent). */
    public suspend fun download(url: SignedUrl, range: ByteRange? = null): Response = t.send(
        relative(url.url), anonymous = true, headers = range?.let { mapOf("Range" to it.header()) } ?: emptyMap(),
    )

    /** Uploads through a signed `PUT` URL (no credentials sent). */
    public suspend fun upload(url: SignedUrl, source: UploadSource, o: PutOptions = PutOptions()): StorageObject = t.request(
        relative(url.url), "PUT", anonymous = true,
        rawBody = source.body(0, source.size, o.contentType ?: OCTET), headers = uploadHeaders(o.checksumSha256, o.metadata),
    ).decode()

    public suspend fun upload(url: SignedUrl, data: ByteArray, o: PutOptions = PutOptions()): StorageObject = upload(url, UploadSource.bytes(data), o)

    private fun relative(path: String): String = if (path.startsWith(t.baseUrl)) path.removePrefix(t.baseUrl) else path
}

/** Object storage: `fb.storage`. */
public class StorageApi internal constructor(private val t: Transport) {
    public val buckets: BucketsApi = BucketsApi(t)
    public val uploads: UploadsApi = UploadsApi(t)
    public val signed: SignedApi = SignedApi(t)

    /** The objects of one bucket. */
    public fun from(bucket: String): BucketApi = BucketApi(t, bucket)
}
