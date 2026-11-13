import XCTest
@testable import ForgeBase

private func objectJSON(_ key: String, size: Int = 3, bucket: String = "avatars") -> JSON {
    [
        "bucket": .string(bucket), "key": .string(key), "version_id": "v1", "size": .int(Int64(size)), "content_type": "image/png",
        "metadata": ["owner": "ada"], "checksum_sha256": nil, "is_latest": true, "delete_marker": false, "created_by": "u1", "created_at": "2026-01-01T00:00:00Z",
    ]
}

private func uploadJSON(id: String = "up_1", kind: String = "multipart", received: Int = 0, size: Int? = nil) -> JSON {
    var o: [String: JSON] = [
        "id": .string(id), "bucket": "docs", "key": "big.bin", "kind": .string(kind), "content_type": "application/octet-stream",
        "received_bytes": .int(Int64(received)), "parts": [], "expires_at": "2026-01-02T00:00:00Z",
    ]
    if let size { o["declared_size"] = .int(Int64(size)) }
    return .object(o)
}

final class StorageTests: XCTestCase {
    var tmp: URL!

    override func setUpWithError() throws {
        tmp = FileManager.default.temporaryDirectory.appendingPathComponent("fb-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: tmp) }

    func testBucketsCRUD() async throws {
        let env = TestEnv()
        let bucket: JSON = ["name": "avatars", "visibility": "private", "versioning": false, "owner": "u1", "created_at": "x", "updated_at": "y"]
        env.server.script([.json(["items": [bucket]]), .json(bucket, status: 201), .json(bucket), .empty(), .json(["entries": [["grantee": "u2", "permission": "read"]]]), .json(["bucket": "avatars", "objects": 2, "versions": 3, "logical_bytes": 10, "version_bytes": 4])])
        let fb = env.client(apiKey: "k")
        let list = try await fb.storage.buckets.list()
        XCTAssertEqual(list.first?.name, "avatars")
        _ = try await fb.storage.buckets.create(CreateBucketInput(name: "avatars", visibility: "private", allowedContentTypes: ["image/*"], maxObjectBytes: 1000))
        XCTAssertEqual(env.server.requests[1].json?["allowed_content_types"], ["image/*"])
        XCTAssertEqual(env.server.requests[1].json?["max_object_bytes"], 1000)
        _ = try await fb.storage.buckets.update("avatars", UpdateBucketInput(versioning: true))
        XCTAssertEqual(env.server.requests[2].method, "PATCH")
        try await fb.storage.buckets.delete("avatars", force: true)
        XCTAssertEqual(env.server.requests[3].query, "force=true")
        let acl = try await fb.storage.buckets.acl("avatars")
        XCTAssertEqual(acl, [AclEntry(grantee: "u2", permission: "read")])
        let usage = try await fb.storage.buckets.usage("avatars")
        XCTAssertEqual(usage.logicalBytes, 10)
    }

    func testPutSendsBodyContentTypeMetadataAndChecksum() async throws {
        let env = TestEnv()
        env.server.script([.json(objectJSON("u1/me me.png"), status: 201)])
        let fb = env.client(apiKey: "k")
        let bytes = Data([1, 2, 3])
        let obj = try await fb.storage.from("avatars").put(
            "u1/me me.png", data: bytes, options: PutOptions(contentType: "image/png", metadata: ["owner": "ada"], checksumSha256: sha256Hex(bytes)))
        XCTAssertEqual(obj.metadata, ["owner": "ada"])
        XCTAssertEqual(obj.size, 3)
        let r = env.server.requests[0]
        XCTAssertEqual(r.method, "PUT")
        XCTAssertEqual(r.url.path, "/api/v1/storage/buckets/avatars/objects/u1/me me.png")
        XCTAssertTrue(r.url.absoluteString.hasSuffix("/objects/u1/me%20me.png"), "slashes stay, segments are encoded")
        XCTAssertEqual(r.body, bytes)
        XCTAssertEqual(r.header("Content-Type"), "image/png")
        XCTAssertEqual(r.header("X-Forgebase-Meta-owner"), "ada")
        XCTAssertEqual(r.header("X-Forgebase-Checksum-Sha256"), "039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81")
    }

    func testPutFromAFileStreamsIt() async throws {
        let env = TestEnv()
        env.server.script([.json(objectJSON("clip.mov", size: 5), status: 201)])
        let fb = env.client(apiKey: "k")
        let file = tmp.appendingPathComponent("clip.mov")
        try Data("hello".utf8).write(to: file)
        _ = try await fb.storage.from("avatars").put("clip.mov", fileURL: file, options: PutOptions(contentType: "video/quicktime"))
        XCTAssertEqual(String(decoding: env.server.requests[0].body, as: UTF8.self), "hello")
        XCTAssertEqual(env.server.requests[0].header("Content-Type"), "video/quicktime")
        XCTAssertEqual(try sha256Hex(fileAt: file), sha256Hex(Data("hello".utf8)))
    }

    func testDownloadWithRangeAndHeaders() async throws {
        let env = TestEnv()
        env.server.script([MockResponse(status: 206, headers: ["Content-Type": "image/png", "ETag": "\"e1\"", "Content-Range": "bytes 0-1/3"], body: Data([1, 2]))])
        let fb = env.client(apiKey: "k")
        let d = try await fb.storage.from("avatars").download("a.png", DownloadOptions(version: "v2", downloadAs: "me.png", range: .bytes(start: 0, end: 1)))
        XCTAssertEqual(d.statusCode, 206)
        XCTAssertEqual(d.data, Data([1, 2]))
        XCTAssertEqual(d.contentType, "image/png")
        XCTAssertEqual(d.etag, "\"e1\"")
        XCTAssertEqual(d.headers["content-range"], "bytes 0-1/3")
        let r = env.server.requests[0]
        XCTAssertEqual(r.header("Range"), "bytes=0-1")
        XCTAssertEqual(r.query, "version=v2&download=me.png")
        XCTAssertEqual(ByteRange.suffix(10).headerValue, "bytes=-10")
        XCTAssertEqual(ByteRange.bytes(start: 5, end: nil).headerValue, "bytes=5-")
    }

    func testDownloadToAFile() async throws {
        let env = TestEnv()
        env.server.script([MockResponse(headers: ["Content-Type": "application/octet-stream"], body: Data("file body".utf8))])
        let fb = env.client(apiKey: "k")
        let dest = tmp.appendingPathComponent("out.bin")
        try Data("old".utf8).write(to: dest)
        try await fb.storage.from("docs").download("a.bin", to: dest)
        XCTAssertEqual(try String(contentsOf: dest), "file body")
    }

    func testFailedDownloadToAFileThrowsTheServerError() async {
        let env = TestEnv()
        env.server.script([.apiError(404, code: "object_not_found")])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.storage.from("docs").download("nope", to: tmp.appendingPathComponent("x")), kind: .notFound, code: "object_not_found")
        XCTAssertFalse(FileManager.default.fileExists(atPath: tmp.appendingPathComponent("x").path))
    }

    func testTextJsonMetaDeleteCopyMoveVersions() async throws {
        let env = TestEnv()
        env.server.script([
            MockResponse(headers: [:], body: Data("hi".utf8)), MockResponse(headers: [:], body: Data(#"{"a":1}"#.utf8)), .json(objectJSON("a")),
            .json(["version_id": "v9", "delete_marker": true]), .json(objectJSON("b")), .json(objectJSON("c")),
            .json(["items": [objectJSON("a"), objectJSON("a")]]), .json(objectJSON("a")),
        ])
        let fb = env.client(apiKey: "k")
        let b = fb.storage.from("docs")
        let text = try await b.text("a.txt")
        XCTAssertEqual(text, "hi")
        struct A: Decodable { let a: Int }
        let a = try await b.json("a.json", as: A.self)
        XCTAssertEqual(a.a, 1)
        _ = try await b.meta("a", version: "v1")
        XCTAssertEqual(env.server.requests[2].path, "/api/v1/storage/buckets/docs/meta/a")
        let del = try await b.delete("a")
        XCTAssertTrue(del.deleteMarker)
        _ = try await b.copy("a", to: "b", destBucket: "other")
        XCTAssertEqual(env.server.requests[4].json?["dest_bucket"], "other")
        _ = try await b.move("b", to: "c")
        XCTAssertEqual(env.server.requests[5].path, "/api/v1/storage/buckets/docs/move")
        let versions = try await b.versions("a")
        XCTAssertEqual(versions.count, 2)
        _ = try await b.restore("a", versionId: "v1")
        XCTAssertEqual(env.server.requests[7].json?["version_id"], "v1")
    }

    func testListAndIterateFollowTheCursor() async throws {
        let env = TestEnv()
        env.server.script([
            .json(["objects": [objectJSON("a"), objectJSON("b")], "prefixes": ["x/"], "next_cursor": "c1"]),
            .json(["objects": [objectJSON("c")], "prefixes": []]),
        ])
        let fb = env.client(apiKey: "k")
        var keys: [String] = []
        for try await o in fb.storage.from("docs").iterate(prefix: "p/", delimiter: "/", limit: 2) { keys.append(o.key) }
        XCTAssertEqual(keys, ["a", "b", "c"])
        XCTAssertEqual(env.server.requests[0].query, "prefix=p%2F&delimiter=%2F&limit=2")
        XCTAssertEqual(env.server.requests[1].query, "prefix=p%2F&delimiter=%2F&limit=2&cursor=c1")
    }

    func testSignedURLs() async throws {
        let env = TestEnv()
        env.server.script([
            .json(["url": "/api/v1/storage/signed/get?sig=abc", "method": "GET", "expires_at": "2026-01-01T00:00:00Z"]),
            .json(["url": "/api/v1/storage/signed/put?sig=def", "method": "PUT", "expires_at": "2026-01-01T00:00:00Z"]),
            MockResponse(headers: ["Content-Type": "image/png"], body: Data([9, 9])),
            .json(objectJSON("up.png"), status: 201),
        ])
        let fb = env.client(apiKey: "k")
        let get = try await fb.storage.from("avatars").createSignedURL("a.png", expiresIn: 60, downloadAs: "me.png")
        XCTAssertEqual(env.server.requests[0].json?["method"], "GET")
        XCTAssertEqual(env.server.requests[0].json?["expires_in"], 60)
        let put = try await fb.storage.from("avatars").createSignedUploadURL("up.png", contentType: "image/png")
        XCTAssertEqual(env.server.requests[1].json?["method"], "PUT")
        XCTAssertEqual(fb.storage.signed.resolve(get)?.absoluteString, "https://fb.test/api/v1/storage/signed/get?sig=abc")

        let d = try await fb.storage.signed.download(get)
        XCTAssertEqual(d.data, Data([9, 9]))
        XCTAssertNil(env.server.requests[2].header("X-API-Key"), "signed URLs carry no credentials")
        _ = try await fb.storage.signed.upload(put, .data(Data([1])), options: PutOptions(contentType: "image/png"))
        XCTAssertEqual(env.server.requests[3].method, "PUT")
        XCTAssertNil(env.server.requests[3].header("X-API-Key"))

        // A request for a background URLSession.
        let req = try fb.storage.signed.urlRequest(put, contentType: "image/png", metadata: ["k": "v"])
        XCTAssertEqual(req.httpMethod, "PUT")
        XCTAssertEqual(req.url?.absoluteString, "https://fb.test/api/v1/storage/signed/put?sig=def")
        XCTAssertEqual(req.value(forHTTPHeaderField: "X-Forgebase-Meta-k"), "v")
        XCTAssertEqual(req.value(forHTTPHeaderField: "Content-Type"), "image/png")
    }

    // MARK: multipart

    func testMultipartUploadSendsPartsInOrderThenCompletes() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/v1/storage/buckets/docs/uploads": { _ in .json(uploadJSON(), status: 201) },
            "PUT /api/v1/storage/uploads/up_1/parts/1": { _ in .json(["part_number": 1, "size": 4, "checksum_sha256": "a"]) },
            "PUT /api/v1/storage/uploads/up_1/parts/2": { _ in .json(["part_number": 2, "size": 4, "checksum_sha256": "b"]) },
            "PUT /api/v1/storage/uploads/up_1/parts/3": { _ in .json(["part_number": 3, "size": 2, "checksum_sha256": "c"]) },
            "POST /api/v1/storage/uploads/up_1/complete": { _ in .json(objectJSON("big.bin", size: 10, bucket: "docs")) },
        ])
        let fb = env.client(apiKey: "k")
        let progress = Locked<[Int64]>([])
        let obj = try await fb.storage.uploads.upload(
            bucket: "docs", key: "big.bin", source: .data(Data("0123456789".utf8)), contentType: "application/x-bin", metadata: ["m": "1"],
            partSize: 4, onProgress: { p in progress.withLock { $0.append(p.uploadedBytes) } })
        XCTAssertEqual(obj.size, 10)
        let parts = env.server.requests.filter { $0.path.contains("/parts/") }
        XCTAssertEqual(parts.map { String(decoding: $0.body, as: UTF8.self) }, ["0123", "4567", "89"])
        XCTAssertEqual(progress.withLock { $0 }, [4, 8, 10])
        XCTAssertEqual(env.server.requests[0].json?["kind"], "multipart")
        XCTAssertEqual(env.server.requests[0].json?["content_type"], "application/x-bin")
        XCTAssertEqual(env.server.requests[0].json?["metadata"]?["m"], "1")
    }

    func testMultipartUploadFromAFileAbortsWhenAPartFails() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/v1/storage/buckets/docs/uploads": { _ in .json(uploadJSON(), status: 201) },
            "PUT /api/v1/storage/uploads/up_1/parts/1": { _ in .json(["part_number": 1, "size": 4, "checksum_sha256": "a"]) },
            "PUT /api/v1/storage/uploads/up_1/parts/2": { _ in .apiError(422, code: "checksum_mismatch") },
            "DELETE /api/v1/storage/uploads/up_1": { _ in .empty() },
        ])
        let fb = env.client(apiKey: "k")
        let file = tmp.appendingPathComponent("f.bin")
        try Data("0123456789".utf8).write(to: file)
        await XCTAssertThrowsForgeBase(
            try await fb.storage.uploads.upload(bucket: "docs", key: "big.bin", source: .file(file), partSize: 4), kind: .validation, status: 422, code: "checksum_mismatch")
        XCTAssertEqual(env.server.requests.last?.method, "DELETE")
        XCTAssertEqual(env.server.requests.last?.path, "/api/v1/storage/uploads/up_1")
    }

    func testEmptyFileStillUploadsOnePart() async throws {
        let env = TestEnv()
        env.server.routes([
            "POST /api/v1/storage/buckets/docs/uploads": { _ in .json(uploadJSON(), status: 201) },
            "PUT /api/v1/storage/uploads/up_1/parts/1": { _ in .json(["part_number": 1, "size": 0, "checksum_sha256": "a"]) },
            "POST /api/v1/storage/uploads/up_1/complete": { _ in .json(objectJSON("big.bin", size: 0, bucket: "docs")) },
        ])
        let fb = env.client(apiKey: "k")
        let obj = try await fb.storage.uploads.upload(bucket: "docs", key: "big.bin", source: .data(Data()))
        XCTAssertEqual(obj.size, 0)
    }

    func testCompleteWithExplicitParts() async throws {
        let env = TestEnv()
        env.server.script([.json(objectJSON("a"))])
        let fb = env.client(apiKey: "k")
        _ = try await fb.storage.uploads.complete("up_1", parts: [(1, "aa"), (2, nil)], checksumSha256: "whole")
        let body = env.server.requests[0].json
        XCTAssertEqual(body?["parts"]?[0]?["part_number"], 1)
        XCTAssertEqual(body?["parts"]?[0]?["checksum_sha256"], "aa")
        XCTAssertEqual(body?["checksum_sha256"], "whole")
    }

    // MARK: resumable

    func testResumeContinuesFromTheServersOffset() async throws {
        let env = TestEnv()
        let appended = Locked<[(Int64, String)]>([])
        env.server.routes([
            "GET /api/v1/storage/uploads/up_2": { _ in .json(uploadJSON(id: "up_2", kind: "resumable", received: 4, size: 10)) },
            "PATCH /api/v1/storage/uploads/up_2": { r in
                let offset = Int64(r.header("Upload-Offset") ?? "") ?? -1
                appended.withLock { $0.append((offset, String(decoding: r.body, as: UTF8.self))) }
                if offset == 8 {
                    return .json(objectJSON("big.bin", size: 10, bucket: "docs"), status: 201, headers: ["Upload-Offset": "10"])
                }
                return .json(uploadJSON(id: "up_2", kind: "resumable", received: Int(offset) + 4, size: 10), headers: ["Upload-Offset": String(offset + 4)])
            },
        ])
        let fb = env.client(apiKey: "k")
        let obj = try await fb.storage.uploads.resume("up_2", source: .data(Data("0123456789".utf8)), chunkSize: 4)
        XCTAssertEqual(obj.size, 10)
        let calls = appended.withLock { $0 }
        XCTAssertEqual(calls.map(\.0), [4, 8])
        XCTAssertEqual(calls.map(\.1), ["4567", "89"])
    }

    func testOffsetMismatchCarriesTheServersOffset() async {
        let env = TestEnv()
        env.server.script([.apiError(409, code: "offset_mismatch", details: ["upload_offset": 6])])
        let fb = env.client(apiKey: "k")
        do {
            _ = try await fb.storage.uploads.append("up_2", offset: 2, chunk: Data([1]))
            XCTFail("expected a conflict")
        } catch let e as ForgeBaseError {
            XCTAssertEqual(e.kind, .conflict)
            XCTAssertEqual(e.details?["upload_offset"]?.intValue, 6)
        } catch { XCTFail("\(error)") }
    }

    func testResumeThatNeverCompletesExplainsWhy() async {
        let env = TestEnv()
        env.server.routes([
            "GET /api/v1/storage/uploads/up_3": { _ in .json(uploadJSON(id: "up_3", kind: "resumable", received: 0)) },
            "PATCH /api/v1/storage/uploads/up_3": { _ in .json(uploadJSON(id: "up_3", kind: "resumable", received: 3), headers: ["Upload-Offset": "3"]) },
        ])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.storage.uploads.resume("up_3", source: .data(Data("abc".utf8))), kind: .invalidRequest)
    }

    func testUploadSessionHelpers() async throws {
        let env = TestEnv()
        env.server.script([.json(uploadJSON(kind: "resumable", size: 10), status: 201), .json(uploadJSON(received: 4)), .empty()])
        let fb = env.client(apiKey: "k")
        let up = try await fb.storage.uploads.create(bucket: "docs", key: "k", CreateUploadOptions(kind: "resumable", size: 10, checksumSha256: "x"))
        XCTAssertEqual(up.declaredSize, 10)
        XCTAssertEqual(env.server.requests[0].json?["size"], 10)
        let got = try await fb.storage.uploads.get("up_1")
        XCTAssertEqual(got.receivedBytes, 4)
        try await fb.storage.uploads.abort("up_1")
        XCTAssertEqual(env.server.requests[2].method, "DELETE")
    }

    func testSHA256Hex() {
        XCTAssertEqual(sha256Hex(Data("abc".utf8)), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    }
}
