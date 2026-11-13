import Foundation

// MARK: - events

public enum RecordEventKind: String, Sendable {
    case created = "record.created"
    case updated = "record.updated"
    case deleted = "record.deleted"
}

/// A record changed.
public struct RecordEvent: Sendable {
    public let kind: RecordEventKind
    /// The subscription that delivered it.
    public let subscriptionId: String
    /// Resume cursor: the SDK stores the highest one per subscription.
    public let cursor: Int64
    public let collection: String
    public let recordId: String
    public let tenantId: String?
    public let timestamp: String
    /// The record (for a delete: its last state).
    public let data: JSON
    /// The primary's commit position when the event was read; fed to the client's commit tokens automatically.
    public let commit: String?

    /// The record decoded into your type.
    public func record<T: Decodable>(as type: T.Type = T.self) throws -> T { try data.decode(T.self) }
}

/// A message published to a channel.
public struct ChannelMessage: Sendable {
    public let subscriptionId: String
    public let channel: String
    public let from: String
    public let data: JSON
    public let timestamp: String

    public func payload<T: Decodable>(as type: T.Type = T.self) throws -> T { try data.decode(T.self) }
}

public struct PresenceMember: Sendable, Decodable, Equatable {
    public let connectionId: String
    public let actor: String
    public let meta: JSON?
    public let joinedAt: String
    public let instance: String?

    enum CodingKeys: String, CodingKey {
        case connectionId = "connection_id", actor, meta, joinedAt = "joined_at", instance
    }
}

public struct PresenceEvent: Sendable {
    public enum Change: Sendable { case join, leave }
    public let change: Change
    public let subscriptionId: String
    public let channel: String
    public let member: PresenceMember
}

/// The server could not replay from the cursor: reload your state, then continue from `cursor`.
public struct ResetEvent: Sendable {
    /// `cursor_expired`, `cursor_invalid`, `too_far_behind`, ...
    public let reason: String
    public let subscriptionId: String
    public let cursor: Int64
}

public enum RealtimeEvent: Sendable {
    case record(RecordEvent)
    case message(ChannelMessage)
    case presence(PresenceEvent)
    case reset(ResetEvent)
}

/// What to subscribe to.
public struct SubscribeOptions: Sendable {
    /// Client-chosen id (1-64 of `[A-Za-z0-9_.:-]`); generated when omitted.
    public var id: String?
    /// Collection to watch (add `record` for one record, or `filter`).
    public var collection: String?
    public var record: String?
    public var filter: String?
    public var params: JSONObject?
    /// Custom pub/sub channel (instead of `collection`).
    public var channel: String?
    /// Replay every event after this cursor, then go live. Without it the stream starts at the head.
    public var cursor: Int64?

    public init(
        id: String? = nil, collection: String? = nil, record: String? = nil, filter: String? = nil,
        params: JSONObject? = nil, channel: String? = nil, cursor: Int64? = nil
    ) {
        self.id = id
        self.collection = collection
        self.record = record
        self.filter = filter
        self.params = params
        self.channel = channel
        self.cursor = cursor
    }

    public static func collection(_ name: String, filter: String? = nil, params: JSONObject? = nil, cursor: Int64? = nil) -> SubscribeOptions {
        SubscribeOptions(collection: name, filter: filter, params: params, cursor: cursor)
    }

    public static func record(collection: String, id: String, cursor: Int64? = nil) -> SubscribeOptions {
        SubscribeOptions(collection: collection, record: id, cursor: cursor)
    }

    public static func channel(_ name: String) -> SubscribeOptions { SubscribeOptions(channel: name) }
}

public enum RealtimeState: String, Sendable {
    case idle, connecting, open, reconnecting, closed
}

/// Connection life cycle, for UI ("reconnecting...").
public enum RealtimeConnectionEvent: Sendable {
    case open
    case close(code: Int, reason: String)
    case reconnecting(attempt: Int, delay: TimeInterval)
    case error(String)
}

/// A live subscription. Iterate `events`; leaving the loop (or cancelling its task) unsubscribes.
public struct RealtimeSubscription: Sendable {
    public let id: String
    public let events: AsyncStream<RealtimeEvent>
    let client: RealtimeClient

    /// Highest cursor delivered so far (what a reconnect resumes from).
    public func cursor() async -> Int64? { await client.cursor(of: id) }
    public func unsubscribe() async { await client.unsubscribe(id) }
}

// MARK: - client

/// Realtime client: one WebSocket carries every subscription. It reconnects with exponential backoff and jitter,
/// re-subscribes every subscription from its last cursor (so no event is lost or repeated), answers
/// the server's heartbeat, and refreshes its credential when the server closes with `4001`.
public actor RealtimeClient {
    private struct Sub {
        var spec: [String: JSON]
        var cursor: Int64?
        var active = false
        let continuation: AsyncStream<RealtimeEvent>.Continuation
    }

    private let transport: Transport
    private let auth: AuthManager
    private let config: RealtimeConfiguration
    private let factory: any WebSocketFactory
    private let now: @Sendable () -> Date

    private var subs: [String: Sub] = [:]
    private var subErrors: [String: ForgeBaseError] = [:]
    private var pending: [String: CheckedContinuation<JSON, Error>] = [:]
    private var openedWaiters: [CheckedContinuation<Void, Error>] = []
    private var listeners: [UUID: AsyncStream<RealtimeConnectionEvent>.Continuation] = [:]
    private var connection: (any WebSocketConnection)?
    private var loop: Task<Void, Never>?
    private var authWatch: Task<Void, Never>?
    private var stopped = false
    private var seq = 0
    private var lastReceived = Date.distantPast
    private(set) var state: RealtimeState = .idle

    init(transport: Transport, auth: AuthManager, configuration: RealtimeConfiguration, now: @escaping @Sendable () -> Date) {
        self.transport = transport
        self.auth = auth
        self.config = configuration
        self.factory = configuration.webSocketFactory ?? URLSessionWebSocketFactory(session: transport.session)
        self.now = now
    }

    public var connectionState: RealtimeState { state }

    /// Connection life-cycle events (a new stream per call).
    public func connectionEvents() -> AsyncStream<RealtimeConnectionEvent> {
        let id = UUID()
        let (stream, c) = AsyncStream<RealtimeConnectionEvent>.makeStream(bufferingPolicy: .bufferingNewest(32))
        listeners[id] = c
        c.onTermination = { [weak self] _ in Task { await self?.dropListener(id) } }
        return stream
    }

    private func dropListener(_ id: UUID) { listeners[id] = nil }

    private func emit(_ e: RealtimeConnectionEvent) {
        for l in listeners.values { l.yield(e) }
    }

    // MARK: public API

    /// Opens the connection (the first `subscribe` does it). Returns once the server welcomed us.
    public func connect() async throws {
        stopped = false
        if state == .open { return }
        if loop == nil {
            loop = Task { [weak self] in await self?.run() }
            startAuthWatch()
        }
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in openedWaiters.append(c) }
    }

    /// Subscribes to a collection, a record or a channel. Returns once the server accepted the subscription.
    public func subscribe(_ options: SubscribeOptions) async throws -> RealtimeSubscription {
        let id = options.id ?? "s\(nextSeq())_\(String(Int.random(in: 0..<1_000_000), radix: 36))"
        if subs[id] != nil { throw ForgeBaseError.realtime("duplicate_subscription", "subscription id \(id) is already in use") }
        let spec = try Self.spec(options)
        let (stream, continuation) = AsyncStream<RealtimeEvent>.makeStream()
        subs[id] = Sub(spec: spec, cursor: options.cursor, continuation: continuation)
        continuation.onTermination = { [weak self] _ in Task { await self?.unsubscribe(id) } }
        do {
            if state == .open {
                try await activate(id)
            } else {
                try await connect()
                if let e = subErrors.removeValue(forKey: id) { throw e }
            }
        } catch {
            subs[id]?.continuation.finish()
            subs[id] = nil
            throw error
        }
        return RealtimeSubscription(id: id, events: stream, client: self)
    }

    /// Convenience: `subscribe(.collection("posts"))`.
    public func subscribe(collection: String, filter: String? = nil, cursor: Int64? = nil) async throws -> RealtimeSubscription {
        try await subscribe(.collection(collection, filter: filter, cursor: cursor))
    }

    public func unsubscribe(_ id: String) async {
        guard let sub = subs.removeValue(forKey: id) else { return }
        sub.continuation.finish()
        if state == .open, sub.active { _ = try? await request(["type": "unsubscribe", "id": .string(id)]) }
    }

    func cursor(of id: String) -> Int64? { subs[id]?.cursor }

    /// Publishes to a channel: over the socket when open, else through REST.
    public func publish(channel: String, data: JSON) async throws {
        if state == .open {
            _ = try await request(["type": "publish", "channel": .string(channel), "data": data])
            return
        }
        try await transport.perform(Request("POST", "/api/v1/realtime/channels/\(seg(channel))/messages", body: wire(["data": data])))
    }

    /// Joins a channel's presence (the channel must be subscribed). Returns the members.
    @discardableResult
    public func joinPresence(channel: String, meta: JSON? = nil) async throws -> [PresenceMember] {
        let r = try await request(["type": "presence.join", "channel": .string(channel), "meta": meta ?? .null])
        return try (r["members"] ?? .array([])).decode([PresenceMember].self)
    }

    public func leavePresence(channel: String) async throws {
        _ = try await request(["type": "presence.leave", "channel": .string(channel)])
    }

    /// Members currently present (REST).
    public func presence(channel: String) async throws -> [PresenceMember] {
        let j = try await transport.json(Request("GET", "/api/v1/realtime/channels/\(seg(channel))/presence"))
        return try (j["members"] ?? .array([])).decode([PresenceMember].self)
    }

    /// Closes the connection and every subscription.
    public func close() {
        stopped = true
        authWatch?.cancel()
        authWatch = nil
        loop?.cancel()
        loop = nil
        for s in subs.values { s.continuation.finish() }
        subs.removeAll()
        fail(ForgeBaseError.realtime("closed", "realtime client closed"))
        if let c = connection { Task { await c.close(code: 1000, reason: "client closed") } }
        connection = nil
        state = .closed
    }

    // MARK: connection loop

    private func nextSeq() -> Int {
        seq += 1
        return seq
    }

    private static func spec(_ o: SubscribeOptions) throws -> [String: JSON] {
        var s: [String: JSON] = [:]
        if let v = o.collection { s["collection"] = .string(v) }
        if let v = o.record { s["record"] = .string(v) }
        if let v = o.filter { s["filter"] = .string(v) }
        if let v = o.params { s["params"] = .object(v) }
        if let v = o.channel { s["channel"] = .string(v) }
        if s["collection"] == nil && s["channel"] == nil {
            throw ForgeBaseError.invalidRequest("subscribe needs a collection or a channel")
        }
        return s
    }

    private func run() async {
        var attempt = 0
        let retry = transport.retry
        while !stopped && !Task.isCancelled {
            state = attempt == 0 ? .connecting : .reconnecting
            var welcomed = false
            do {
                welcomed = try await runSocket()
            } catch {
                if stopped || Task.isCancelled { break }
                emit(.error(error.localizedDescription))
                if let e = error as? ForgeBaseError, [400, 401, 403, 404].contains(e.status) {
                    if e.status == 401, (try? await refreshCredential()) != nil {} else {
                        fail(e)
                        break
                    }
                }
            }
            if stopped || Task.isCancelled { break }
            if welcomed { attempt = 0 }
            for id in subs.keys { subs[id]?.active = false }
            attempt += 1
            if let max = config.maxReconnectAttempts, attempt > max {
                fail(ForgeBaseError.realtime("reconnect_failed", "gave up after \(max) reconnect attempts"))
                break
            }
            let ceiling = min(config.reconnectMaxDelay, config.reconnectBaseDelay * pow(2, Double(attempt - 1)))
            let delay = welcomed && attempt == 1 ? 0 : retry.random() * ceiling
            state = .reconnecting
            emit(.reconnecting(attempt: attempt, delay: delay))
            do { try await retry.sleep(delay) } catch { break }
        }
        state = .closed
        loop = nil
        if !stopped { fail(ForgeBaseError.realtime("closed", "realtime connection ended")) }
    }

    private func refreshCredential() async throws {
        _ = try await auth.refresh(transport: transport)
    }

    private func wsURL() throws -> URL {
        guard var c = URLComponents(string: transport.baseURL + "/api/v1/realtime/ws") else {
            throw ForgeBaseError.invalidRequest("invalid realtime URL")
        }
        c.scheme = c.scheme == "https" ? "wss" : "ws"
        guard let u = c.url else { throw ForgeBaseError.invalidRequest("invalid realtime URL") }
        return u
    }

    /// Runs one connection. Returns whether the server welcomed us.
    private func runSocket() async throws -> Bool {
        var headers = transport.defaultHeaders
        if let creds = transport.credentials {
            for (k, v) in await creds.headers(for: "GET", transport: transport) { headers[k] = v }
        }
        let conn: any WebSocketConnection
        do {
            conn = try await factory.connect(to: try wsURL(), headers: headers)
        } catch {
            throw ForgeBaseError.realtime("connect_failed", "WebSocket connection failed: \(error.localizedDescription)")
        }
        connection = conn
        lastReceived = now()
        var welcomed = false
        var heartbeat: Task<Void, Never>?
        defer {
            heartbeat?.cancel()
            connection = nil
            failPending(ForgeBaseError.realtime("connection_closed", "connection closed"))
        }
        do {
            while true {
                let text = try await conn.receive()
                lastReceived = now()
                guard let m = try? JSON(data: Data(text.utf8)) else { continue }
                if m["type"]?.stringValue == "welcome", !welcomed {
                    welcomed = true
                    let hb = (m["heartbeat_ms"]?.doubleValue).map { $0 / 1000 } ?? 25
                    heartbeat = Task { [weak self] in await self?.heartbeat(every: hb, on: conn) }
                    state = .open
                    Task { [weak self] in await self?.afterWelcome() }
                    continue
                }
                handle(m)
            }
        } catch let closed as WebSocketClosed {
            emit(.close(code: closed.code, reason: closed.reason))
            if closed.code == 4001 { try? await refreshCredential() }
            if !welcomed { throw ForgeBaseError.realtime("connect_failed", "connection closed before welcome (\(closed.code))") }
            return welcomed
        } catch is CancellationError {
            await conn.close(code: 1000, reason: "cancelled")
            return welcomed
        } catch {
            emit(.close(code: 1006, reason: error.localizedDescription))
            if !welcomed { throw ForgeBaseError.realtime("connect_failed", "WebSocket connection failed: \(error.localizedDescription)") }
            return welcomed
        }
    }

    private func heartbeat(every interval: TimeInterval, on conn: any WebSocketConnection) async {
        let sleep = transport.retry.sleep
        while !Task.isCancelled {
            do { try await sleep(interval) } catch { return }
            if Task.isCancelled { return }
            if now().timeIntervalSince(lastReceived) > interval * 2.5 {
                await conn.close(code: 4002, reason: "heartbeat timeout")
                return
            }
            try? await conn.send(#"{"type":"ping"}"#)
        }
    }

    private func afterWelcome() async {
        await resubscribeAll()
        emit(.open)
        let w = openedWaiters
        openedWaiters = []
        for c in w { c.resume() }
    }

    /// Re-establishes every subscription from its last cursor.
    private func resubscribeAll() async {
        for id in Array(subs.keys) {
            do {
                try await activate(id)
            } catch let e as ForgeBaseError {
                // A subscription the server now refuses (rule changed while offline) is dropped and reported.
                subErrors[id] = e
                subs[id]?.continuation.finish()
                subs[id] = nil
                emit(.error(e.message))
            } catch {}
        }
    }

    private func activate(_ id: String) async throws {
        guard let sub = subs[id] else { return }
        var msg = sub.spec
        msg["type"] = "subscribe"
        msg["id"] = .string(id)
        if let c = sub.cursor { msg["cursor"] = .int(c) }
        let r = try await request(msg)
        guard subs[id] != nil else { return }
        subs[id]?.active = true
        if let c = r["cursor"]?.int64Value, subs[id]?.cursor == nil { subs[id]?.cursor = c }
    }

    private func request(_ msg: [String: JSON]) async throws -> JSON {
        guard let conn = connection, state == .open || msg["type"]?.stringValue == "subscribe" else {
            throw ForgeBaseError.realtime("not_connected", "realtime socket is not open")
        }
        let ref = String(nextSeq())
        var m = msg
        m["ref"] = .string(ref)
        let text = String(decoding: try JSON.object(m).data(), as: UTF8.self)
        return try await withCheckedThrowingContinuation { (c: CheckedContinuation<JSON, Error>) in
            pending[ref] = c
            Task { [weak self] in
                do { try await conn.send(text) } catch {
                    await self?.failRequest(ref, ForgeBaseError.realtime("send_failed", error.localizedDescription))
                }
            }
        }
    }

    private func failRequest(_ ref: String, _ e: Error) {
        pending.removeValue(forKey: ref)?.resume(throwing: e)
    }

    private func handle(_ m: JSON) {
        let type = m["type"]?.stringValue ?? ""
        let ref = m["ref"]?.stringValue ?? m["ref"]?.int64Value.map(String.init)
        if let ref, let p = pending.removeValue(forKey: ref) {
            if type == "error" {
                p.resume(throwing: ForgeBaseError.realtime(m["code"]?.stringValue ?? "error", m["message"]?.stringValue ?? "realtime error"))
            } else {
                p.resume(returning: m)
            }
            return
        }
        switch type {
        case "record.created", "record.updated", "record.deleted", "message", "presence.join", "presence.leave", "reset":
            guard let id = m["id"]?.stringValue, subs[id] != nil else { return }
            if let commit = m["commit"]?.stringValue {
                let t = transport
                Task { await t.commitTokens.note(commit) }
            }
            if let c = m["cursor"]?.int64Value, subs[id]?.cursor == nil || c > subs[id]!.cursor! || type == "reset" {
                subs[id]?.cursor = c
            }
            if let ev = Self.event(type: type, m: m, id: id) { subs[id]?.continuation.yield(ev) }
        case "error":
            emit(.error(m["message"]?.stringValue ?? "realtime error"))
        default:
            break
        }
    }

    private static func event(type: String, m: JSON, id: String) -> RealtimeEvent? {
        switch type {
        case "record.created", "record.updated", "record.deleted":
            guard let kind = RecordEventKind(rawValue: type) else { return nil }
            return .record(
                RecordEvent(
                    kind: kind, subscriptionId: id, cursor: m["cursor"]?.int64Value ?? 0,
                    collection: m["collection"]?.stringValue ?? "", recordId: m["record_id"]?.stringValue ?? "",
                    tenantId: m["tenant_id"]?.stringValue, timestamp: m["timestamp"]?.stringValue ?? "",
                    data: m["data"] ?? .null, commit: m["commit"]?.stringValue))
        case "message":
            return .message(
                ChannelMessage(
                    subscriptionId: id, channel: m["channel"]?.stringValue ?? "", from: m["from"]?.stringValue ?? "",
                    data: m["data"] ?? .null, timestamp: m["timestamp"]?.stringValue ?? ""))
        case "presence.join", "presence.leave":
            guard let member = try? m["member"]?.decode(PresenceMember.self) else { return nil }
            return .presence(
                PresenceEvent(
                    change: type == "presence.join" ? .join : .leave, subscriptionId: id,
                    channel: m["channel"]?.stringValue ?? "", member: member))
        case "reset":
            return .reset(ResetEvent(reason: m["reason"]?.stringValue ?? "", subscriptionId: id, cursor: m["cursor"]?.int64Value ?? 0))
        default:
            return nil
        }
    }

    // MARK: credential rotation

    private func startAuthWatch() {
        let auth = self.auth
        authWatch = Task { [weak self] in
            for await change in await auth.stateChanges() where change.event == .tokenRefreshed {
                guard let token = change.session?.accessToken else { continue }
                await self?.sendAuth(token)
            }
        }
    }

    private func sendAuth(_ token: String) async {
        guard state == .open, let conn = connection else { return }
        let m: JSON = ["type": "auth", "token": .string(token)]
        if let data = try? m.data() { try? await conn.send(String(decoding: data, as: UTF8.self)) }
    }

    // MARK: failures

    private func failPending(_ e: Error) {
        let p = pending
        pending = [:]
        for c in p.values { c.resume(throwing: e) }
    }

    private func fail(_ e: Error) {
        failPending(e)
        let w = openedWaiters
        openedWaiters = []
        for c in w { c.resume(throwing: e) }
    }
}
