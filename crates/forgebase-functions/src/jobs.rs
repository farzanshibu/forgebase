//! Scheduled and background functions through the durable job queue (FB-61).
//!
//! The job kind [`INVOKE_JOB`] runs a function with the job's payload
//! `{"function": "<name>", "input": <any JSON>}`; the function's JSON output
//! becomes the job result. A job schedule (cron) of this kind is a scheduled
//! function; enqueueing one is a background task or queue consumer. Retries,
//! backoff and the dead-letter queue are the job queue's: a timeout or busy
//! engine is retried, a missing function or a function fault is not.
//!
//! **Invocation context (FB-142, `rls-coverage.md` G4, G5).** The kind is
//! guarded: a non-privileged caller may enqueue it (or schedule it, or put it
//! in a workflow) only if it may invoke the function over HTTP (its access
//! mode: `public`, `authenticated`, or `permission` = `engine:invoke_function`),
//! checked when the job is stored. The job then runs **as the recorded
//! enqueuer** ([`forgebase_jobs::JobContext::enqueuer`]): JavaScript `ctx`
//! services act as that caller, so collection rules, tenancy and policies
//! apply as if it had invoked the function itself. Before the run, the
//! enqueuer must still be allowed to invoke the function and to enqueue jobs
//! (`engine:enqueue_job` or `engine:manage_jobs`, which also re-validates its
//! tenant), and a user must still exist; otherwise the job fails for good.
//! Privileged enqueuers (administrators, admin service accounts) and the
//! system do not lend their privileges to function code: the job runs as the
//! function's own identity `service:function.<name>` in the recorded realm,
//! organization and tenant (the job's tenant for system jobs). Only a
//! `service_role` version reaches privileged services (`ctx.admin`).

use forgebase_data::{AccessContext, Action, DataError, GetOptions, Principal};
use forgebase_jobs::{HandlerError, JobContext, JobError, JobRegistry};
use serde_json::{Value as Json, json};

use crate::error::{ExecError, FunctionError};
use crate::model::Trigger;
use crate::model::Outcome;
use crate::{FunctionService, identity};

/// Job kind that invokes a function.
pub const INVOKE_JOB: &str = "function.invoke";

/// The `function` of a payload.
fn function_of(payload: &Json) -> Option<&str> {
    payload.get("function").and_then(Json::as_str)
}

/// The context a hook or job runs a function with when it acts for
/// `enqueuer` (FB-142): the enqueuer itself, unless it is privileged (or
/// `None`, the system); then the function's own identity in the enqueuer's
/// realm, organization and tenant (`tenant` when the enqueuer has none).
pub(crate) fn invocation_context(function: &str, enqueuer: Option<&AccessContext>, tenant: Option<&str>) -> AccessContext {
    match enqueuer {
        Some(e) if !e.is_privileged() => e.clone(),
        Some(e) => {
            let mut ctx = identity(function).with_realm(e.realm_id.clone());
            ctx.org_id.clone_from(&e.org_id);
            ctx.tenant = e.tenant.clone().or_else(|| tenant.map(str::to_owned));
            ctx.request_id.clone_from(&e.request_id);
            ctx
        }
        None => {
            let ctx = identity(function);
            match tenant {
                Some(t) => ctx.with_tenant(t),
                None => ctx,
            }
        }
    }
}

impl FunctionService {
    /// Runs the active version of `name` with `input` as `trigger`, with
    /// JavaScript `ctx` services acting as `caller` (FB-142: hooks and jobs
    /// pass an explicit context). No access check: callers decide `caller`
    /// (see [`invocation_context`]).
    pub async fn invoke_as(
        &self,
        name: &str,
        trigger: Trigger,
        input: &Json,
        caller: AccessContext,
    ) -> crate::error::Result<Outcome> {
        let e = self.entry(name)?;
        let request_id = caller.request_id.clone();
        self.execute(
            &e,
            None,
            trigger,
            input.to_string().into_bytes(),
            request_id,
            Some(caller),
            |_| None,
        )
        .await
    }

    /// Whether `ctx` may have `name` invoked for it by a job: the HTTP
    /// invoke check. Unknown and forbidden functions look alike.
    fn admit_invoke(&self, ctx: &AccessContext, name: &str) -> Result<(), JobError> {
        self.http_entry(ctx, name).map(|_| ()).map_err(|e| match e {
            FunctionError::Data(DataError::Unauthenticated) => JobError::Access(DataError::Unauthenticated),
            _ => JobError::Access(DataError::Forbidden(format!(
                "you may not invoke function {name:?} (or it does not exist)"
            ))),
        })
    }

    /// The enqueuer of a job, checked again before it runs; `Err` = a
    /// permanent failure.
    async fn revalidate(&self, mut ctx: AccessContext, name: &str) -> Result<AccessContext, HandlerError> {
        self.admit_invoke(&ctx, name)
            .map_err(|e| HandlerError::fatal(format!("the enqueuer may no longer invoke {name:?}: {e}")))?;
        let may_enqueue = [Action::EnqueueJob, Action::ManageJobs]
            .into_iter()
            .any(|a| self.authorize(&ctx, a).is_ok());
        if !may_enqueue {
            return Err(HandlerError::fatal(
                "the enqueuer may no longer enqueue jobs in its tenant",
            ));
        }
        if let Principal::User { collection, id } = &ctx.principal {
            // `@request.auth.<field>` in rules: the user's record, as the
            // tenancy layer resolves it for requests.
            let mut rec = self
                .inner
                .data
                .get_record(&AccessContext::system(), collection, id, &GetOptions::default())
                .await
                .map_err(|_| HandlerError::fatal("the enqueuer no longer exists"))?;
            rec.remove("token_key");
            rec.remove("_tenant");
            ctx.attributes = rec;
        }
        Ok(ctx)
    }
}

/// Registers the [`INVOKE_JOB`] handler (a guarded kind, see the module docs).
pub fn register_jobs(registry: JobRegistry, svc: FunctionService) -> JobRegistry {
    let guard = svc.clone();
    registry.register_guarded(
        INVOKE_JOB,
        move |ctx: JobContext| {
            let svc = svc.clone();
            async move {
                let name = function_of(ctx.payload())
                    .ok_or_else(|| HandlerError::fatal("payload needs \"function\""))?
                    .to_owned();
                let enqueuer = ctx.enqueuer();
                let caller = match enqueuer.filter(|e| !e.is_privileged()) {
                    Some(e) => svc.revalidate(e, &name).await?,
                    None => invocation_context(&name, ctx.enqueuer().as_ref(), ctx.job().tenant.as_deref()),
                };
                let input = json!({
                    "trigger": "job",
                    "job_id": ctx.job().id,
                    "attempt": ctx.attempt(),
                    "input": ctx.payload().get("input").cloned().unwrap_or(Json::Null),
                });
                match svc.invoke_as(&name, Trigger::Job, &input, caller).await {
                    Ok(out) => Ok(out
                        .json()
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| json!({"version": out.version}))),
                    Err(FunctionError::NotFound(n)) => Err(HandlerError::fatal(format!("no function {n:?}"))),
                    Err(e @ (FunctionError::Busy(_) | FunctionError::Execution(ExecError::Timeout(_)))) => {
                        Err(HandlerError::retry(e.to_string()))
                    }
                    Err(e @ FunctionError::Execution(_)) => Err(HandlerError::fatal(e.to_string())),
                    Err(e) => Err(HandlerError::retry(e.to_string())),
                }
            }
        },
        move |ctx: &AccessContext, payload: &Json| {
            let name = function_of(payload).ok_or_else(|| JobError::Invalid {
                code: "invalid_job",
                message: "a function.invoke payload needs \"function\"".into(),
            })?;
            guard.admit_invoke(ctx, name)
        },
    )
}
