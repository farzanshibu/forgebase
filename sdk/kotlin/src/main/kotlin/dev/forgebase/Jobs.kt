package dev.forgebase

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.onCompletion
import kotlinx.coroutines.flow.transformWhile
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

/*
 * Background jobs and long-running tasks (`/api/v1/jobs`, ADR-0023): `fb.jobs`.
 * Enqueueing, watching, cancelling and waiting are here; queue pause/resume and workflows take and return JSON.
 */

public enum class JobStatus(public val wire: String) {
    QUEUED("queued"), RUNNING("running"), SUCCEEDED("succeeded"), DEAD("dead"), CANCELLED("cancelled");

    /** True once the job can no longer change by itself (`dead` can still be retried by hand). */
    public val isFinal: Boolean get() = this == SUCCEEDED || this == DEAD || this == CANCELLED

    public companion object {
        public fun of(wire: String): JobStatus? = entries.firstOrNull { it.wire == wire }
    }
}

@Serializable
public data class JobProgress(
    val percent: Double? = null,
    val stage: String? = null,
    val message: String? = null,
    @SerialName("updated_at") val updatedAt: String = "",
)

@Serializable
public data class Job(
    val id: String,
    val queue: String = "",
    val kind: String = "",
    val payload: JsonElement = JsonNull,
    val priority: Int = 0,
    val status: String,
    val attempts: Int = 0,
    @SerialName("max_attempts") val maxAttempts: Int = 0,
    val result: JsonElement? = null,
    @SerialName("last_error") val lastError: String? = null,
    val progress: JobProgress? = null,
    @SerialName("cancel_requested") val cancelRequested: Boolean = false,
    @SerialName("workflow_id") val workflowId: String? = null,
    @SerialName("created_at") val createdAt: String? = null,
    @SerialName("updated_at") val updatedAt: String? = null,
    @SerialName("finished_at") val finishedAt: String? = null,
) {
    /** [status] as an enum (`null` for a status this SDK does not know). */
    public val state: JobStatus? get() = JobStatus.of(status)
}

/** A live update delivered on the `jobs:<id>` channel. */
@Serializable
public data class JobUpdate(
    /** `progress` (a report or checkpoint) or `status` (a state change). */
    val event: String = "status",
    @SerialName("job_id") val jobId: String,
    val queue: String = "",
    val kind: String = "",
    val status: String,
    val attempts: Int = 0,
    val progress: JobProgress? = null,
    @SerialName("cancel_requested") val cancelRequested: Boolean = false,
    @SerialName("last_error") val lastError: String? = null,
    @SerialName("updated_at") val updatedAt: String = "",
    @SerialName("finished_at") val finishedAt: String? = null,
) {
    public val state: JobStatus? get() = JobStatus.of(status)
}

public data class EnqueueOptions(
    val queue: String? = null,
    val priority: Int? = null,
    /** RFC 3339 start time. */
    val runAt: String? = null,
    val delayMs: Long? = null,
    val maxAttempts: Int? = null,
    val backoffBaseMs: Long? = null,
    val backoffMaxMs: Long? = null,
    val visibilityTimeoutMs: Long? = null,
    /** A repeat with the same key returns the existing job (and is retried safely). */
    val idempotencyKey: String? = null,
    /** Longest run of one attempt, in ms (1 s to 30 days). */
    val timeoutMs: Long? = null,
    /** Job ids that must succeed first. */
    val dependsOn: List<String>? = null,
)

public data class ListJobsOptions(
    val status: JobStatus? = null,
    val queue: String? = null,
    val kind: String? = null,
    val limit: Int? = null,
    val offset: Int? = null,
    val workflowId: String? = null,
)

public data class JobPage(val items: List<Job>, val total: Long)

/** A job did not finish in time or ended unsuccessfully ([JobsApi.await]). */
public class JobFailedException(public val job: Job, message: String? = null) :
    ForgeBaseException(message ?: "job ${job.id} ${job.status}${job.lastError?.let { ": $it" } ?: ""}", 0, "job_failed")

private const val J = "/api/v1/jobs"

/** Jobs and long tasks: enqueue, watch progress live, results, queues and workflows: `fb.jobs`. */
public class JobsApi internal constructor(private val t: Transport, private val rt: RealtimeClient) {
    /** Enqueues a job; [payload] is any JSON-able value. Without an idempotency key the call is not retried. */
    public suspend fun enqueue(kind: String, payload: Any? = null, o: EnqueueOptions = EnqueueOptions()): Job {
        val body = obj(
            "kind" to kind, "payload" to (payload ?: JsonNull), "queue" to o.queue, "priority" to o.priority, "run_at" to o.runAt,
            "delay_ms" to o.delayMs, "max_attempts" to o.maxAttempts, "backoff_base_ms" to o.backoffBaseMs, "backoff_max_ms" to o.backoffMaxMs,
            "visibility_timeout_ms" to o.visibilityTimeoutMs, "idempotency_key" to o.idempotencyKey, "timeout_ms" to o.timeoutMs,
            "depends_on" to o.dependsOn,
        )
        return t.request(J, "POST", body = body, retry = o.idempotencyKey != null).decode()
    }

    public suspend fun get(id: String): Job = t.request("$J/${seg(id)}").decode()

    public suspend fun list(o: ListJobsOptions = ListJobsOptions()): JobPage {
        val r = t.request(
            J, query = mapOf("status" to o.status?.wire, "queue" to o.queue, "kind" to o.kind, "limit" to o.limit, "offset" to o.offset, "workflow_id" to o.workflowId),
        )
        return JobPage(r.items().map { it.decode() }, r.objOrNull()?.get("total").long() ?: 0)
    }

    public suspend fun cancel(id: String): Job = t.request("$J/${seg(id)}/cancel", "POST").decode()
    public suspend fun retry(id: String): Job = t.request("$J/${seg(id)}/retry", "POST").decode()

    public suspend fun delete(id: String) {
        t.request("$J/${seg(id)}", "DELETE")
    }

    public suspend fun stats(): JsonObject = t.request("$J/stats") as JsonObject
    public suspend fun attempts(id: String): List<JsonElement> = t.request("$J/${seg(id)}/attempts").items()
    public suspend fun checkpoint(id: String): JsonObject = t.request("$J/${seg(id)}/checkpoint") as JsonObject

    /** The stored result: `{ job_id, stored, size, result }`. */
    public suspend fun result(id: String): JsonObject = t.request("$J/${seg(id)}/result") as JsonObject

    public suspend fun children(id: String): List<Job> = t.request("$J/${seg(id)}/children").items().map { it.decode() }

    /**
     * Watches one job: the flow emits the stored state first (so a late collector sees the current progress), then every
     * live update from the `jobs:<id>` realtime channel, and completes once the job reaches a final state.
     */
    public fun progress(id: String, progressOnly: Boolean = false): Flow<JobUpdate> = flow {
        var seenAt = ""
        val live = rt.subscribe(SubscribeOptions(channel = "jobs:$id"))
        val snapshot = get(id).toUpdate()
        val updates = flow {
            emit(snapshot)
            if (snapshot.state?.isFinal == true) return@flow
            live.collect { e ->
                if (e is RealtimeEvent.Message) runCatching { e.data.decode<JobUpdate>() }.getOrNull()?.let { emit(it) }
            }
        }
        updates.transformWhile { u ->
            if (u.updatedAt.isNotEmpty() && seenAt.isNotEmpty() && u.updatedAt < seenAt) return@transformWhile true // older than the snapshot
            if (u.updatedAt.isNotEmpty()) seenAt = u.updatedAt
            if (!(progressOnly && u.event != "progress")) emit(u)
            u.state?.isFinal != true
        }.onCompletion { }.collect { emit(it) }
    }

    /**
     * Resolves when the job succeeds; throws [JobFailedException] when it ends `dead` or `cancelled`, or when
     * [timeoutMs] passes. Uses plain polling, so it works without a realtime connection.
     */
    public suspend fun await(id: String, pollMs: Long = 1000, timeoutMs: Long = 300_000): Job {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            currentCoroutineContext().ensureActive()
            val job = get(id)
            when (job.state) {
                JobStatus.SUCCEEDED -> return job
                JobStatus.DEAD, JobStatus.CANCELLED -> throw JobFailedException(job)
                else -> if (System.currentTimeMillis() >= deadline) throw JobFailedException(job, "job $id still ${job.status} after $timeoutMs ms")
            }
            delay(pollMs)
        }
    }

    public suspend fun queues(): List<JsonObject> = t.request("$J/queues").items().map { it as JsonObject }

    // ---------------------------------------------------------------- workflows

    /** Creates a workflow from its spec (`{name, on_failure?, steps:[{name, kind, payload, depends_on, ...}]}`). */
    public suspend fun createWorkflow(spec: Map<String, Any?>): JsonObject =
        t.request("$J/workflows", "POST", body = toJsonElement(spec), retry = false) as JsonObject

    public suspend fun workflow(id: String): JsonObject = t.request("$J/workflows/${seg(id)}") as JsonObject
    public suspend fun cancelWorkflow(id: String): JsonObject = t.request("$J/workflows/${seg(id)}/cancel", "POST") as JsonObject
}

private fun Job.toUpdate(): JobUpdate = JobUpdate(
    event = "status", jobId = id, queue = queue, kind = kind, status = status, attempts = attempts, progress = progress,
    cancelRequested = cancelRequested, lastError = lastError, updatedAt = updatedAt ?: "", finishedAt = finishedAt,
)
