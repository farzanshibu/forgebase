//! Restore drills (ADR-0041): the result type, the history, the metrics and
//! the way the server runs one.
//!
//! A drill proves that a backup restores. `forgebase backup drill` does the
//! work (restore into a scratch directory, start a quiescent server on it,
//! check the data); this module holds what the CLI, the REST API and the
//! scheduler share:
//!
//! - [`DrillReport`] is the result, with the measured RTO and RPO.
//! - [`DrillStore`] is the history, one JSON file per drill under
//!   `<data_dir>/backups/drills/` (no migration; it works on both backends).
//! - [`publish_metrics`] sets `forgebase_backup_last_drill_success_timestamp_seconds`
//!   and the duration gauges, read from the history on every scrape.
//! - [`DrillRunner`] and [`register_jobs`] run the job `system.backup_drill`
//!   in a child process of the server's own executable, so a drill never
//!   shares memory, metrics or signal handlers with the live server.
//! - [`sync_schedule`] keeps the optional `[backup] drill_schedule`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use forgebase_jobs::{CancelToken, HandlerError, JobContext, JobRegistry, JobService, NewJob, NewSchedule};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{BackupError, Result, io};

/// Job kind that runs a drill (reserved: only the system and administrators enqueue it).
pub const DRILL_JOB: &str = "system.backup_drill";
/// Name of the schedule `[backup] drill_schedule` creates.
pub const DRILL_SCHEDULE: &str = "backup-drill";
/// Drills kept in the history.
pub const KEEP_DRILLS: usize = 100;
/// Longest a drill job may run.
const DRILL_TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// Verdict of one check of a drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// Passed.
    Ok,
    /// Passed with something worth knowing.
    Warn,
    /// Failed: the drill fails.
    Fail,
    /// Not applicable to this backup (the detail says why).
    Skip,
}

/// One check of a drill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DrillCheck {
    /// Short name: `verify`, `restore`, `server`, `doctor`, `schema`, `counts`, ...
    pub name: String,
    /// Verdict.
    pub status: CheckStatus,
    /// What was found.
    pub detail: String,
}

/// Overall verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DrillOutcome {
    /// The backup restored and every check passed.
    Passed,
    /// A check failed, or the backup could not be restored or tested.
    Failed,
}

/// Wall time of the phases, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DrillPhases {
    /// Verifying the backup and restoring it into the scratch directory.
    pub restore_ms: u64,
    /// Starting the quiescent server on the restored data (migrations,
    /// PostgreSQL crash recovery, listener).
    pub start_ms: u64,
    /// The checks.
    pub checks_ms: u64,
}

/// What the checks looked at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DrillCounts {
    /// Collections counted.
    pub collections: u64,
    /// Records in them.
    pub records: u64,
    /// Records read back through the data service.
    pub records_sampled: u64,
    /// Storage objects in the restored database.
    pub storage_objects: u64,
    /// Blob files read and hashed.
    pub blobs_sampled: u64,
}

/// The result of one restore drill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DrillReport {
    /// Drill id (its UTC start time).
    pub id: String,
    /// The backup that was restored.
    pub backup_id: String,
    /// When the backup was taken (RFC 3339).
    pub backup_created: String,
    /// `sqlite`, `postgres 18.3 (managed)`.
    pub engine: String,
    /// Schema version of the backup.
    pub schema_version: i64,
    /// Start of the drill (RFC 3339).
    pub started_at: String,
    /// End of the drill (RFC 3339).
    pub finished_at: String,
    /// Verdict.
    pub outcome: DrillOutcome,
    /// Recovery time: restore, start and checks, as wall time in seconds.
    pub rto_seconds: f64,
    /// Recovery point: age of the backup when the drill started, in seconds.
    /// Everything written since the backup is what a restore would lose.
    pub rpo_seconds: i64,
    /// Time per phase.
    pub phases: DrillPhases,
    /// What was counted and sampled.
    pub counts: DrillCounts,
    /// Every check, in order.
    pub checks: Vec<DrillCheck>,
    /// The failed checks, as messages.
    pub problems: Vec<String>,
    /// Who ran it: `cli`, `api` or `schedule`.
    pub trigger: String,
    /// The scratch directory, when it was kept (`--keep`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<String>,
}

impl DrillReport {
    /// Whether the drill passed.
    pub fn passed(&self) -> bool {
        self.outcome == DrillOutcome::Passed
    }

    /// Unix time the drill finished, `0` when unreadable.
    pub fn finished_unix(&self) -> i64 {
        OffsetDateTime::parse(&self.finished_at, &Rfc3339).map_or(0, OffsetDateTime::unix_timestamp)
    }
}

/// The drill history of one data directory.
#[derive(Debug, Clone)]
pub struct DrillStore {
    dir: PathBuf,
}

impl DrillStore {
    /// The history of `layout`: `<data_dir>/backups/drills/`.
    pub fn new(layout: &DataLayout) -> Self {
        Self {
            dir: layout.backups_dir().join("drills"),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `report` atomically and trims the history to [`KEEP_DRILLS`].
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn record(&self, report: &DrillReport) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;
        let name = format!("{}.json", report.id);
        let json = serde_json::to_vec_pretty(report).map_err(|e| BackupError::Internal(e.to_string()))?;
        crate::fsutil::write_atomic(&self.dir.join(name), &json)?;
        let all = self.list();
        for old in all.iter().skip(KEEP_DRILLS) {
            let _ = std::fs::remove_file(self.dir.join(format!("{}.json", old.id)));
        }
        Ok(())
    }

    /// Every recorded drill, newest first. A file that does not parse is skipped.
    pub fn list(&self) -> Vec<DrillReport> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<DrillReport> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| b.id.cmp(&a.id)));
        out
    }

    /// The drills of backup `backup_id`, newest first.
    pub fn of_backup(&self, backup_id: &str) -> Vec<DrillReport> {
        self.list().into_iter().filter(|r| r.backup_id == backup_id).collect()
    }
}

/// Sets the drill gauges from the history: the Unix time of the last drill
/// that passed (`0` when none did), whether the latest passed, and the
/// latest drill's measured recovery time per phase and backup age. Nothing
/// is set until a first drill is on record.
pub fn publish_metrics(store: &DrillStore) {
    let all = store.list();
    let Some(latest) = all.first() else {
        return;
    };
    let last_ok = all.iter().find(|r| r.passed()).map_or(0, DrillReport::finished_unix);
    metrics::gauge!("forgebase_backup_last_drill_success_timestamp_seconds").set(last_ok as f64);
    metrics::gauge!("forgebase_backup_last_drill_ok").set(if latest.passed() { 1.0 } else { 0.0 });
    metrics::gauge!("forgebase_backup_drill_rpo_seconds").set(latest.rpo_seconds as f64);
    let p = latest.phases;
    for (phase, secs) in [
        ("total", latest.rto_seconds),
        ("restore", p.restore_ms as f64 / 1000.0),
        ("start", p.start_ms as f64 / 1000.0),
        ("checks", p.checks_ms as f64 / 1000.0),
    ] {
        metrics::gauge!("forgebase_backup_drill_duration_seconds", "phase" => phase).set(secs);
    }
}

/// How the server runs a drill: its own executable, `backup drill`.
#[derive(Debug, Clone)]
pub struct DrillRunner {
    exe: PathBuf,
    /// Global flags placed before the subcommand (`--config`, `--data-dir`).
    globals: Vec<String>,
}

impl DrillRunner {
    /// A runner for the running executable, pointed at `data_dir` and, when
    /// the server loaded one, the config file. `None` when the executable
    /// cannot be found, or is not the `forgebase` command (an embedding
    /// program or a test binary has no `backup drill`).
    pub fn for_current_exe(data_dir: &Path, config_file: Option<&Path>) -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let stem = exe.file_stem()?.to_string_lossy().to_ascii_lowercase();
        // A test harness (`target/debug/deps/forgebase-<hash>`) is not the command.
        let in_deps = exe.parent().and_then(Path::file_name).is_some_and(|n| n == "deps");
        if stem != "forgebase" || in_deps {
            return None;
        }
        Some(Self::new(exe, data_dir, config_file))
    }

    /// A runner for `exe` (a `forgebase` command).
    pub fn new(exe: PathBuf, data_dir: &Path, config_file: Option<&Path>) -> Self {
        let mut globals = Vec::new();
        if let Some(c) = config_file {
            globals.push("--config".to_owned());
            globals.push(c.display().to_string());
        }
        globals.push("--data-dir".to_owned());
        globals.push(data_dir.display().to_string());
        Self { exe, globals }
    }

    /// Runs a drill of `backup_id` and returns its report. The child records
    /// the drill in the history itself. A drill that finds a problem is a
    /// report with outcome `failed`, not an error.
    ///
    /// # Errors
    /// The command could not be started, was cancelled, or printed no report.
    pub async fn run(
        &self,
        backup_id: &str,
        trigger: &str,
        sample: Option<u32>,
        cancel: &CancelToken,
    ) -> Result<DrillReport> {
        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.args(&self.globals)
            .args(["backup", "drill", backup_id, "--json", "--trigger", trigger])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(n) = sample {
            cmd.arg("--sample").arg(n.to_string());
        }
        let child = cmd
            .spawn()
            .map_err(|e| BackupError::Internal(format!("cannot start {}: {e}", self.exe.display())))?;
        let out = tokio::select! {
            out = child.wait_with_output() => out.map_err(|e| BackupError::Internal(format!("drill process failed: {e}")))?,
            () = cancel.cancelled() => {
                // `kill_on_drop` stops the child; its scratch directory is
                // swept by the next drill's cleanup of stale ones.
                return Err(BackupError::Internal("the drill was cancelled".into()));
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        match serde_json::from_str::<DrillReport>(stdout.trim()) {
            Ok(report) => Ok(report),
            Err(_) => {
                let err = String::from_utf8_lossy(&out.stderr);
                let tail: String = err
                    .lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("; ");
                Err(BackupError::Internal(format!(
                    "the drill printed no report ({}): {tail}",
                    out.status
                )))
            }
        }
    }
}

/// Registers the `system.backup_drill` job: runs a drill with `runner` and
/// returns the report as the job's result. A failed drill is a *finished*
/// job whose result says `failed` (retrying would only repeat it); the
/// metrics and the history carry the verdict.
pub fn register_jobs(registry: JobRegistry, runner: DrillRunner, store: DrillStore) -> JobRegistry {
    // One drill at a time: each needs the disk of a whole restore.
    let one_at_a_time = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    registry.register(DRILL_JOB, move |ctx: JobContext| {
        let runner = runner.clone();
        let store = store.clone();
        let one_at_a_time = one_at_a_time.clone();
        async move {
            let _turn = one_at_a_time
                .acquire_owned()
                .await
                .map_err(|e| HandlerError::fatal(e.to_string()))?;
            let payload = ctx.payload().clone();
            let backup = payload
                .get("backup_id")
                .and_then(|v| v.as_str())
                .unwrap_or("latest")
                .to_owned();
            let trigger = payload
                .get("trigger")
                .and_then(|v| v.as_str())
                .unwrap_or("api")
                .to_owned();
            let sample = payload
                .get("sample")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok());
            let report = runner
                .run(&backup, &trigger, sample, &ctx.cancel_token())
                .await
                .map_err(|e| HandlerError::fatal(e.to_string()))?;
            let outcome = if report.passed() { "passed" } else { "failed" };
            metrics::counter!("forgebase_backup_drills_total", "outcome" => outcome).increment(1);
            publish_metrics(&store);
            serde_json::to_value(&report).map_err(|e| HandlerError::fatal(e.to_string()))
        }
    })
}

/// Enqueues a drill of `backup_id` (an id or `latest`).
///
/// # Errors
/// The job cannot be stored.
pub async fn enqueue(jobs: &JobService, backup_id: &str, trigger: &str, sample: Option<u32>) -> Result<String> {
    let mut payload = serde_json::json!({ "backup_id": backup_id, "trigger": trigger });
    if let Some(n) = sample {
        payload["sample"] = n.into();
    }
    let job = NewJob::new(DRILL_JOB, payload)
        .retries(1, Duration::from_secs(1), Duration::from_secs(1))
        .timeout(DRILL_TIMEOUT);
    jobs.enqueue(&AccessContext::system(), job)
        .await
        .map(|(job, _)| job.id)
        .map_err(|e| BackupError::Internal(format!("the drill job could not be queued: {e}")))
}

/// Whether a drill job is queued or running.
pub async fn active(jobs: &JobService) -> bool {
    use forgebase_jobs::{JobFilter, JobStatus};
    for status in [JobStatus::Queued, JobStatus::Running] {
        let filter = JobFilter {
            status: Some(status),
            kind: Some(DRILL_JOB.to_owned()),
            limit: Some(1),
            ..JobFilter::default()
        };
        if jobs
            .list(&AccessContext::system(), filter)
            .await
            .is_ok_and(|p| p.total > 0)
        {
            return true;
        }
    }
    false
}

/// Makes the schedule `backup-drill` match `[backup] drill_schedule`: created
/// or updated when a cron expression is set, disabled when it is not.
/// A schedule is an ordinary job schedule: `GET /api/v1/jobs/schedules`
/// lists it.
pub async fn sync_schedule(jobs: &JobService, cron: Option<&str>) {
    let sys = AccessContext::system();
    let existing = match jobs.schedules(&sys).await {
        Ok(list) => list.into_iter().find(|s| s.name == DRILL_SCHEDULE),
        Err(e) => {
            tracing::warn!(error = %e, "drill schedule not synced");
            return;
        }
    };
    let payload = serde_json::json!({ "backup_id": "latest", "trigger": "schedule" });
    match (cron, existing) {
        (Some(expr), None) => {
            let new = NewSchedule {
                name: DRILL_SCHEDULE.to_owned(),
                cron: expr.to_owned(),
                kind: DRILL_JOB.to_owned(),
                queue: None,
                payload,
                priority: Some(-1),
                max_attempts: Some(1),
                backoff_base_ms: None,
                backoff_max_ms: None,
                visibility_timeout_ms: None,
                enabled: Some(true),
            };
            if let Err(e) = jobs.create_schedule(&sys, new).await {
                tracing::warn!(error = %e, cron = expr, "[backup] drill_schedule is not a valid schedule; no drill is scheduled");
            }
        }
        (Some(expr), Some(s)) => {
            if s.cron != expr || !s.enabled {
                let patch = forgebase_jobs::SchedulePatch {
                    cron: Some(expr.to_owned()),
                    enabled: Some(true),
                    ..forgebase_jobs::SchedulePatch::default()
                };
                if let Err(e) = jobs.update_schedule(&sys, &s.id, patch).await {
                    tracing::warn!(error = %e, cron = expr, "[backup] drill_schedule is not a valid schedule; the old one stays");
                }
            }
        }
        (None, Some(s)) if s.enabled => {
            let patch = forgebase_jobs::SchedulePatch {
                enabled: Some(false),
                ..forgebase_jobs::SchedulePatch::default()
            };
            let _ = jobs.update_schedule(&sys, &s.id, patch).await;
        }
        (None, _) => {}
    }
}
