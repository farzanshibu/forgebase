//! `/api/v1/jobs`: enqueue, inspect and manage jobs and schedules, and the
//! external worker protocol (claim, heartbeat, complete, fail).
//!
//! Handlers only translate HTTP; [`JobService`] authorises each call through
//! the data core's authorizer (anonymous callers get `401`, callers without
//! permission `403`).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_data::{AccessContext, Action};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json_;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::JobError;
use crate::model::{
    ClaimRequest, ClaimedJob, Heartbeat, Job, JobEvent, JobFilter, JobPage, JobStats, JobStatus, NewJob, NewSchedule,
    NewWorker, QueueStats, Schedule, SchedulePatch, Worker,
};
use crate::service::JobService;
use crate::tasks::{
    Attempt, Checkpoint, Compensation, FailurePolicy, JobUpdate, MAX_RESULT_BYTES, NewStep, NewWorkflow, ParentResult,
    Progress, ProgressReport, QueueInfo, QueueSettings, RatePeriod, Workflow, WorkflowDetail, WorkflowPage,
    WorkflowStatus, WorkflowStep, WorkflowUpdate,
};

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

/// Authorises first, then surfaces the body: an anonymous or forbidden caller
/// gets `401`/`403` whatever they sent, never a `400` about their input.
fn body<T>(s: &JobService, ctx: &AccessContext, action: Action, body: Body<T>) -> Result<T, ApiError> {
    s.authorize(ctx, action).map_err(api)?;
    Ok(body?.0)
}

fn api(e: JobError) -> ApiError {
    ApiError::from_error(&e)
}

/// Largest JSON body of the jobs API (payloads are limited separately).
const MAX_BODY: usize = 1024 * 1024;

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(service: JobService) -> Router<S> {
    // Completions may carry a large result (kept in object storage).
    let large = Router::<JobService>::new()
        .route("/jobs/{id}/complete", post(complete))
        .layer(RequestBodyLimitLayer::new(MAX_RESULT_BYTES + 64 * 1024));
    Router::<JobService>::new()
        .route("/jobs", post(enqueue).get(list))
        .route("/jobs/queues", get(list_queues))
        .route(
            "/jobs/queues/{name}",
            get(get_queue).put(put_queue).delete(delete_queue),
        )
        .route("/jobs/queues/{name}/pause", post(pause_queue))
        .route("/jobs/queues/{name}/resume", post(resume_queue))
        .route("/jobs/workflows", get(list_workflows).post(create_workflow))
        .route("/jobs/workflows/{id}", get(get_workflow))
        .route("/jobs/workflows/{id}/cancel", post(cancel_workflow))
        .route("/jobs/{id}/progress", post(report_progress))
        .route("/jobs/{id}/checkpoint", get(get_checkpoint).post(save_checkpoint))
        .route("/jobs/{id}/result", get(get_result))
        .route("/jobs/{id}/attempts", get(list_attempts))
        .route("/jobs/{id}/children", post(spawn_children))
        .route("/jobs/stats", get(stats))
        .route("/jobs/claim", post(claim))
        .route("/jobs/schedules", get(schedules).post(create_schedule))
        .route(
            "/jobs/schedules/{id}",
            get(schedule).patch(update_schedule).delete(delete_schedule),
        )
        .route("/jobs/workers", get(workers).post(register_worker))
        .route("/jobs/{id}", get(get_job).delete(delete_job))
        .route("/jobs/{id}/events", get(events))
        .route("/jobs/{id}/cancel", post(cancel))
        .route("/jobs/{id}/retry", post(retry))
        .route("/jobs/{id}/heartbeat", post(heartbeat))
        .route("/jobs/{id}/fail", post(fail))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .merge(large)
        .with_state(service)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(service: JobService) -> ApiExtension {
    ApiExtension {
        routes: router(service),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas of the jobs API.
pub fn openapi() -> utoipa::openapi::OpenApi {
    JobsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        enqueue, list, stats, claim, schedules, create_schedule, schedule, update_schedule, delete_schedule,
        workers, register_worker, get_job, delete_job, events, cancel, retry, heartbeat, complete, fail,
        list_queues, get_queue, put_queue, delete_queue, pause_queue, resume_queue, list_workflows,
        create_workflow, get_workflow, cancel_workflow, report_progress, get_checkpoint, save_checkpoint,
        get_result, list_attempts, spawn_children,
    ),
    components(schemas(
        NewJob, Job, JobStatus, JobPage, JobEvent, EventList, JobStats, QueueStats, ClaimRequest, ClaimedJob,
        ClaimResponse, Heartbeat, LeaseBody, CompleteBody, FailBody, Schedule, NewSchedule, SchedulePatch,
        ScheduleList, Worker, NewWorker, WorkerList, Progress, ProgressReport, ProgressBody, Checkpoint,
        CheckpointBody, Attempt, AttemptList, ParentResult, JobResult, ChildrenBody, ChildList, QueueInfo,
        QueueSettings, QueueList, RatePeriod, NewWorkflow, NewStep, Compensation, FailurePolicy, Workflow,
        WorkflowStatus, WorkflowStep, WorkflowDetail, WorkflowPage, JobUpdate, WorkflowUpdate,
    )),
    tags((name = "jobs", description = "Durable background jobs: queues, schedules and the external worker protocol")),
)]
struct JobsDoc;

/// History of a job.
#[derive(Serialize, ToSchema)]
pub struct EventList {
    /// Oldest first.
    pub items: Vec<JobEvent>,
}

/// Jobs claimed by a worker.
#[derive(Serialize, ToSchema)]
pub struct ClaimResponse {
    /// Up to the requested number; empty when nothing is due.
    pub jobs: Vec<ClaimedJob>,
}

/// Schedules.
#[derive(Serialize, ToSchema)]
pub struct ScheduleList {
    /// By name.
    pub items: Vec<Schedule>,
}

/// Workers.
#[derive(Serialize, ToSchema)]
pub struct WorkerList {
    /// Most recently seen first.
    pub items: Vec<Worker>,
}

/// Body of a heartbeat.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaseBody {
    /// The lease from the claim.
    pub lease_id: String,
}

/// Body of a completion.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// Stored as the job's result.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub result: Option<Json_>,
}

/// Body of a failure report.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FailBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// What went wrong.
    pub error: String,
    /// `false` sends the job straight to the dead-letter queue (default `true`).
    #[serde(default = "yes")]
    pub retryable: bool,
}

fn yes() -> bool {
    true
}

/// Body of a progress report.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// 0 to 100.
    #[serde(default)]
    pub percent: Option<f64>,
    /// Phase name (at most 128 characters).
    #[serde(default)]
    pub stage: Option<String>,
    /// Detail (at most 2000 characters).
    #[serde(default)]
    pub message: Option<String>,
}

/// Body of a checkpoint.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBody {
    /// The lease from the claim.
    pub lease_id: String,
    /// State to resume from (JSON, at most 256 KiB).
    #[schema(value_type = Object)]
    pub checkpoint: Json_,
}

/// Body of a child-job request.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChildrenBody {
    /// The lease of the running parent.
    pub lease_id: String,
    /// The children (1 to 100).
    pub jobs: Vec<NewJob>,
}

/// Child jobs.
#[derive(Serialize, ToSchema)]
pub struct ChildList {
    /// In request order.
    pub items: Vec<Job>,
}

/// Attempts of a job.
#[derive(Serialize, ToSchema)]
pub struct AttemptList {
    /// Oldest first.
    pub items: Vec<Attempt>,
}

/// Named queues.
#[derive(Serialize, ToSchema)]
pub struct QueueList {
    /// By name.
    pub items: Vec<QueueInfo>,
}

/// The result of a succeeded job.
#[derive(Serialize, ToSchema)]
pub struct JobResult {
    /// Job id.
    pub job_id: String,
    /// It was kept in object storage.
    pub stored: bool,
    /// Size in bytes when stored.
    pub size: Option<i64>,
    /// The result (`null` when the handler returned none).
    #[schema(value_type = Option<Object>)]
    pub result: Option<Json_>,
}

fn workflow_status(q: &Query) -> Result<Option<WorkflowStatus>, ApiError> {
    match q.get("status") {
        None | Some("") => Ok(None),
        Some(text) => WorkflowStatus::parse(text).map(Some).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_parameter",
                "status must be running, succeeded, failed, compensating, compensated or cancelled",
            )
        }),
    }
}

/// Enqueues a job. With an `idempotency_key` that exists in the queue, the first job is returned (`200`).
#[utoipa::path(
    post, path = "/api/v1/jobs", tag = "jobs",
    request_body = NewJob,
    responses(
        (status = 201, description = "Enqueued", body = Job),
        (status = 200, description = "An earlier job has this idempotency key", body = Job),
    )
)]
async fn enqueue(State(s): State<JobService>, Access(ctx): Access, new: Body<NewJob>) -> ApiResult {
    let new = body(&s, &ctx, Action::EnqueueJob, new)?;
    let (job, created) = s.enqueue(&ctx, new).await.map_err(api)?;
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(job)).into_response())
}

/// Lists jobs, newest first. `status=dead` is the dead-letter queue.
#[utoipa::path(
    get, path = "/api/v1/jobs", tag = "jobs",
    params(
        ("status" = Option<String>, Query, description = "`queued`, `running`, `succeeded`, `dead` or `cancelled`"),
        ("queue" = Option<String>, Query, description = "Only this queue"),
        ("kind" = Option<String>, Query, description = "Only this handler"),
        ("limit" = Option<u32>, Query, description = "Page size, default 50, at most 200"),
        ("offset" = Option<u32>, Query, description = "Rows to skip"),
        ("workflow_id" = Option<String>, Query, description = "Only jobs of this workflow"),
        ("parent_id" = Option<String>, Query, description = "Only children of this job"),
    ),
    responses((status = 200, description = "A page of jobs", body = JobPage))
)]
async fn list(State(s): State<JobService>, Access(ctx): Access, q: Query) -> ApiResult {
    let status = match q.get("status") {
        None | Some("") => None,
        Some(text) => Some(JobStatus::parse(text).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_parameter",
                "status must be queued, running, succeeded, dead or cancelled",
            )
        })?),
    };
    let filter = JobFilter {
        status,
        queue: q.get("queue").filter(|v| !v.is_empty()).map(str::to_owned),
        kind: q.get("kind").filter(|v| !v.is_empty()).map(str::to_owned),
        limit: q.u32("limit")?.map(i64::from),
        offset: q.u32("offset")?.map(i64::from),
        workflow_id: q.get("workflow_id").filter(|v| !v.is_empty()).map(str::to_owned),
        parent_id: q.get("parent_id").filter(|v| !v.is_empty()).map(str::to_owned),
    };
    Ok(Json(s.list(&ctx, filter).await.map_err(api)?).into_response())
}

/// Counts per state and queue.
#[utoipa::path(
    get, path = "/api/v1/jobs/stats", tag = "jobs",
    responses((status = 200, description = "Queue statistics", body = JobStats))
)]
async fn stats(State(s): State<JobService>, Access(ctx): Access) -> ApiResult {
    Ok(Json(s.stats(&ctx).await.map_err(api)?).into_response())
}

/// One job.
#[utoipa::path(
    get, path = "/api/v1/jobs/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The job", body = Job))
)]
async fn get_job(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.get(&ctx, &id).await.map_err(api)?).into_response())
}

/// Deletes a finished job and its history.
#[utoipa::path(
    delete, path = "/api/v1/jobs/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 204, description = "Deleted"), (status = 409, description = "The job is not finished (`job_active`)"))
)]
async fn delete_job(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// A job's history, oldest first.
#[utoipa::path(
    get, path = "/api/v1/jobs/{id}/events", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "State changes", body = EventList))
)]
async fn events(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    let items = s.events(&ctx, &id).await.map_err(api)?;
    Ok(Json(EventList { items }).into_response())
}

/// Cancels a job: queued jobs at once, running jobs cooperatively.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/cancel", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The job (a running job shows `cancel_requested`)", body = Job), (status = 409, description = "Already finished (`job_finished`)"))
)]
async fn cancel(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.cancel(&ctx, &id).await.map_err(api)?).into_response())
}

/// Moves a dead or cancelled job back to the queue with a fresh attempt count.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/retry", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The queued job", body = Job), (status = 409, description = "Not dead or cancelled (`job_not_retryable`)"))
)]
async fn retry(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.retry(&ctx, &id).await.map_err(api)?).into_response())
}

/// Claims due jobs for an external worker (highest priority first). Each job
/// comes with a `lease_id`; send heartbeats before `visibility_timeout_ms`
/// passes or the job is handed to another worker.
#[utoipa::path(
    post, path = "/api/v1/jobs/claim", tag = "jobs",
    request_body = ClaimRequest,
    responses((status = 200, description = "Claimed jobs (possibly none)", body = ClaimResponse))
)]
async fn claim(State(s): State<JobService>, Access(ctx): Access, req: Body<ClaimRequest>) -> ApiResult {
    let req = body(&s, &ctx, Action::WorkJobs, req)?;
    let jobs = s.claim(&ctx, req).await.map_err(api)?;
    Ok(Json(ClaimResponse { jobs }).into_response())
}

/// Extends the lease. `cancel_requested` tells the worker to stop and `fail`.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/heartbeat", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    request_body = LeaseBody,
    responses((status = 200, description = "New expiry", body = Heartbeat), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn heartbeat(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<LeaseBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    Ok(Json(s.heartbeat(&ctx, &id, &b.lease_id).await.map_err(api)?).into_response())
}

/// Reports success. Exactly one completion is accepted per lease.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/complete", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    request_body = CompleteBody,
    responses((status = 200, description = "The succeeded job", body = Job), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn complete(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<CompleteBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    Ok(Json(s.complete(&ctx, &id, &b.lease_id, b.result).await.map_err(api)?).into_response())
}

/// Reports a failed attempt: retried after a backoff, or dead-lettered once attempts run out.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/fail", tag = "jobs",
    params(("id" = String, Path, description = "Job id")),
    request_body = FailBody,
    responses((status = 200, description = "The job: `queued` (retry), `dead` or `cancelled`", body = Job), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn fail(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<FailBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    Ok(Json(
        s.fail(&ctx, &id, &b.lease_id, &b.error, b.retryable)
            .await
            .map_err(api)?,
    )
    .into_response())
}

/// Lists schedules.
#[utoipa::path(
    get, path = "/api/v1/jobs/schedules", tag = "jobs",
    responses((status = 200, description = "Schedules by name", body = ScheduleList))
)]
async fn schedules(State(s): State<JobService>, Access(ctx): Access) -> ApiResult {
    let items = s.schedules(&ctx).await.map_err(api)?;
    Ok(Json(ScheduleList { items }).into_response())
}

/// Creates a recurring job (cron, UTC).
#[utoipa::path(
    post, path = "/api/v1/jobs/schedules", tag = "jobs",
    request_body = NewSchedule,
    responses((status = 201, description = "Created", body = Schedule), (status = 409, description = "Name taken (`schedule_exists`)"))
)]
async fn create_schedule(State(s): State<JobService>, Access(ctx): Access, new: Body<NewSchedule>) -> ApiResult {
    let new = body(&s, &ctx, Action::ManageJobs, new)?;
    let schedule = s.create_schedule(&ctx, new).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(schedule)).into_response())
}

/// One schedule.
#[utoipa::path(
    get, path = "/api/v1/jobs/schedules/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Schedule id")),
    responses((status = 200, description = "The schedule", body = Schedule))
)]
async fn schedule(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.schedule(&ctx, &id).await.map_err(api)?).into_response())
}

/// Changes a schedule (cron, enabled, payload, priority).
#[utoipa::path(
    patch, path = "/api/v1/jobs/schedules/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Schedule id")),
    request_body = SchedulePatch,
    responses((status = 200, description = "The updated schedule", body = Schedule))
)]
async fn update_schedule(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    patch: Body<SchedulePatch>,
) -> ApiResult {
    let patch = body(&s, &ctx, Action::ManageJobs, patch)?;
    Ok(Json(s.update_schedule(&ctx, &id, patch).await.map_err(api)?).into_response())
}

/// Deletes a schedule; jobs it already enqueued stay.
#[utoipa::path(
    delete, path = "/api/v1/jobs/schedules/{id}", tag = "jobs",
    params(("id" = String, Path, description = "Schedule id")),
    responses((status = 204, description = "Deleted"))
)]
async fn delete_schedule(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_schedule(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Lists workers that registered or claimed jobs.
#[utoipa::path(
    get, path = "/api/v1/jobs/workers", tag = "jobs",
    responses((status = 200, description = "Workers, most recently seen first", body = WorkerList))
)]
async fn workers(State(s): State<JobService>, Access(ctx): Access) -> ApiResult {
    let items = s.workers(&ctx).await.map_err(api)?;
    Ok(Json(WorkerList { items }).into_response())
}

/// Registers (or updates) a worker.
#[utoipa::path(
    post, path = "/api/v1/jobs/workers", tag = "jobs",
    request_body = NewWorker,
    responses((status = 201, description = "Registered", body = Worker))
)]
async fn register_worker(State(s): State<JobService>, Access(ctx): Access, new: Body<NewWorker>) -> ApiResult {
    let new = body(&s, &ctx, Action::WorkJobs, new)?;
    let worker = s.register_worker(&ctx, new).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(worker)).into_response())
}

/// Lists named queues: settings and counts.
#[utoipa::path(
    get, path = "/api/v1/jobs/queues", tag = "jobs", operation_id = "jobs_list_queues",
    responses((status = 200, description = "Queues by name", body = QueueList))
)]
async fn list_queues(State(s): State<JobService>, Access(ctx): Access) -> ApiResult {
    let items = s.queues(&ctx).await.map_err(api)?;
    Ok(Json(QueueList { items }).into_response())
}

/// One queue: settings and counts.
#[utoipa::path(
    get, path = "/api/v1/jobs/queues/{name}", tag = "jobs", operation_id = "jobs_get_queue",
    params(("name" = String, Path, description = "Queue name")),
    responses((status = 200, description = "The queue", body = QueueInfo))
)]
async fn get_queue(State(s): State<JobService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(s.queue(&ctx, &name).await.map_err(api)?).into_response())
}

/// Saves a queue's settings: concurrency limit, rate limit per second or
/// minute, pause, default priority and timeout. Limits left out are removed.
#[utoipa::path(
    put, path = "/api/v1/jobs/queues/{name}", tag = "jobs", operation_id = "jobs_put_queue",
    params(("name" = String, Path, description = "Queue name")),
    request_body = QueueSettings,
    responses((status = 200, description = "The queue", body = QueueInfo))
)]
async fn put_queue(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    b: Body<QueueSettings>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::ManageJobs, b)?;
    Ok(Json(s.set_queue(&ctx, &name, b).await.map_err(api)?).into_response())
}

/// Removes a queue's settings (its jobs stay).
#[utoipa::path(
    delete, path = "/api/v1/jobs/queues/{name}", tag = "jobs", operation_id = "jobs_delete_queue",
    params(("name" = String, Path, description = "Queue name")),
    responses((status = 204, description = "Settings removed"))
)]
async fn delete_queue(State(s): State<JobService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    s.delete_queue(&ctx, &name).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Pauses a queue: no worker gets its jobs until it is resumed.
#[utoipa::path(
    post, path = "/api/v1/jobs/queues/{name}/pause", tag = "jobs", operation_id = "jobs_pause_queue",
    params(("name" = String, Path, description = "Queue name")),
    responses((status = 200, description = "The paused queue", body = QueueInfo))
)]
async fn pause_queue(State(s): State<JobService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(s.pause_queue(&ctx, &name).await.map_err(api)?).into_response())
}

/// Resumes a paused queue.
#[utoipa::path(
    post, path = "/api/v1/jobs/queues/{name}/resume", tag = "jobs", operation_id = "jobs_resume_queue",
    params(("name" = String, Path, description = "Queue name")),
    responses((status = 200, description = "The queue", body = QueueInfo))
)]
async fn resume_queue(State(s): State<JobService>, Access(ctx): Access, Path(name): Path<String>) -> ApiResult {
    Ok(Json(s.resume_queue(&ctx, &name).await.map_err(api)?).into_response())
}

/// Lists workflows, newest first.
#[utoipa::path(
    get, path = "/api/v1/jobs/workflows", tag = "jobs", operation_id = "jobs_list_workflows",
    params(
        ("status" = Option<String>, Query, description = "`running`, `succeeded`, `failed`, `compensating`, `compensated` or `cancelled`"),
        ("limit" = Option<u32>, Query, description = "Page size, default 50, at most 200"),
        ("offset" = Option<u32>, Query, description = "Rows to skip"),
    ),
    responses((status = 200, description = "A page of workflows", body = WorkflowPage))
)]
async fn list_workflows(State(s): State<JobService>, Access(ctx): Access, q: Query) -> ApiResult {
    let status = workflow_status(&q)?;
    let page = s
        .workflows(
            &ctx,
            status,
            q.u32("limit")?.map(i64::from),
            q.u32("offset")?.map(i64::from),
        )
        .await
        .map_err(api)?;
    Ok(Json(page).into_response())
}

/// Creates a workflow: chains, fan-out and fan-in of steps (a step waits for
/// every step in its `depends_on`), with `on_failure` `fail` or `compensate`.
#[utoipa::path(
    post, path = "/api/v1/jobs/workflows", tag = "jobs", operation_id = "jobs_create_workflow",
    request_body = NewWorkflow,
    responses((status = 201, description = "Created", body = WorkflowDetail))
)]
async fn create_workflow(State(s): State<JobService>, Access(ctx): Access, b: Body<NewWorkflow>) -> ApiResult {
    let b = body(&s, &ctx, Action::EnqueueJob, b)?;
    let wf = s.create_workflow(&ctx, b).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(wf)).into_response())
}

/// A workflow with its jobs (the graph), their state and timings.
#[utoipa::path(
    get, path = "/api/v1/jobs/workflows/{id}", tag = "jobs", operation_id = "jobs_get_workflow",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, description = "The workflow", body = WorkflowDetail))
)]
async fn get_workflow(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.workflow(&ctx, &id).await.map_err(api)?).into_response())
}

/// Cancels a running workflow (no compensations run).
#[utoipa::path(
    post, path = "/api/v1/jobs/workflows/{id}/cancel", tag = "jobs", operation_id = "jobs_cancel_workflow",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, description = "The cancelled workflow", body = WorkflowDetail), (status = 409, description = "Already finished (`workflow_finished`)"))
)]
async fn cancel_workflow(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.cancel_workflow(&ctx, &id).await.map_err(api)?).into_response())
}

/// Reports progress of a running job (stored, streamed on the realtime
/// channel `jobs:<id>`); counts as a heartbeat.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/progress", tag = "jobs", operation_id = "jobs_report_progress",
    params(("id" = String, Path, description = "Job id")),
    request_body = ProgressBody,
    responses((status = 200, description = "New lease expiry", body = Heartbeat), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn report_progress(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<ProgressBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    let report = ProgressReport {
        percent: b.percent,
        stage: b.stage,
        message: b.message,
    };
    Ok(Json(s.progress(&ctx, &id, &b.lease_id, report).await.map_err(api)?).into_response())
}

/// The last checkpoint of a job.
#[utoipa::path(
    get, path = "/api/v1/jobs/{id}/checkpoint", tag = "jobs", operation_id = "jobs_get_checkpoint",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The checkpoint", body = Checkpoint))
)]
async fn get_checkpoint(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok(Json(s.checkpoint(&ctx, &id).await.map_err(api)?).into_response())
}

/// Saves a checkpoint of a running job; the next attempt resumes from it.
/// Counts as a heartbeat.
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/checkpoint", tag = "jobs", operation_id = "jobs_save_checkpoint",
    params(("id" = String, Path, description = "Job id")),
    request_body = CheckpointBody,
    responses((status = 200, description = "New lease expiry", body = Heartbeat), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn save_checkpoint(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<CheckpointBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    Ok(Json(
        s.save_checkpoint(&ctx, &id, &b.lease_id, &b.checkpoint)
            .await
            .map_err(api)?,
    )
    .into_response())
}

/// The result of a succeeded job, also when it is kept in object storage.
#[utoipa::path(
    get, path = "/api/v1/jobs/{id}/result", tag = "jobs", operation_id = "jobs_get_result",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "The result", body = JobResult), (status = 409, description = "Not succeeded (`job_not_succeeded`)"))
)]
async fn get_result(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    let result = s.result(&ctx, &id).await.map_err(api)?;
    let job = s.get(&ctx, &id).await.map_err(api)?;
    Ok(Json(JobResult {
        job_id: id,
        stored: job.result_stored,
        size: job.result_size,
        result,
    })
    .into_response())
}

/// The attempts of a job with their timings.
#[utoipa::path(
    get, path = "/api/v1/jobs/{id}/attempts", tag = "jobs", operation_id = "jobs_list_attempts",
    params(("id" = String, Path, description = "Job id")),
    responses((status = 200, description = "Attempts, oldest first", body = AttemptList))
)]
async fn list_attempts(State(s): State<JobService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    let items = s.attempts(&ctx, &id).await.map_err(api)?;
    Ok(Json(AttemptList { items }).into_response())
}

/// Enqueues child jobs of a running job (they join its workflow and are
/// cancelled with it).
#[utoipa::path(
    post, path = "/api/v1/jobs/{id}/children", tag = "jobs", operation_id = "jobs_spawn_children",
    params(("id" = String, Path, description = "Job id of the running parent")),
    request_body = ChildrenBody,
    responses((status = 201, description = "Enqueued", body = ChildList), (status = 409, description = "Lease lost (`lease_lost`)"))
)]
async fn spawn_children(
    State(s): State<JobService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<ChildrenBody>,
) -> ApiResult {
    let b = body(&s, &ctx, Action::WorkJobs, b)?;
    let items = s.spawn_children(&ctx, &id, &b.lease_id, b.jobs).await.map_err(api)?;
    Ok((StatusCode::CREATED, Json(ChildList { items })).into_response())
}
