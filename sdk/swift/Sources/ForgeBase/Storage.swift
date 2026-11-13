import CryptoKit
import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

// MARK: - wire types

public struct Bucket: Codable, Sendable, Equatable {
    public let name: String
    /// `private`, `public` or `authenticated`.
    public let visibility: String
    public let versioning: Bool
    public let owner: String
    public let tenant: String?
    public let allowedContentTypes: [String]?
    public let maxObjectBytes: Int64?
    public let createdAt: String
    public let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case name, visibility, versioning, owner, tenant
        case allowedContentTypes = "allowed_content_types", maxObjectBytes = "max_object_bytes"
        case createdAt = "created_at", updatedAt = "updated_at"
    }
}

public struct BucketUsage: Codable, Sendable, Equatable {
    public let bucket: String
    public let objects: Int64
    public let versions: Int64
    public let logicalBytes: Int64
    public let versionBytes: Int64

    enum CodingKeys: String, CodingKey {
        case bucket, objects, versions, logicalBytes = "logical_bytes", versionBytes = "version_bytes"
    }
}

public struct StorageObject: Codable, Sendable, Equatable {
    public let bucket: String
    public let key: String
    public let versionId: String
    public let size: Int64
    public let contentType: String
    public let metadata: [String: String]
    public let checksumSha256: String?
    public let isLatest: Bool
    public let deleteMarker: Bool
    public let createdBy: String
    public let createdAt: String

    enum CodingKeys: String, CodingKey {
        case bucket, key, versionId = "version_id", size, contentType = "content_type", metadata
        case checksumSha256 = "checksum_sha256", isLatest = "is_latest", deleteMarker = "delete_marker"
        case createdBy = "created_by", createdAt = "created_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        bucket = try c.decode(String.self, forKey: .bucket)
        key = try c.decode(String.self, forKey: .key)
        versionId = try c.decode(String.self, forKey: .versionId)
        size = try c.decode(Int64.self, forKey: .size)
        contentType = try c.decode(String.self, forKey: .contentType)
        let raw = try c.decodeIfPresent([String: JSON].self, forKey: .metadata) ?? [:]
        metadata = raw.mapValues { $0.stringValue ?? $0.description }
        checksumSha256 = try c.decodeIfPresent(String.self, forKey: .checksumSha256)
        isLatest = try c.decodeIfPresent(Bool.self, forKey: .isLatest) ?? true
        deleteMarker = try c.decodeIfPresent(Bool.self, forKey: .deleteMarker) ?? false
        createdBy = try c.decodeIfPresent(String.self, forKey: .createdBy) ?? ""
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
    }
}

public struct ObjectPage: Decodable, Sendable, Equatable {
    public let objects: [StorageObject]
    /// "Folders" when listing with a delimiter.
    public let prefixes: [String]
    public let nextCursor: String?

    enum CodingKeys: String, CodingKey { case objects, prefixes, nextCursor = "next_cursor" }
}

public struct DeleteResult: Decodable, Sendable, Equatable {
    public let versionId: String
    public let deleteMarker: Bool
    enum CodingKeys: String, CodingKey { case versionId = "version_id", deleteMarker = "delete_marker" }
}

public struct UploadPart: Codable, Sendable, Equatable {
    public let partNumber: Int
    public let size: Int64
    public let checksumSha256: String?
    enum CodingKeys: String, CodingKey { case partNumber = "part_number", size, checksumSha256 = "checksum_sha256" }
}

/// A multipart or resumable upload session.
public struct Upload: Codable, Sendable, Equatable {
    public let id: String
    public let bucket: String
    public let key: String
    /// `multipart` or `resumable`.
    public let kind: String
    public let contentType: String
    /// Where a resumable client continues.
    public let receivedBytes: Int64
    public let declaredSize: Int64?
    public let declaredSha256: String?
    public let parts: [UploadPart]
    public let expiresAt: String

    enum CodingKeys: String, CodingKey {
        case id, bucket, key, kind, contentType = "content_type", receivedBytes = "received_bytes"
        case declaredSize = "declared_size", declaredSha256 = "declared_sha256", parts, expiresAt = "expires_at"
    }
}

public struct SignedURL: Codable, Sendable, Equatable {
    /// Relative to the server root (or absolute).
    public let url: String
    /// `GET` or `PUT`.
    public let method: String
    public let expiresAt: String
    enum CodingKeys: String, CodingKey { case url, method, expiresAt = "expires_at" }
}

public struct AclEntry: Codable, Sendable, Equatable {
    public var grantee: String
    /// `read`, `write` or `admin`.
    public var permission: String
    public var prefix: String?
    public init(grantee: String, permission: String, prefix: String? = nil) {
        self.grantee = grantee
        self.permission = permission
        self.prefix = prefix
    }
}

// MARK: - options

public struct CreateBucketInput: Sendable {
    public var name: String
    /// `private` (default), `public` or `authenticated`.
    public var visibility: String?
    public var versioning: Bool?
    /// Allowed content types; `image/*` patterns work.
    public var allowedContentTypes: [String]?
    public var maxObjectBytes: Int64?
    public init(name: String, visibility: String? = nil, versioning: Bool? = nil, allowedContentTypes: [String]? = nil, maxObjectBytes: Int64? = nil) {
        self.name = name
        self.visibility = visibility
        self.versioning = versioning
        self.allowedContentTypes = allowedContentTypes
        self.maxObjectBytes = maxObjectBytes
    }
}

public struct UpdateBucketInput: Sendable {
    public var visibility: String?
    public var versioning: Bool?
    /// An empty list clears the allow list.
    public var allowedContentTypes: [String]?
    /// `0` clears the limit.
    public var maxObjectBytes: Int64?
    public init(visibility: String? = nil, versioning: Bool? = nil, allowedContentTypes: [String]? = nil, maxObjectBytes: Int64? = nil) {
        self.visibility = visibility
        self.versioning = versioning
        self.allowedContentTypes = allowedContentTypes
        self.maxObjectBytes = maxObjectBytes
    }
}

public struct PutOptions: Sendable {
    public var contentType: String?
    /// User metadata, stored with the object and returned on download as `X-ForgeBase-Meta-<name>`.
    public var metadata: [String: String]
    /// Hex SHA-256 of the content (see `sha256Hex`); the server rejects the upload on a mismatch (`422 checksum_mismatch`).
    public var checksumSha256: String?
    public init(contentType: String? = nil, metadata: [String: String] = [:], checksumSha256: String? = nil) {
        self.contentType = contentType
        self.metadata = metadata
        self.checksumSha256 = checksumSha256
    }
}

public struct ListObjectsOptions: Sendable {
    public var prefix: String?
    /// Group keys on this character (`"/"`) into `prefixes`.
    public var delimiter: String?
    public var limit: Int?
    public var cursor: String?
    public init(prefix: String? = nil, delimiter: String? = nil, limit: Int? = nil, cursor: String? = nil) {
        self.prefix = prefix
        self.delimiter = delimiter
        self.limit = limit
        self.cursor = cursor
    }
}

/// A byte range of a download.
public enum ByteRange: Sendable {
    /// A raw header value: `bytes=0-99`.
    case header(String)
    /// Inclusive range; `end == nil` reads to the end.
    case bytes(start: Int64, end: Int64?)
    /// The last `n` bytes.
    case suffix(Int64)

    var headerValue: String {
        switch self {
        case .header(let h): return h
        case .bytes(let s, let e): return "bytes=\(s)-\(e.map(String.init) ?? "")"
        case .suffix(let n): return "bytes=-\(n)"
        }
    }
}

public struct DownloadOptions: Sendable {
    /// A specific version.
    public var version: String?
    /// Offer this file name (`Content-Disposition: attachment`).
    public var downloadAs: String?
    public var range: ByteRange?
    public init(version: String? = nil, downloadAs: String? = nil, range: ByteRange? = nil) {
        self.version = version
        self.downloadAs = downloadAs
        self.range = range
    }
}

/// A downloaded object.
public struct Download: Sendable {
    public let data: Data
    /// 200, or 206 for a range.
    public let statusCode: Int
    public let contentType: String?
    public let etag: String?
    public let headers: [String: String]
}

public struct SignOptions: Sendable {
    /// `GET` (default) or `PUT`.
    public var method: String?
    /// Lifetime in seconds (default 900).
    public var expiresIn: Int?
    /// Uploads: the content type the upload must declare.
    public var contentType: String?
    /// Downloads: file name to offer.
    public var downloadAs: String?
    /// Downloads: a specific version.
    public var versionId: String?
    public init(method: String? = nil, expiresIn: Int? = nil, contentType: String? = nil, downloadAs: String? = nil, versionId: String? = nil) {
        self.method = method
        self.expiresIn = expiresIn
        self.contentType = contentType
        self.downloadAs = downloadAs
        self.versionId = versionId
    }
}

public struct CreateUploadOptions: Sendable {
    /// `multipart` (numbered parts, any order; default) or `resumable` (sequential offsets).
    public var kind: String?
    public var contentType: String?
    public var metadata: [String: String]
    /// Total size; resumable uploads complete themselves when it is reached.
    public var size: Int64?
    /// SHA-256 of the whole object, verified on completion.
    public var checksumSha256: String?
    public init(kind: String? = nil, contentType: String? = nil, metadata: [String: String] = [:], size: Int64? = nil, checksumSha256: String? = nil) {
        self.kind = kind
        self.contentType = contentType
        self.metadata = metadata
        self.size = size
        self.checksumSha256 = checksumSha256
    }
}

public struct UploadProgress: Sendable, Equatable {
    public let uploadedBytes: Int64
    public let totalBytes: Int64
    public var fraction: Double { totalBytes == 0 ? 1 : Double(uploadedBytes) / Double(totalBytes) }
}

/// Result of appending a chunk to a resumable upload.
public struct AppendResult: Sendable {
    /// Where the next chunk starts.
    public let offset: Int64
    /// The session, while the upload is open.
    public let upload: Upload?
    /// The finished object, once the declared size was reached.
    public let object: StorageObject?
}

/// Bytes to upload: in memory, or read from a file in slices (nothing but one part is held in memory).
public enum UploadSource: Sendable {
    case data(Data)
    case file(URL)

    public func size() throws -> Int64 {
        switch self {
        case .data(let d): return Int64(d.count)
        case .file(let u):
            let attrs = try FileManager.default.attributesOfItem(atPath: u.path)
            return (attrs[.size] as? NSNumber)?.int64Value ?? 0
        }
    }

    /// Up to `length` bytes starting at `offset`.
    public func read(offset: Int64, length: Int64) throws -> Data {
        switch self {
        case .data(let d):
            let start = Int(min(offset, Int64(d.count)))
            let end = Int(min(offset + length, Int64(d.count)))
            return d.subdata(in: (d.startIndex + start)..<(d.startIndex + end))
        case .file(let u):
            let h = try FileHandle(forReadingFrom: u)
            defer { try? h.close() }
            try h.seek(toOffset: UInt64(offset))
            return try h.read(upToCount: Int(length)) ?? Data()
        }
    }
}

/// Hex SHA-256 of bytes, for `checksumSha256`.
public func sha256Hex(_ data: Data) -> String {
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
}

/// Hex SHA-256 of a file, read in 1 MiB slices.
public func sha256Hex(fileAt url: URL) throws -> String {
    let h = try FileHandle(forReadingFrom: url)
    defer { try? h.close() }
    var hasher = SHA256()
    while let chunk = try h.read(upToCount: 1 << 20), !chunk.isEmpty { hasher.update(data: chunk) }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
}

private let metaPrefix = "X-Forgebase-Meta-"

private func uploadHeaders(contentType: String?, metadata: [String: String], checksum: String?) -> [String: String] {
    var h: [String: String] = [:]
    if let checksum { h["X-Forgebase-Checksum-Sha256"] = checksum }
    for (k, v) in metadata { h[metaPrefix + k] = v }
    _ = contentType
    return h
}

// MARK: - buckets

/// Buckets: `fb.storage.buckets`.
public struct BucketsAPI: Sendable {
    let t: Transport

    /// Buckets the caller may read.
    public func list() async throws -> [Bucket] {
        try await t.json(Request("GET", "/api/v1/storage/buckets"))["items"]?.decode([Bucket].self) ?? []
    }

    public func get(_ name: String) async throws -> Bucket {
        try await t.decode(Request("GET", "/api/v1/storage/buckets/\(seg(name))"))
    }

    public func create(_ input: CreateBucketInput) async throws -> Bucket {
        try await t.decode(
            Request(
                "POST", "/api/v1/storage/buckets",
                body: wire([
                    "name": input.name, "visibility": input.visibility, "versioning": input.versioning,
                    "allowed_content_types": input.allowedContentTypes, "max_object_bytes": input.maxObjectBytes,
                ])))
    }

    /// Changes a bucket's policy.
    public func update(_ name: String, _ patch: UpdateBucketInput) async throws -> Bucket {
        try await t.decode(
            Request(
                "PATCH", "/api/v1/storage/buckets/\(seg(name))",
                body: wire([
                    "visibility": patch.visibility, "versioning": patch.versioning,
                    "allowed_content_types": patch.allowedContentTypes, "max_object_bytes": patch.maxObjectBytes,
                ])))
    }

    /// Deletes a bucket; `force: true` removes its objects too.
    public func delete(_ name: String, force: Bool = false) async throws {
        try await t.perform(Request("DELETE", "/api/v1/storage/buckets/\(seg(name))", query: query(["force": force ? true : nil])))
    }

    /// The ACL of a bucket (needs admin access on it).
    public func acl(_ name: String) async throws -> [AclEntry] {
        try await t.json(Request("GET", "/api/v1/storage/buckets/\(seg(name))/acl"))["entries"]?.decode([AclEntry].self) ?? []
    }

    /// Replaces the whole ACL.
    public func setACL(_ name: String, entries: [AclEntry]) async throws -> [AclEntry] {
        let body: JSON = ["entries": try JSON(encoding: entries)]
        return try await t.json(Request("PUT", "/api/v1/storage/buckets/\(seg(name))/acl", body: body))["entries"]?.decode([AclEntry].self) ?? []
    }

    /// Space used by a bucket.
    public func usage(_ name: String) async throws -> BucketUsage {
        try await t.decode(Request("GET", "/api/v1/storage/buckets/\(seg(name))/usage"))
    }
}

// MARK: - objects

/// The objects of one bucket: `fb.storage.from("avatars")`.
public struct BucketAPI: Sendable {
    let t: Transport
    public let bucket: String

    private func path(_ suffix: String = "") -> String { "/api/v1/storage/buckets/\(seg(bucket))\(suffix)" }

    /// Current objects under a prefix, one page.
    public func list(_ o: ListObjectsOptions = .init()) async throws -> ObjectPage {
        try await t.decode(
            Request("GET", path("/objects"), query: query(["prefix": o.prefix, "delimiter": o.delimiter, "limit": o.limit, "cursor": o.cursor])))
    }

    /// Every object under a prefix, lazily, following the cursor.
    public func iterate(prefix: String? = nil, delimiter: String? = nil, limit: Int? = nil) -> AsyncThrowingStream<StorageObject, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var cursor: String?
                    repeat {
                        let page = try await list(ListObjectsOptions(prefix: prefix, delimiter: delimiter, limit: limit, cursor: cursor))
                        for o in page.objects { continuation.yield(o) }
                        cursor = page.nextCursor
                    } while cursor != nil
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Uploads an object in one request (`PUT`). Overwrites the key (a new version when the bucket is versioned).
    /// A `.file` source is streamed from disk by `URLSession` without loading it into memory.
    @discardableResult
    public func put(_ key: String, _ source: UploadSource, options o: PutOptions = .init()) async throws -> StorageObject {
        var r = Request("PUT", path("/objects/\(keyPath(key))"))
        let ct = o.contentType ?? "application/octet-stream"
        switch source {
        case .data(let d): r.body = .data(d, contentType: ct)
        case .file(let u): r.body = .file(u, contentType: ct)
        }
        r.headers = uploadHeaders(contentType: ct, metadata: o.metadata, checksum: o.checksumSha256)
        return try await t.decode(r)
    }

    @discardableResult
    public func put(_ key: String, data: Data, options: PutOptions = .init()) async throws -> StorageObject {
        try await put(key, .data(data), options: options)
    }

    @discardableResult
    public func put(_ key: String, fileURL: URL, options: PutOptions = .init()) async throws -> StorageObject {
        try await put(key, .file(fileURL), options: options)
    }

    /// Downloads an object into memory. A range answers `206`.
    public func download(_ key: String, _ o: DownloadOptions = .init()) async throws -> Download {
        var r = Request("GET", path("/objects/\(keyPath(key))"), query: query(["version": o.version, "download": o.downloadAs]))
        r.accept = "*/*"
        if let range = o.range { r.headers["Range"] = range.headerValue }
        let res = try await t.send(r)
        var headers: [String: String] = [:]
        for (k, v) in res.http.allHeaderFields { headers[String(describing: k).lowercased()] = String(describing: v) }
        return Download(
            data: res.data, statusCode: res.http.statusCode, contentType: res.http.value(forHTTPHeaderField: "Content-Type"),
            etag: res.http.value(forHTTPHeaderField: "ETag"), headers: headers)
    }

    /// Downloads an object straight to a file (large objects: nothing is held in memory). Returns the response headers.
    @discardableResult
    public func download(_ key: String, to destination: URL, _ o: DownloadOptions = .init()) async throws -> HTTPURLResponse {
        var r = Request("GET", path("/objects/\(keyPath(key))"), query: query(["version": o.version, "download": o.downloadAs]))
        r.accept = "*/*"
        if let range = o.range { r.headers["Range"] = range.headerValue }
        return try await t.download(r, to: destination)
    }

    /// The object as bytes.
    public func data(_ key: String, _ o: DownloadOptions = .init()) async throws -> Data { try await download(key, o).data }

    /// The object as text.
    public func text(_ key: String, _ o: DownloadOptions = .init()) async throws -> String {
        String(decoding: try await data(key, o), as: UTF8.self)
    }

    /// The object parsed as JSON into `T`.
    public func json<T: Decodable>(_ key: String, as type: T.Type = T.self, _ o: DownloadOptions = .init()) async throws -> T {
        try JSONDecoder().decode(T.self, from: try await data(key, o))
    }

    /// Metadata of an object (or of one `version`) without reading its bytes.
    public func meta(_ key: String, version: String? = nil) async throws -> StorageObject {
        try await t.decode(Request("GET", path("/meta/\(keyPath(key))"), query: query(["version": version])))
    }

    /// Deletes an object (a delete marker in a versioned bucket), or one version with `version`.
    @discardableResult
    public func delete(_ key: String, version: String? = nil) async throws -> DeleteResult {
        try await t.decode(Request("DELETE", path("/objects/\(keyPath(key))"), query: query(["version": version])))
    }

    /// Copies an object (no bytes move).
    public func copy(_ key: String, to destKey: String, destBucket: String? = nil, versionId: String? = nil) async throws -> StorageObject {
        try await t.decode(
            Request("POST", path("/copy"), body: wire(["key": key, "dest_key": destKey, "dest_bucket": destBucket, "version_id": versionId])))
    }

    /// Moves (renames) an object.
    public func move(_ key: String, to destKey: String, destBucket: String? = nil) async throws -> StorageObject {
        try await t.decode(Request("POST", path("/move"), body: wire(["key": key, "dest_key": destKey, "dest_bucket": destBucket])))
    }

    /// Every version of a key, newest first.
    public func versions(_ key: String) async throws -> [StorageObject] {
        try await t.json(Request("GET", path("/versions/\(keyPath(key))")))["items"]?.decode([StorageObject].self) ?? []
    }

    /// Restores an earlier version as a new current version.
    public func restore(_ key: String, versionId: String) async throws -> StorageObject {
        try await t.decode(Request("POST", path("/restore"), body: wire(["key": key, "version_id": versionId])))
    }

    /// A signed URL (relative to the server root; see `storage.signed`).
    public func sign(_ key: String, _ o: SignOptions = .init()) async throws -> SignedURL {
        try await t.decode(
            Request(
                "POST", path("/sign"),
                body: wire([
                    "key": key, "method": o.method, "expires_in": o.expiresIn, "content_type": o.contentType,
                    "download_as": o.downloadAs, "version_id": o.versionId,
                ])))
    }

    /// A download link anyone holding it can use until it expires.
    public func createSignedURL(_ key: String, expiresIn: Int? = nil, downloadAs: String? = nil, versionId: String? = nil) async throws -> SignedURL {
        try await sign(key, SignOptions(method: "GET", expiresIn: expiresIn, downloadAs: downloadAs, versionId: versionId))
    }

    /// An upload link: `PUT` the content to it (`storage.signed.upload` or a background `URLSession` upload task).
    public func createSignedUploadURL(_ key: String, expiresIn: Int? = nil, contentType: String? = nil) async throws -> SignedURL {
        try await sign(key, SignOptions(method: "PUT", expiresIn: expiresIn, contentType: contentType))
    }
}

// MARK: - multipart and resumable uploads

/// Multipart and resumable upload sessions: `fb.storage.uploads`.
public struct UploadsAPI: Sendable {
    let t: Transport
    /// Default part / chunk size: 8 MiB.
    public static let defaultPartSize: Int64 = 8 * 1024 * 1024

    /// Opens a session for `key`.
    public func create(bucket: String, key: String, _ o: CreateUploadOptions = .init()) async throws -> Upload {
        try await t.decode(
            Request(
                "POST", "/api/v1/storage/buckets/\(seg(bucket))/uploads",
                body: wire([
                    "key": key, "kind": o.kind, "content_type": o.contentType,
                    "metadata": o.metadata.isEmpty ? nil : o.metadata, "size": o.size, "checksum_sha256": o.checksumSha256,
                ])))
    }

    /// State of a session: `receivedBytes` is where a resumable client continues.
    public func get(_ id: String) async throws -> Upload {
        try await t.decode(Request("GET", "/api/v1/storage/uploads/\(seg(id))"))
    }

    /// Cancels a session.
    public func abort(_ id: String) async throws {
        try await t.perform(Request("DELETE", "/api/v1/storage/uploads/\(seg(id))"))
    }

    /// Uploads one numbered part (1 to 10000) of a multipart session. A part can be sent again.
    public func putPart(_ id: String, partNumber: Int, data: Data, checksumSha256: String? = nil) async throws -> UploadPart {
        var r = Request("PUT", "/api/v1/storage/uploads/\(seg(id))/parts/\(partNumber)")
        r.body = .data(data, contentType: "application/octet-stream")
        r.headers = uploadHeaders(contentType: nil, metadata: [:], checksum: checksumSha256)
        return try await t.decode(r)
    }

    /// Assembles the parts into the object. Without `parts`, every received part in part-number order.
    public func complete(_ id: String, parts: [(partNumber: Int, checksumSha256: String?)]? = nil, checksumSha256: String? = nil) async throws -> StorageObject {
        let partsJSON: [JSON]? = parts?.map { wire(["part_number": $0.partNumber, "checksum_sha256": $0.checksumSha256]) }
        return try await t.decode(
            Request("POST", "/api/v1/storage/uploads/\(seg(id))/complete", body: wire(["parts": partsJSON.map(JSON.array), "checksum_sha256": checksumSha256])))
    }

    /// Appends a chunk to a resumable session at `offset` (a wrong offset is a `.conflict` error with code
    /// `offset_mismatch` and `details.upload_offset`). The upload finishes by itself at the declared size.
    public func append(_ id: String, offset: Int64, chunk: Data, checksumSha256: String? = nil) async throws -> AppendResult {
        var r = Request("PATCH", "/api/v1/storage/uploads/\(seg(id))")
        r.body = .data(chunk, contentType: "application/octet-stream")
        r.headers = uploadHeaders(contentType: nil, metadata: [:], checksum: checksumSha256)
        r.headers["Upload-Offset"] = String(offset)
        let res = try await t.send(r)
        let next = res.http.value(forHTTPHeaderField: "Upload-Offset").flatMap { Int64($0) }
        if res.http.statusCode == 201 {
            let obj = try JSONDecoder().decode(StorageObject.self, from: res.data)
            return AppendResult(offset: next ?? offset + Int64(chunk.count), upload: nil, object: obj)
        }
        let up = try JSONDecoder().decode(Upload.self, from: res.data)
        return AppendResult(offset: next ?? up.receivedBytes, upload: up, object: nil)
    }

    /// Uploads `source` through a multipart session in parts of `partSize` bytes, then completes it. Aborts the
    /// session when a part fails. For a file that fits in one request use `from(bucket).put`.
    public func upload(
        bucket: String, key: String, source: UploadSource, contentType: String? = nil, metadata: [String: String] = [:],
        checksumSha256: String? = nil, partSize: Int64 = UploadsAPI.defaultPartSize,
        onProgress: (@Sendable (UploadProgress) -> Void)? = nil
    ) async throws -> StorageObject {
        let part = max(1, partSize)
        let total = try source.size()
        let session = try await create(
            bucket: bucket, key: key,
            CreateUploadOptions(kind: "multipart", contentType: contentType, metadata: metadata, checksumSha256: checksumSha256))
        do {
            var n = 0
            var from: Int64 = 0
            repeat {
                try Task.checkCancellation()
                let chunk = try source.read(offset: from, length: part)
                n += 1
                _ = try await putPart(session.id, partNumber: n, data: chunk)
                from += Int64(chunk.count)
                onProgress?(UploadProgress(uploadedBytes: from, totalBytes: total))
            } while from < total
            return try await complete(session.id)
        } catch {
            try? await abort(session.id)
            throw error
        }
    }

    /// Sends what a resumable session is still missing: reads the server's offset, then uploads `source` from there in
    /// chunks. Call it again after a failure (or a relaunch, with the saved session id) to continue where the server
    /// stopped. Returns the finished object.
    public func resume(
        _ id: String, source: UploadSource, chunkSize: Int64 = UploadsAPI.defaultPartSize,
        onProgress: (@Sendable (UploadProgress) -> Void)? = nil
    ) async throws -> StorageObject {
        let chunk = max(1, chunkSize)
        let total = try source.size()
        var offset = try await get(id).receivedBytes
        while true {
            try Task.checkCancellation()
            let data = try source.read(offset: offset, length: chunk)
            let r = try await append(id, offset: offset, chunk: data)
            offset = r.offset
            onProgress?(UploadProgress(uploadedBytes: offset, totalBytes: total))
            if let obj = r.object { return obj }
            if offset >= total || data.isEmpty {
                throw ForgeBaseError.invalidRequest("upload \(id) received all \(total) bytes but did not complete: declare the size when creating it")
            }
        }
    }
}

// MARK: - signed URLs

/// Signed URLs, used without credentials: `fb.storage.signed`.
public struct SignedAPI: Sendable {
    let t: Transport

    /// The absolute URL of a signed URL.
    public func resolve(_ url: SignedURL) -> URL? { URL(string: t.resolve(url.url)) }

    /// Downloads through a signed URL (no credentials sent).
    public func download(_ url: SignedURL, range: ByteRange? = nil) async throws -> Download {
        var r = try request("GET", url)
        r.accept = "*/*"
        if let range { r.headers["Range"] = range.headerValue }
        let res = try await t.send(r)
        return Download(
            data: res.data, statusCode: res.http.statusCode, contentType: res.http.value(forHTTPHeaderField: "Content-Type"),
            etag: res.http.value(forHTTPHeaderField: "ETag"), headers: [:])
    }

    /// Uploads through a signed `PUT` URL (no credentials sent).
    @discardableResult
    public func upload(_ url: SignedURL, _ source: UploadSource, options o: PutOptions = .init()) async throws -> StorageObject {
        var r = try request("PUT", url)
        let ct = o.contentType ?? "application/octet-stream"
        switch source {
        case .data(let d): r.body = .data(d, contentType: ct)
        case .file(let u): r.body = .file(u, contentType: ct)
        }
        r.headers = uploadHeaders(contentType: ct, metadata: o.metadata, checksum: o.checksumSha256)
        return try await t.decode(r)
    }

    /// A ready `URLRequest` for a signed URL, for your own `URLSession`: hand it to a background session's
    /// `uploadTask(with:fromFile:)` / `downloadTask(with:)` so the transfer survives the app being suspended.
    public func urlRequest(_ url: SignedURL, contentType: String? = nil, metadata: [String: String] = [:], checksumSha256: String? = nil) throws -> URLRequest {
        guard let u = resolve(url) else { throw ForgeBaseError.invalidRequest("invalid signed URL: \(url.url)") }
        var req = URLRequest(url: u)
        req.httpMethod = url.method
        if url.method == "PUT" {
            req.setValue(contentType ?? "application/octet-stream", forHTTPHeaderField: "Content-Type")
            for (k, v) in uploadHeaders(contentType: contentType, metadata: metadata, checksum: checksumSha256) { req.setValue(v, forHTTPHeaderField: k) }
        }
        return req
    }

    private func request(_ method: String, _ url: SignedURL) throws -> Request {
        guard let u = resolve(url) else { throw ForgeBaseError.invalidRequest("invalid signed URL: \(url.url)") }
        var r = Request(method, "")
        r.absoluteURL = u
        r.anonymous = true
        return r
    }
}

/// Object storage: `fb.storage`.
public struct StorageAPI: Sendable {
    let t: Transport
    public let buckets: BucketsAPI
    public let uploads: UploadsAPI
    public let signed: SignedAPI

    init(transport: Transport) {
        t = transport
        buckets = BucketsAPI(t: transport)
        uploads = UploadsAPI(t: transport)
        signed = SignedAPI(t: transport)
    }

    /// The objects of one bucket.
    public func from(_ bucket: String) -> BucketAPI { BucketAPI(t: t, bucket: bucket) }

    /// Instance totals (needs the `storage_admin` permission).
    public func stats() async throws -> JSON { try await t.json(Request("GET", "/api/v1/storage/stats")) }
}
