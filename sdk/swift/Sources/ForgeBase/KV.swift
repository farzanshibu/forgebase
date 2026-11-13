import Foundation

/// A value of the key-value engine: UTF-8 text, or bytes (sent and received as `{ "base64": ... }`).
public enum KVValue: Sendable, Equatable, Codable, ExpressibleByStringLiteral {
    case text(String)
    case bytes(Data)

    public init(stringLiteral value: String) { self = .text(value) }
    public init(_ value: String) { self = .text(value) }
    public init(_ value: Data) { self = .bytes(value) }

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let s = try? c.decode(String.self) {
            self = .text(s)
        } else {
            let o = try c.decode([String: String].self)
            guard let b64 = o["base64"], let d = Data(base64Encoded: b64) else {
                throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a KV value")
            }
            self = .bytes(d)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .text(let s): try c.encode(s)
        case .bytes(let d): try c.encode(["base64": d.base64EncodedString()])
        }
    }

    /// The bytes (text is UTF-8 encoded).
    public var data: Data {
        switch self {
        case .text(let s): return Data(s.utf8)
        case .bytes(let d): return d
        }
    }

    /// The text (bytes are decoded as UTF-8, lossy).
    public var string: String {
        switch self {
        case .text(let s): return s
        case .bytes(let d): return String(decoding: d, as: UTF8.self)
        }
    }

    fileprivate var wire: JSON {
        switch self {
        case .text(let s): return .string(s)
        case .bytes(let d): return ["base64": .string(d.base64EncodedString())]
        }
    }
}

public struct KVSetOptions: Sendable {
    /// Expire after this many milliseconds.
    public var ttlMs: Int64?
    /// Only set when the key does not exist.
    public var nx: Bool?
    /// Only set when the key exists.
    public var xx: Bool?
    /// Keep the key's current TTL.
    public var keepTTL: Bool?
    public init(ttlMs: Int64? = nil, nx: Bool? = nil, xx: Bool? = nil, keepTTL: Bool? = nil) {
        self.ttlMs = ttlMs
        self.nx = nx
        self.xx = xx
        self.keepTTL = keepTTL
    }
}

public struct KVScanPage: Sendable {
    /// Pass back as `cursor` for the next page; `"0"` when the scan is done.
    public let cursor: String
    public let keys: [String]
}

/// Type, TTL, size and value of a key.
public struct KVKey: Sendable {
    public let key: String
    /// `string`, `hash`, `list`, `set`, `zset` or `stream`.
    public let type: String
    /// Milliseconds until the key expires; `nil` when it does not expire.
    public let ttlMs: Int64?
    public let size: Int64
    /// `value` was cut to the `limit` of the call.
    public let truncated: Bool
    /// Shaped by `type` (text or `{base64}` for strings, an object for hashes, arrays for lists / sets / zsets / streams).
    public let value: JSON
}

public struct KVStreamEntry: Sendable {
    /// `<ms>-<seq>`.
    public let id: String
    public let fields: [String: KVValue]
}

public struct KVMessage: Sendable, Equatable {
    public let channel: String
    public let message: KVValue
}

public enum KVSubscriptionEvent: Sendable, Equatable {
    case message(KVMessage)
    /// The server says a slow reader missed `n` messages.
    case lagged(Int)
}

public enum KVListSide: String, Sendable { case left, right }

/// A command argument of `exec` / `batch`: numbers are sent in decimal.
public typealias KVArg = KVValue

/// Namespaces, engine statistics and generic access: `fb.kv`. Key operations live on a namespace: `fb.kv.ns("cache")`.
public struct KVAPI: Sendable {
    let t: Transport

    /// A handle on one namespace; every key operation goes through it. Cheap: create as many as you like.
    public func ns(_ name: String) -> KVNamespace { KVNamespace(t: t, name: name) }

    /// Engine statistics: memory, hit rate, evictions (administrators).
    public func info() async throws -> JSON { try await t.json(Request("GET", "/api/v1/kv/_info")) }

    /// Namespaces of the caller's tenant that hold keys.
    public func namespaces() async throws -> [JSON] {
        try await t.json(Request("GET", "/api/v1/kv/_namespaces"))["namespaces"]?.arrayValue ?? []
    }

    /// Deletes every key of a namespace; returns the number of keys removed.
    public func flush(_ namespace: String) async throws -> Int {
        try await t.json(Request("DELETE", "/api/v1/kv/\(seg(namespace))"))["deleted"]?.intValue ?? 0
    }
}

/// The key operations of one namespace.
public struct KVNamespace: Sendable {
    let t: Transport
    public let name: String

    private func path(_ rest: String = "") -> String { "/api/v1/kv/\(seg(name))\(rest)" }
    private func keyPath(_ kind: String, _ key: String, _ rest: String = "") -> String { path("/\(kind)/\(seg(key))\(rest)") }

    // MARK: keys

    /// One page of keys (`SCAN`): by key order, tolerant of concurrent changes.
    public func scan(cursor: String? = nil, match: String? = nil, count: Int? = nil, type: String? = nil) async throws -> KVScanPage {
        let j = try await t.json(Request("GET", path("/keys"), query: query(["cursor": cursor, "match": match, "count": count, "type": type])))
        return KVScanPage(cursor: j["cursor"]?.stringValue ?? "0", keys: j["keys"]?.arrayValue?.compactMap(\.stringValue) ?? [])
    }

    /// Every key matching the options, page after page.
    public func keys(match: String? = nil, count: Int? = nil, type: String? = nil) -> AsyncThrowingStream<String, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var cursor: String?
                    repeat {
                        let page = try await scan(cursor: cursor, match: match, count: count, type: type)
                        for k in page.keys { continuation.yield(k) }
                        cursor = page.cursor
                    } while cursor != nil && cursor != "0"
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Type, TTL, size and value of a key; at most `limit` items of a collection (default 1000). A missing key is `.notFound`.
    public func get(_ key: String, limit: Int? = nil) async throws -> KVKey {
        let j = try await t.json(Request("GET", path("/keys/\(seg(key))"), query: query(["limit": limit])))
        return KVKey(
            key: j["key"]?.stringValue ?? key, type: j["type"]?.stringValue ?? "", ttlMs: j["ttl_ms"]?.int64Value,
            size: j["size"]?.int64Value ?? 0, truncated: j["truncated"]?.boolValue ?? false, value: j["value"] ?? .null)
    }

    /// Deletes a key of any type; `false` when there was none.
    public func delete(_ key: String) async throws -> Bool {
        try await t.json(Request("DELETE", path("/keys/\(seg(key))")))["deleted"]?.boolValue ?? false
    }

    /// Makes the key expire in `ttlMs` milliseconds. A missing key is `.notFound`.
    public func expire(_ key: String, ttlMs: Int64) async throws {
        try await t.perform(Request("PUT", path("/keys/\(seg(key))/ttl"), body: wire(["ttl_ms": ttlMs])))
    }

    /// Removes the TTL; `false` when the key had none.
    public func persist(_ key: String) async throws -> Bool {
        try await t.json(Request("DELETE", path("/keys/\(seg(key))/ttl")))["persisted"]?.boolValue ?? false
    }

    // MARK: strings

    /// Sets a string. Returns `false` when `nx` / `xx` prevented the write.
    @discardableResult
    public func set(_ key: String, _ value: KVValue, _ o: KVSetOptions = .init()) async throws -> Bool {
        let body = wire(["value": value.wire, "ttl_ms": o.ttlMs, "nx": o.nx, "xx": o.xx, "keepttl": o.keepTTL])
        return try await t.json(Request("PUT", keyPath("strings", key), body: body))["stored"]?.boolValue ?? false
    }

    /// The string at `key`, or `nil` when there is none (or the key holds another type).
    public func string(_ key: String) async throws -> KVValue? { try await mget([key]).first ?? nil }

    /// The string at `key` as text.
    public func text(_ key: String) async throws -> String? { try await string(key)?.string }

    /// Adds `by` (default 1; negative decrements) to the integer in a string; returns the new value.
    public func incr(_ key: String, by: Int64? = nil) async throws -> Int64 {
        try await t.json(Request("POST", keyPath("strings", key, "/incr"), body: wire(["by": by])))["value"]?.int64Value ?? 0
    }

    /// Reads several strings at once, in key order; `nil` for a missing key or one of another type.
    public func mget(_ keys: [String]) async throws -> [KVValue?] {
        let j = try await t.json(Request("POST", path("/mget"), body: wire(["keys": keys])))
        return try (j["values"]?.arrayValue ?? []).map { $0.isNull ? nil : try $0.decode(KVValue.self) }
    }

    /// Writes several strings in one step; returns how many.
    public func mset(_ entries: [String: KVValue]) async throws -> Int {
        let list = entries.sorted { $0.key < $1.key }.map { JSON.object(["key": .string($0.key), "value": $0.value.wire]) }
        return try await t.json(Request("POST", path("/mset"), body: ["entries": .array(list)]))["stored"]?.intValue ?? 0
    }

    // MARK: hashes

    /// Sets hash fields; returns the number of new fields.
    public func hashSet(_ key: String, _ fields: [String: KVValue]) async throws -> Int {
        try await t.json(Request("POST", keyPath("hashes", key, "/set"), body: ["fields": .object(fields.mapValues(\.wire))]))["added"]?.intValue ?? 0
    }

    public func hashDelete(_ key: String, fields: [String]) async throws -> Int {
        try await t.json(Request("POST", keyPath("hashes", key, "/delete"), body: wire(["fields": fields])))["removed"]?.intValue ?? 0
    }

    public func hashIncr(_ key: String, field: String, by: Int64? = nil) async throws -> Int64 {
        try await t.json(Request("POST", keyPath("hashes", key, "/incr"), body: wire(["field": field, "by": by])))["value"]?.int64Value ?? 0
    }

    // MARK: lists

    /// Pushes values onto the head (`.left`) or tail (`.right`), one after the other; returns the new length.
    public func listPush(_ key: String, side: KVListSide, values: [KVValue]) async throws -> Int {
        try await t.json(Request("POST", keyPath("lists", key, "/push"), body: ["side": .string(side.rawValue), "values": .array(values.map(\.wire))]))["length"]?.intValue ?? 0
    }

    /// Pops `count` (default 1) values from one end; an empty list gives `[]`.
    public func listPop(_ key: String, side: KVListSide, count: Int? = nil) async throws -> [KVValue] {
        let j = try await t.json(Request("POST", keyPath("lists", key, "/pop"), body: wire(["side": side.rawValue, "count": count])))
        return try (j["values"] ?? .array([])).decode([KVValue].self)
    }

    public func listRemove(_ key: String, value: KVValue, count: Int? = nil) async throws -> Int {
        try await t.json(Request("POST", keyPath("lists", key, "/remove"), body: wire(["value": value.wire, "count": count])))["removed"]?.intValue ?? 0
    }

    public func listSet(_ key: String, index: Int, value: KVValue) async throws {
        try await t.perform(Request("POST", keyPath("lists", key, "/set"), body: wire(["index": index, "value": value.wire])))
    }

    // MARK: sets and sorted sets

    public func setAdd(_ key: String, members: [KVValue]) async throws -> Int {
        try await t.json(Request("POST", keyPath("sets", key, "/add"), body: ["members": .array(members.map(\.wire))]))["added"]?.intValue ?? 0
    }

    public func setRemove(_ key: String, members: [KVValue]) async throws -> Int {
        try await t.json(Request("POST", keyPath("sets", key, "/remove"), body: ["members": .array(members.map(\.wire))]))["removed"]?.intValue ?? 0
    }

    /// Adds members or re-scores them (scores must be finite; use `exec` for infinities).
    public func zsetAdd(_ key: String, members: [(member: KVValue, score: Double)]) async throws -> Int {
        let list = members.map { JSON.object(["member": $0.member.wire, "score": .double($0.score)]) }
        return try await t.json(Request("POST", keyPath("zsets", key, "/add"), body: ["members": .array(list)]))["added"]?.intValue ?? 0
    }

    public func zsetRemove(_ key: String, members: [KVValue]) async throws -> Int {
        try await t.json(Request("POST", keyPath("zsets", key, "/remove"), body: ["members": .array(members.map(\.wire))]))["removed"]?.intValue ?? 0
    }

    // MARK: streams

    /// Appends an entry; returns its id.
    public func streamAdd(_ key: String, fields: [String: KVValue], id: String? = nil, maxLen: Int? = nil) async throws -> String {
        var body: [String: JSON] = ["fields": .object(fields.mapValues(\.wire))]
        if let id { body["id"] = .string(id) }
        if let maxLen { body["maxlen"] = .int(Int64(maxLen)) }
        return try await t.json(Request("POST", keyPath("streams", key, "/add"), body: .object(body)))["id"]?.stringValue ?? ""
    }

    public func streamDelete(_ key: String, ids: [String]) async throws -> Int {
        try await t.json(Request("POST", keyPath("streams", key, "/delete"), body: wire(["ids": ids])))["removed"]?.intValue ?? 0
    }

    /// Entries between two ids (`-` and `+` are the ends).
    public func streamRange(_ key: String, start: String? = nil, end: String? = nil, count: Int? = nil, reverse: Bool? = nil) async throws -> [KVStreamEntry] {
        let j = try await t.json(Request("GET", keyPath("streams", key, "/range"), query: query(["start": start, "end": end, "count": count, "rev": reverse])))
        return try Self.entries(j)
    }

    /// Entries after an id, without waiting for new ones: poll with the last id you saw, or use pub/sub to learn of new entries.
    public func streamRead(_ key: String, after: String? = nil, count: Int? = nil) async throws -> [KVStreamEntry] {
        let j = try await t.json(Request("GET", keyPath("streams", key, "/read"), query: query(["after": after, "count": count])))
        return try Self.entries(j)
    }

    private static func entries(_ j: JSON) throws -> [KVStreamEntry] {
        try (j["entries"]?.arrayValue ?? []).map { e in
            KVStreamEntry(id: e["id"]?.stringValue ?? "", fields: try (e["fields"]?.objectValue ?? [:]).mapValues { try $0.decode(KVValue.self) })
        }
    }

    // MARK: generic commands

    /// Runs one command given as an array: `exec(["ZRANGE", "board", "0", "-1", "WITHSCORES"])`.
    public func exec(_ command: [KVArg]) async throws -> JSON {
        try await t.json(Request("POST", path("/exec"), body: ["command": .array(command.map(\.wire))]))["result"] ?? .null
    }

    /// Runs 1 to 1000 commands in order with no other command in between. A failing command reports `{error}` and the
    /// rest still run; earlier ones are not undone.
    public func batch(_ commands: [[KVArg]]) async throws -> [JSON] {
        let body: JSON = ["commands": .array(commands.map { .array($0.map(\.wire)) })]
        return try await t.json(Request("POST", path("/batch"), body: body))["results"]?.arrayValue ?? []
    }

    // MARK: pub/sub

    /// Publishes to the subscribers connected now (messages are not stored); returns how many received it.
    public func publish(_ channel: String, _ message: KVValue) async throws -> Int {
        try await t.json(Request("POST", path("/publish/\(seg(channel))"), body: ["message": message.wire]))["receivers"]?.intValue ?? 0
    }

    /// Live channels of the namespace (those with at least one subscriber).
    public func channels() async throws -> [JSON] {
        try await t.json(Request("GET", path("/channels")))["channels"]?.arrayValue ?? []
    }

    /// Subscribes to a channel (server-sent events): every message published after the stream opens. A dropped stream
    /// reconnects with backoff (messages published meanwhile are lost: the engine does not store them); a refusal
    /// (missing permission) ends the stream with the error. Cancel the consuming task to unsubscribe.
    public func subscribe(_ channel: String, reconnect: Bool = true) -> AsyncThrowingStream<KVSubscriptionEvent, Error> {
        let events = t.eventStream({ Request("GET", self.path("/subscribe/\(seg(channel))")) }, reconnect: reconnect)
        return AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    for try await ev in events {
                        if ev.event == "lagged" {
                            continuation.yield(.lagged(Int(ev.data) ?? 0))
                        } else if ev.event == "message", !ev.data.isEmpty, let j = try? JSON(data: Data(ev.data.utf8)),
                            let m = try? j["message"]?.decode(KVValue.self)
                        {
                            continuation.yield(.message(KVMessage(channel: j["channel"]?.stringValue ?? channel, message: m)))
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
