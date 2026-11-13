import Foundation

/// A message of a topic.
public struct QueueMessage: Sendable {
    public let id: String
    public let topic: String
    public let seq: Int64
    public let data: JSON
    public let attributes: [String: String]
    public let orderingKey: String?
    public let dedupId: String?
    public let publisher: String
    public let publishedAt: String

    init(_ j: JSON) {
        id = j["id"]?.stringValue ?? ""
        topic = j["topic"]?.stringValue ?? ""
        seq = j["seq"]?.int64Value ?? 0
        data = j["data"] ?? .null
        attributes = (j["attributes"]?.objectValue ?? [:]).compactMapValues(\.stringValue)
        orderingKey = j["ordering_key"]?.stringValue
        dedupId = j["dedup_id"]?.stringValue
        publisher = j["publisher"]?.stringValue ?? ""
        publishedAt = j["published_at"]?.stringValue ?? ""
    }

    /// The message data decoded into your type.
    public func payload<T: Decodable>(as type: T.Type = T.self) throws -> T { try data.decode(T.self) }
}

/// One delivery of a message (pull or stream).
public struct ReceivedMessage: Sendable {
    public let ackId: String
    /// 1 on the first delivery.
    public let deliveryAttempt: Int
    public let ackDeadline: String
    public let message: QueueMessage

    init(_ j: JSON) {
        ackId = j["ack_id"]?.stringValue ?? ""
        deliveryAttempt = j["delivery_attempt"]?.intValue ?? 1
        ackDeadline = j["ack_deadline"]?.stringValue ?? ""
        message = QueueMessage(j["message"] ?? .null)
    }
}

public struct Published: Sendable, Equatable {
    public let messageId: String
    public let seq: Int64
    /// The dedup id was seen inside the window: nothing was stored, `messageId` is the first message's.
    public let duplicate: Bool
}

public struct AckResult: Sendable, Equatable {
    public let count: Int
    /// Ack ids the server no longer knows (expired and delivered again, or already settled).
    public let unknown: [String]
}

public struct PublishOptions: Sendable {
    public var attributes: [String: String]
    /// Messages with one ordering key reach an ordered subscription in publish order.
    public var orderingKey: String?
    /// A repeat inside the topic's dedup window is accepted once (and makes the publish safe to retry).
    public var dedupId: String?
    public init(attributes: [String: String] = [:], orderingKey: String? = nil, dedupId: String? = nil) {
        self.attributes = attributes
        self.orderingKey = orderingKey
        self.dedupId = dedupId
    }
}

public struct CreateTopicOptions: Sendable {
    public var retentionMs: Int64?
    public var retentionBytes: Int64?
    public var maxMessageBytes: Int?
    public var dedupWindowMs: Int64?
    /// JSON Schema (documented subset) that publishes are validated against.
    public var schema: JSON?
    public var labels: [String: String]
    public init(retentionMs: Int64? = nil, retentionBytes: Int64? = nil, maxMessageBytes: Int? = nil, dedupWindowMs: Int64? = nil, schema: JSON? = nil, labels: [String: String] = [:]) {
        self.retentionMs = retentionMs
        self.retentionBytes = retentionBytes
        self.maxMessageBytes = maxMessageBytes
        self.dedupWindowMs = dedupWindowMs
        self.schema = schema
        self.labels = labels
    }
}

public struct CreateSubscriptionOptions: Sendable {
    /// Attribute filter, e.g. `["kind": "rush"]` or `["kind": ["in": ["a", "b"]]]`.
    public var filter: JSON?
    public var ackDeadlineMs: Int?
    public var maxDeliveryAttempts: Int?
    public var backoffBaseMs: Int?
    public var backoffMaxMs: Int?
    public var deadLetterTopic: String?
    public var ordering: Bool?
    /// Where a new subscription starts reading: `"latest"` (default), `"earliest"` or `["time": "<rfc3339>"]`.
    public var start: JSON?
    public var labels: [String: String]
    public init(
        filter: JSON? = nil, ackDeadlineMs: Int? = nil, maxDeliveryAttempts: Int? = nil, backoffBaseMs: Int? = nil,
        backoffMaxMs: Int? = nil, deadLetterTopic: String? = nil, ordering: Bool? = nil, start: JSON? = nil, labels: [String: String] = [:]
    ) {
        self.filter = filter
        self.ackDeadlineMs = ackDeadlineMs
        self.maxDeliveryAttempts = maxDeliveryAttempts
        self.backoffBaseMs = backoffBaseMs
        self.backoffMaxMs = backoffMaxMs
        self.deadLetterTopic = deadLetterTopic
        self.ordering = ordering
        self.start = start
        self.labels = labels
    }
}

public struct PullOptions: Sendable {
    /// 1-1000, default 10.
    public var maxMessages: Int?
    /// Long-poll up to this many milliseconds when nothing is ready.
    public var waitMs: Int?
    public var consumer: String?
    public init(maxMessages: Int? = nil, waitMs: Int? = nil, consumer: String? = nil) {
        self.maxMessages = maxMessages
        self.waitMs = waitMs
        self.consumer = consumer
    }
}

public struct QueueSubscribeOptions: Sendable {
    /// Most unacknowledged messages this stream holds (flow control, default 100).
    public var maxMessages: Int?
    /// Most unacknowledged bytes this stream holds (default 10 MiB).
    public var maxBytes: Int?
    /// Ack when the handler returns and nack when it throws (default true). Turn off to settle by hand.
    public var autoAck: Bool
    /// Reconnect after a dropped stream (default true), with exponential backoff.
    public var reconnect: Bool
    /// Called with errors that do not end the stream (a handler that threw, a failed ack).
    public var onError: (@Sendable (Error) -> Void)?
    public init(maxMessages: Int? = nil, maxBytes: Int? = nil, autoAck: Bool = true, reconnect: Bool = true, onError: (@Sendable (Error) -> Void)? = nil) {
        self.maxMessages = maxMessages
        self.maxBytes = maxBytes
        self.autoAck = autoAck
        self.reconnect = reconnect
        self.onError = onError
    }
}

/// A delivery handed to a `subscribe` handler.
public struct Delivery: Sendable {
    public let received: ReceivedMessage
    let queues: QueuesAPI
    let subscription: String
    private let settled = Locked(false)

    init(received: ReceivedMessage, queues: QueuesAPI, subscription: String) {
        self.received = received
        self.queues = queues
        self.subscription = subscription
    }

    public var message: QueueMessage { received.message }
    public var data: JSON { received.message.data }
    public var attributes: [String: String] { received.message.attributes }
    public var attempt: Int { received.deliveryAttempt }

    public func payload<T: Decodable>(as type: T.Type = T.self) throws -> T { try data.decode(T.self) }

    /// Acknowledge now (the handler returning normally does it for you unless `autoAck` is off).
    public func ack() async throws {
        guard settle() else { return }
        _ = try await queues.ack(subscription, ackIds: [received.ackId])
    }

    /// Negative acknowledgement: the message is redelivered after the subscription's backoff.
    public func nack() async throws {
        guard settle() else { return }
        _ = try await queues.nack(subscription, ackIds: [received.ackId])
    }

    /// Give the handler more time (milliseconds from now, 1 s to 10 min).
    public func extend(ackDeadlineMs: Int) async throws {
        _ = try await queues.modifyDeadline(subscription, ackIds: [received.ackId], ackDeadlineMs: ackDeadlineMs)
    }

    var isSettled: Bool { settled.withLock { $0 } }
    private func settle() -> Bool { settled.withLock { s in
        if s { return false }
        s = true
        return true
    } }
}

/// A running streaming pull.
public struct QueueStream: Sendable {
    let task: Task<Void, Error>

    /// Stops the stream; deliveries still held return after their ack deadline.
    public func cancel() { task.cancel() }

    /// Returns when the stream ended (after `cancel`), throws when the server refused it for good.
    public func wait() async throws {
        do { try await task.value } catch is CancellationError {}
    }
}

/// Topics, subscriptions, publish, pull and streaming pull: `fb.queues`.
public struct QueuesAPI: Sendable {
    let t: Transport
    private static let base = "/api/v1/queues"

    // MARK: topics

    public func listTopics() async throws -> [JSON] {
        try await t.json(Request("GET", "\(Self.base)/topics"))["items"]?.arrayValue ?? []
    }

    public func getTopic(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/topics/\(seg(name))"))
    }

    @discardableResult
    public func createTopic(_ name: String, _ o: CreateTopicOptions = .init()) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "\(Self.base)/topics",
                body: wire([
                    "name": name, "retention_ms": o.retentionMs, "retention_bytes": o.retentionBytes,
                    "max_message_bytes": o.maxMessageBytes, "dedup_window_ms": o.dedupWindowMs, "schema": o.schema,
                    "labels": o.labels.isEmpty ? nil : o.labels,
                ])))
    }

    /// Deletes a topic; `force` deletes its subscriptions too.
    public func deleteTopic(_ name: String, force: Bool = false) async throws {
        try await t.perform(Request("DELETE", "\(Self.base)/topics/\(seg(name))", query: query(["force": force ? true : nil])))
    }

    public func topicMetrics(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/topics/\(seg(name))/metrics"))
    }

    // MARK: publish

    /// Publishes one message. Returns its id and sequence number.
    @discardableResult
    public func publish<T: Encodable>(_ topic: String, _ data: T, _ o: PublishOptions = .init()) async throws -> Published {
        guard let first = try await publishBatch(topic, [(try JSON(encoding: data), o)]).first else {
            throw ForgeBaseError.invalidResponse("publish returned no result")
        }
        return first
    }

    /// Publishes up to 1000 messages in one request; results are in the order sent.
    public func publishBatch(_ topic: String, _ messages: [(data: JSON, options: PublishOptions)]) async throws -> [Published] {
        let list: [JSON] = messages.map { m in
            wire([
                "data": m.data, "attributes": m.options.attributes.isEmpty ? nil : m.options.attributes,
                "ordering_key": m.options.orderingKey, "dedup_id": m.options.dedupId,
            ])
        }
        var r = Request("POST", "\(Self.base)/topics/\(seg(topic))/publish", body: ["messages": .array(list)])
        // A dedup id makes a repeat harmless, so those publishes are safe to retry after a network failure.
        if messages.isEmpty || !messages.allSatisfy({ $0.options.dedupId != nil }) { r.retry = .disabled }
        let j = try await t.json(r)
        return (j["results"]?.arrayValue ?? []).map {
            Published(messageId: $0["message_id"]?.stringValue ?? "", seq: $0["seq"]?.int64Value ?? 0, duplicate: $0["duplicate"]?.boolValue ?? false)
        }
    }

    // MARK: subscriptions

    public func listSubscriptions(topic: String? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "\(Self.base)/subscriptions", query: query(["topic": topic])))["items"]?.arrayValue ?? []
    }

    public func getSubscription(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/subscriptions/\(seg(name))"))
    }

    @discardableResult
    public func createSubscription(_ name: String, topic: String, _ o: CreateSubscriptionOptions = .init()) async throws -> JSON {
        try await t.json(
            Request(
                "POST", "\(Self.base)/subscriptions",
                body: wire([
                    "name": name, "topic": topic, "filter": o.filter, "ack_deadline_ms": o.ackDeadlineMs,
                    "max_delivery_attempts": o.maxDeliveryAttempts, "backoff_base_ms": o.backoffBaseMs,
                    "backoff_max_ms": o.backoffMaxMs, "dead_letter_topic": o.deadLetterTopic, "ordering": o.ordering,
                    "start": o.start, "labels": o.labels.isEmpty ? nil : o.labels,
                ])))
    }

    public func deleteSubscription(_ name: String) async throws {
        try await t.perform(Request("DELETE", "\(Self.base)/subscriptions/\(seg(name))"))
    }

    public func subscriptionMetrics(_ name: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/subscriptions/\(seg(name))/metrics"))
    }

    // MARK: pull and ack

    /// Takes up to `maxMessages` ready messages (leasing them for the subscription's ack deadline). Acknowledge each with `ack`.
    /// Not retried automatically: a blind retry could lease the messages twice.
    public func pull(_ subscription: String, _ o: PullOptions = .init()) async throws -> [ReceivedMessage] {
        var r = Request(
            "POST", "\(Self.base)/subscriptions/\(seg(subscription))/pull",
            body: wire(["max_messages": o.maxMessages, "wait_ms": o.waitMs, "consumer": o.consumer]))
        r.retry = .disabled
        return try await t.json(r)["messages"]?.arrayValue?.map(ReceivedMessage.init) ?? []
    }

    @discardableResult
    public func ack(_ subscription: String, ackIds: [String]) async throws -> AckResult {
        try await settle("ack", subscription, ["ack_ids": ackIds])
    }

    @discardableResult
    public func nack(_ subscription: String, ackIds: [String]) async throws -> AckResult {
        try await settle("nack", subscription, ["ack_ids": ackIds])
    }

    /// Changes the ack deadline of held messages (1 s to 10 min from now; `0` is a nack).
    @discardableResult
    public func modifyDeadline(_ subscription: String, ackIds: [String], ackDeadlineMs: Int) async throws -> AckResult {
        try await settle("modify-deadline", subscription, ["ack_ids": ackIds, "ack_deadline_ms": ackDeadlineMs])
    }

    private func settle(_ op: String, _ subscription: String, _ body: KeyValuePairs<String, (any JSONConvertible)?>) async throws -> AckResult {
        let j = try await t.json(Request("POST", "\(Self.base)/subscriptions/\(seg(subscription))/\(op)", body: wire(body)))
        return AckResult(count: j["count"]?.intValue ?? 0, unknown: j["unknown"]?.arrayValue?.compactMap(\.stringValue) ?? [])
    }

    /// Messages that used up their delivery attempts, newest first.
    public func deadLetters(_ subscription: String, before: Int64? = nil, limit: Int? = nil) async throws -> [JSON] {
        try await t.json(Request("GET", "\(Self.base)/subscriptions/\(seg(subscription))/dead-letters", query: query(["before": before, "limit": limit])))["items"]?.arrayValue ?? []
    }

    /// Replays: moves the cursor back to a time (RFC 3339, inside the topic's retention) so later messages are delivered again.
    @discardableResult
    public func seek(_ subscription: String, to time: Date) async throws -> JSON {
        try await t.json(Request("POST", "\(Self.base)/subscriptions/\(seg(subscription))/seek", body: wire(["time": time])))
    }

    // MARK: streaming pull

    /// Streaming pull with acknowledgement: `handler` runs for every delivery as the server pushes it (server-sent
    /// events, flow-controlled by `maxMessages` / `maxBytes`). When the handler returns the message is acked; when it
    /// throws it is nacked and redelivered after the subscription's backoff (`autoAck: false` leaves both to the
    /// handler). A dropped stream reconnects with backoff; a refusal (missing permission, push subscription, no such
    /// subscription) ends it, and `wait()` throws.
    public func subscribe(
        _ subscription: String, options o: QueueSubscribeOptions = .init(),
        handler: @escaping @Sendable (Delivery) async throws -> Void
    ) -> QueueStream {
        let events = t.eventStream(
            {
                Request(
                    "GET", "\(Self.base)/subscriptions/\(seg(subscription))/events",
                    query: query(["max_messages": o.maxMessages, "max_bytes": o.maxBytes]))
            }, reconnect: o.reconnect)
        let me = self
        let task = Task {
            try await withThrowingTaskGroup(of: Void.self) { group in
                for try await ev in events {
                    guard ev.event == "message", !ev.data.isEmpty, let j = try? JSON(data: Data(ev.data.utf8)) else { continue }
                    let d = Delivery(received: ReceivedMessage(j), queues: me, subscription: subscription)
                    group.addTask {
                        do {
                            try await handler(d)
                            if o.autoAck { try await d.ack() }
                        } catch {
                            o.onError?(error)
                            if o.autoAck { do { try await d.nack() } catch { o.onError?(error) } }
                        }
                    }
                }
                try await group.waitForAll()
            }
        }
        return QueueStream(task: task)
    }
}
