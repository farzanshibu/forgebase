import XCTest
@testable import ForgeBase

private struct Post: Codable, Sendable, Equatable {
    var id: String?
    var title: String
    var views: Int?
    var _version: Int?
}

final class RecordsTests: XCTestCase {
    private func page(_ items: [JSON], page: Int? = nil, totalPages: Int? = nil, next: String? = nil) -> JSON {
        var o: [String: JSON] = ["items": .array(items), "per_page": 2]
        if let page { o["page"] = .int(Int64(page)) }
        if let totalPages { o["total_pages"] = .int(Int64(totalPages)) }
        if let next { o["next_cursor"] = .string(next) }
        return .object(o)
    }

    func testListSendsFilterSortAndPagingAsAQuery() async throws {
        let env = TestEnv()
        env.server.script([.json(page([["id": "a", "title": "A"]], page: 1, totalPages: 1))])
        let fb = env.client(apiKey: "k")
        let res = try await fb.collection("posts").list(
            ListOptions(filter: #"views > 3 && status = "live""#, sort: ["-created", "title"], expand: ["author"], fields: ["id", "title"], search: "swift", page: 2, perPage: 20, skipTotal: true))
        XCTAssertEqual(res.items.count, 1)
        XCTAssertEqual(res.items[0]["title"], "A")
        XCTAssertEqual(res.perPage, 2)
        let r = env.server.requests[0]
        XCTAssertEqual(r.method, "GET")
        XCTAssertEqual(r.path, "/api/v1/collections/posts/records")
        let items = URLComponents(url: r.url, resolvingAgainstBaseURL: false)!.queryItems!
        func v(_ n: String) -> String? { items.first { $0.name == n }?.value }
        XCTAssertEqual(v("filter"), #"views > 3 && status = "live""#)
        XCTAssertEqual(v("sort"), "-created,title")
        XCTAssertEqual(v("expand"), "author")
        XCTAssertEqual(v("fields"), "id,title")
        XCTAssertEqual(v("per_page"), "20")
        XCTAssertEqual(v("skip_total"), "true")
        XCTAssertEqual(v("search"), "swift")
    }

    func testListWithParamsUsesTheTypedQueryEndpoint() async throws {
        let env = TestEnv()
        env.server.script([.json(page([]))])
        let fb = env.client(apiKey: "k")
        _ = try await fb.collection("posts").list(ListOptions(filter: "price > {:min}", params: ["min": 10], sort: ["price"], perPage: 5))
        let r = env.server.requests[0]
        XCTAssertEqual(r.method, "POST")
        XCTAssertEqual(r.path, "/api/v1/collections/posts/records/query")
        XCTAssertEqual(r.json?["params"]?["min"], 10)
        XCTAssertEqual(r.json?["filter"], "price > {:min}")
        XCTAssertEqual(r.json?["sort"], "price")
        XCTAssertEqual(r.json?["per_page"], 5)
    }

    func testTypedRecordsDecodeIntoCodableTypes() async throws {
        let env = TestEnv()
        env.server.script([.json(page([["id": "a", "title": "A", "views": 7, "_version": 3, "created": "x"]]))])
        let fb = env.client(apiKey: "k")
        let res = try await fb.collection("posts", as: Post.self).list()
        XCTAssertEqual(res.items, [Post(id: "a", title: "A", views: 7, _version: 3)])
    }

    func testPagesFollowCursorsThenStop() async throws {
        let env = TestEnv()
        env.server.script([
            .json(page([["id": "1"], ["id": "2"]], next: "c1")),
            .json(page([["id": "3"], ["id": "4"]], next: "c2")),
            .json(page([["id": "5"]])),
        ])
        let fb = env.client(apiKey: "k")
        let all = try await fb.collection("posts").all(ListOptions(perPage: 2))
        XCTAssertEqual(all.compactMap { $0["id"]?.stringValue }, ["1", "2", "3", "4", "5"])
        let cursors = env.server.requests.map { URLComponents(url: $0.url, resolvingAgainstBaseURL: false)!.queryItems?.first { $0.name == "cursor" }?.value }
        XCTAssertEqual(cursors, [nil, "c1", "c2"])
    }

    func testPagesFallBackToPageNumbers() async throws {
        let env = TestEnv()
        env.server.script([
            .json(page([["id": "1"]], page: 1, totalPages: 2)),
            .json(page([["id": "2"]], page: 2, totalPages: 2)),
        ])
        let fb = env.client(apiKey: "k")
        var ids: [String] = []
        for try await p in fb.collection("posts").pages() { ids += p.items.compactMap { $0["id"]?.stringValue } }
        XCTAssertEqual(ids, ["1", "2"])
        XCTAssertTrue(env.server.requests[1].query.contains("page=2"))
    }

    func testFirstAndCount() async throws {
        let env = TestEnv()
        env.server.script([.json(page([["id": "z"]])), .json(["count": 42])])
        let fb = env.client(apiKey: "k")
        let first = try await fb.collection("posts").first(filter: "slug = {:s}", params: ["s": "hi"])
        XCTAssertEqual(first?["id"], "z")
        XCTAssertEqual(env.server.requests[0].json?["per_page"], 1)
        let n = try await fb.collection("posts").count(filter: "views > 1")
        XCTAssertEqual(n, 42)
    }

    func testCRUDAndOptimisticVersions() async throws {
        let env = TestEnv()
        env.server.script([.json(["id": "n1", "title": "New"], status: 201), .json(["id": "n1", "title": "Edited", "_version": 4]), .json(["id": "n1", "title": "R"]), .empty()])
        let fb = env.client(apiKey: "k")
        let posts = fb.collection("posts", as: Post.self)
        let created = try await posts.create(Post(id: nil, title: "New", views: nil, _version: nil))
        XCTAssertEqual(created.id, "n1")
        XCTAssertEqual(env.server.requests[0].json?["title"], "New")
        XCTAssertNil(env.server.requests[0].json?["id"], "nil optionals are not sent")

        let updated = try await posts.update("n1", ["title": "Edited"] as JSONObject, version: 3)
        XCTAssertEqual(updated._version, 4)
        XCTAssertEqual(env.server.requests[1].method, "PATCH")
        XCTAssertEqual(env.server.requests[1].header("If-Match"), "\"3\"")

        _ = try await posts.replace("n1", Post(id: "n1", title: "R", views: 1, _version: nil))
        XCTAssertEqual(env.server.requests[2].method, "PUT")
        XCTAssertNil(env.server.requests[2].header("If-Match"))

        try await posts.delete("n1", version: 4)
        XCTAssertEqual(env.server.requests[3].method, "DELETE")
        XCTAssertEqual(env.server.requests[3].header("If-Match"), "\"4\"")
    }

    func testStaleVersionIsAConflict() async {
        let env = TestEnv()
        env.server.script([.apiError(412, code: "version_mismatch", message: "stale")])
        let fb = env.client(apiKey: "k")
        await XCTAssertThrowsForgeBase(try await fb.collection("posts").update("p", ["a": 1] as JSONObject, version: 1), kind: .conflict, status: 412, code: "version_mismatch")
    }

    func testIDsAreEncodedInThePath() async throws {
        let env = TestEnv()
        env.server.script([.json(["id": "a/b c"])])
        let fb = env.client(apiKey: "k")
        _ = try await fb.collection("my posts").get("a/b c", expand: ["x", "y"], consistency: .strong)
        let r = env.server.requests[0]
        XCTAssertEqual(r.url.absoluteString, "https://fb.test/api/v1/collections/my%20posts/records/a%2Fb%20c?expand=x%2Cy")
        XCTAssertEqual(r.header(consistencyHeader), "strong")
    }

    func testExportAndImport() async throws {
        let env = TestEnv()
        env.server.script([.text("id,title\n1,a\n"), .json(["imported": 1])])
        let fb = env.client(apiKey: "k")
        let csv = try await fb.collection("posts").export(format: .csv, filter: "views > 0")
        XCTAssertEqual(csv, "id,title\n1,a\n")
        _ = try await fb.collection("posts").import(format: .ndjson, body: "{\"title\":\"a\"}\n")
        XCTAssertEqual(env.server.requests[1].path, "/api/v1/collections/posts/import")
        XCTAssertEqual(String(decoding: env.server.requests[1].body, as: UTF8.self), "{\"title\":\"a\"}\n")
        XCTAssertEqual(env.server.requests[1].header("Content-Type"), "application/octet-stream")
    }

    func testCollectionDefinitions() async throws {
        let env = TestEnv()
        env.server.script([.json(["items": [["name": "posts"]]]), .json(["name": "posts"]), .empty()])
        let fb = env.client(apiKey: "k")
        let list = try await fb.collections.list()
        XCTAssertEqual(list.count, 1)
        _ = try await fb.collections.get("posts")
        try await fb.collections.delete("posts")
        XCTAssertEqual(env.server.requests[2].method, "DELETE")
    }
}
