import Foundation

public enum JobStatus: String, Sendable, Codable {
    case queued, running, succeeded, dead, cancelled
    public var isFinal: Bool { self == .succeeded || self == .dead || self == .cancelled }
}

/// A background job.
public struct Job: Sendable, Equatable {
    public let id: String
    public let queue: String
    public let kind: String
    public let payload: JSON
    public let priority: Int
    public let status: JobStatus
    public let attempts: Int
    public let maxAttempts: Int
    public let result: JSON?
    public let lastError: String?
    /// `{done, total, message...}` as reported by the worker.
    public let progress: JSON?
    public let createdAt: String?
    public let updatedAt: String?
    public let finishedAt: String?
    /// The whole job object as the server sent it.
    public let raw: JSON

    init(_ j: JSON) throws {
        guard let id = j["id"]?.stringValue, let st = j["status"]?.stringValue, let status = JobStatus(rawValue: st) else {
            throw ForgeBaseError.invalidResponse("not a job: \(j)")
        }
        self.id = id
        self.status = status
        queue = j["queue"]?.stringValue ?? ""
        kind = j["kind"]?.stringValue ?? ""
        payload = j["payload"] ?? .null
        priority = j["priority"]?.intValue ?? 0
        attempts = j["attempts"]?.intValue ?? 0
        maxAttempts = j["max_attempts"]?.intValue ?? 0
        result = j["result"].flatMap { $0.isNull ? nil : $0 }
        lastError = j["last_error"]?.stringValue
        progress = j["progress"].flatMap { $0.isNull ? nil : $0 }
        createdAt = j["created_at"]?.stringValue
        updatedAt = j["updated_at"]?.stringValue
        finishedAt = j["finished_at"]?.stringValue
        raw = j
    }
}

/// A live update delivered on the `jobs:<id>` channel.
public struct JobUpdate: Sendable, Equatable {
    /// `progress` (a report or checkpoint) or `status` (a state change).
    public let event: String
    public let jobId: String
    public let status: JobStatus?
    public let attempts: Int
    public let progress: JSON?
    public let lastError: String?
    public let updatedAt: String
    public let raw: JSON

    init(_ j: JSON) {
        event = j["event"]?.stringValue ?? "status"
        jobId = j["job_id"]?.stringValue ?? j["id"]?.stringValue ?? ""
        status = j["status"]?.stringValue.flatMap(JobStatus.init(rawValue:))
        attempts = j["attempts"]?.intValue ?? 0
        progress = j["progress"].flatMap { $0.isNull ? nil : $0 }
        lastError = j["last_error"]?.stringValue
        updatedAt = j["updated_at"]?.stringValue ?? ""
        raw = j
    }
}

public struct EnqueueOptions: Sendable {
    public var queue: String?
    public var priority: Int?
    /// RFC 3339 start time.
    public var runAt: Date?
    public var delayMs: Int64?
    public var maxAttempts: Int?
    public var backoffBaseMs: Int64?
    public var backoffMaxMs: Int64?
    public var visibilityTimeoutMs: Int64?
    /// A repeat with the same key returns the existing job (and is retried safely).
    public var idempotencyKey: String?
    /// Longest run of one attempt, in ms (1 s to 30 days).
    public var timeoutMs: Int64?
    /// Job ids that must succeed first.
    public var dependsOn: [String]?
    public init(
        queue: String? = nil, priority: Int? = nil, runAt: Date? = nil, delayMs: Int64? = nil, maxAttempts: Int? = nil,
        backoffBaseMs: Int64? = nil, backoffMaxMs: Int64? = nil, visibilityTimeoutMs: Int64? = nil,
        idempotencyKey: String? = nil, timeoutMs: Int64? = nil, dependsOn: [String]? = nil
    ) {
        self.queue = queue
        self.priority = priority
        self.runAt = runAt
        self.delayMs = delayMs
        self.maxAttempts = maxAttempts
        self.backoffBaseMs = backoffBaseMs
        self.backoffMaxMs = backoffMaxMs
        self.visibilityTimeoutMs = visibilityTimeoutMs
        self.idempotencyKey = idempotencyKey
        self.timeoutMs = timeoutMs
        self.dependsOn = dependsOn
    }
}

public struct ListJobsOptions: Sendable {
    public var status: JobStatus?
    public var queue: String?
    public var kind: String?
    public var limit: Int?
    public var offset: Int?
    public var workflowId: String?
    public init(status: JobStatus? = nil, queue: String? = nil, kind: String? = nil, limit: Int? = nil, offset: Int? = nil, workflowId: String? = nil) {
        self.status = status
        self.queue = queue
        self.kind = kind
        self.limit = limit
        self.offset = offset
        self.workflowId = workflowId
    }
}

/// `wait` ended in a failure: the job went `dead` or `cancelled`, or the wait timed out.
public func jobFailure(_ job: Job, message: String? = nil) -> ForgeBaseError {
    ForgeBaseError(
        kind: .jobFailed, code: "job_\(job.status.rawValue)",
        message: message ?? "job \(job.id) ended \(job.status.rawValue)\(job.lastError.map { ": \($0)" } ?? "")", details: job.raw)
}

/// Background jobs: `fb.jobs`. Workers (handlers) run on the server; the SDK enqueues, inspects and follows jobs.
public struct JobsAPI: Sendable {
    let t: Transport
    let realtime: RealtimeClient
    private static let base = "/api/v1/jobs"

    /// Enqueues a job of `kind`. With an `idempotencyKey` the call is safe to retry.
    @discardableResult
    public func enqueue<P: Encodable>(_ kind: String, payload: P, _ o: EnqueueOptions = .init()) async throws -> Job {
        var r = Request(
            "POST", Self.base,
            body: wire([
                "kind": kind, "payload": try JSON(encoding: payload), "queue": o.queue, "priority": o.priority,
                "run_at": o.runAt, "delay_ms": o.delayMs, "max_attempts": o.maxAttempts, "backoff_base_ms": o.backoffBaseMs,
                "backoff_max_ms": o.backoffMaxMs, "visibility_timeout_ms": o.visibilityTimeoutMs,
                "idempotency_key": o.idempotencyKey, "timeout_ms": o.timeoutMs, "depends_on": o.dependsOn,
            ]))
        if o.idempotencyKey == nil { r.retry = .disabled }
        return try Job(try await t.json(r))
    }

    @discardableResult
    public func enqueue(_ kind: String, _ o: EnqueueOptions = .init()) async throws -> Job {
        try await enqueue(kind, payload: JSON.null, o)
    }

    public func get(_ id: String) async throws -> Job {
        try Job(try await t.json(Request("GET", "\(Self.base)/\(seg(id))")))
    }

    public func list(_ o: ListJobsOptions = .init()) async throws -> (items: [Job], total: Int) {
        let j = try await t.json(
            Request(
                "GET", Self.base,
                query: query(["status": o.status?.rawValue, "queue": o.queue, "kind": o.kind, "limit": o.limit, "offset": o.offset, "workflow_id": o.workflowId])))
        return (try (j["items"]?.arrayValue ?? []).map(Job.init), j["total"]?.intValue ?? 0)
    }

    @discardableResult
    public func cancel(_ id: String) async throws -> Job {
        try Job(try await t.json(Request("POST", "\(Self.base)/\(seg(id))/cancel")))
    }

    @discardableResult
    public func retry(_ id: String) async throws -> Job {
        try Job(try await t.json(Request("POST", "\(Self.base)/\(seg(id))/retry")))
    }

    public func delete(_ id: String) async throws {
        try await t.perform(Request("DELETE", "\(Self.base)/\(seg(id))"))
    }

    /// The stored result of a finished job (`stored` is false when the worker returned none).
    public func result(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/\(seg(id))/result"))
    }

    public func children(_ id: String) async throws -> [Job] {
        try (try await t.json(Request("GET", "\(Self.base)/\(seg(id))/children"))["items"]?.arrayValue ?? []).map(Job.init)
    }

    public func stats() async throws -> JSON { try await t.json(Request("GET", "\(Self.base)/stats")) }

    /// Returns when the job succeeds; throws a `.jobFailed` error when it ends `dead` or `cancelled`, or when
    /// `timeout` passes. Uses plain polling, so it works without a realtime connection.
    public func wait(_ id: String, pollInterval: TimeInterval = 1, timeout: TimeInterval = 300) async throws -> Job {
        let deadline = Date().addingTimeInterval(timeout)
        let sleep = t.retry.sleep
        while true {
            try Task.checkCancellation()
            let job = try await get(id)
            if job.status == .succeeded { return job }
            if job.status == .dead || job.status == .cancelled { throw jobFailure(job) }
            if Date() >= deadline { throw jobFailure(job, message: "job \(id) still \(job.status.rawValue) after \(Int(timeout)) s") }
            try await sleep(pollInterval)
        }
    }

    /// Watches one job: the stored state first (a late subscriber sees the current progress), then every live update
    /// from the `jobs:<id>` realtime channel. The stream finishes after the final update; cancel the consuming
    /// task to stop earlier.
    public func updates(_ id: String, progressOnly: Bool = false) async throws -> AsyncThrowingStream<JobUpdate, Error> {
        let sub = try await realtime.subscribe(.channel("jobs:\(id)"))
        let job: Job
        do { job = try await get(id) } catch {
            await sub.unsubscribe()
            throw error
        }
        let snapshot = JobUpdate(job.raw)
        return AsyncThrowingStream { continuation in
            let task = Task {
                var seen = ""
                func deliver(_ u: JobUpdate) -> Bool {
                    if progressOnly && u.event != "progress" { return u.status?.isFinal ?? false }
                    if !u.updatedAt.isEmpty && !seen.isEmpty && u.updatedAt < seen { return false }
                    if !u.updatedAt.isEmpty { seen = u.updatedAt }
                    continuation.yield(u)
                    return u.status?.isFinal ?? false
                }
                if deliver(snapshot), job.status.isFinal {
                    await sub.unsubscribe()
                    continuation.finish()
                    return
                }
                for await event in sub.events {
                    guard case .message(let m) = event else { continue }
                    if deliver(JobUpdate(m.data)) { break }
                }
                await sub.unsubscribe()
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    // MARK: queues of jobs

    public func queues() async throws -> [JSON] {
        try await t.json(Request("GET", "\(Self.base)/queues"))["items"]?.arrayValue ?? []
    }

    // MARK: workflows

    public func workflow(_ id: String) async throws -> JSON {
        try await t.json(Request("GET", "\(Self.base)/workflows/\(seg(id))"))
    }

    public func cancelWorkflow(_ id: String) async throws -> JSON {
        try await t.json(Request("POST", "\(Self.base)/workflows/\(seg(id))/cancel"))
    }
}
