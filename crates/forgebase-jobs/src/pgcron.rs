//! Schedules on pg_cron (FB-165, ADR-0029 §1.7).
//!
//! On a PostgreSQL whose pg_cron the extension layer reports active
//! (`Database::feature_active(Feature::Cron)`), a schedule whose expression
//! pg_cron reads exactly like ForgeBase's parser ([`map_expression`]) is fired
//! by pg_cron: a job `fbsched_<id>` in `cron.job` calls
//! `forgebase.fire_schedule('<id>')` (migration 0038), which inserts the job
//! as [`JobService`]'s own tick does and runs `NOTIFY forgebase_jobs`. The
//! server listens on that channel and wakes its workers at once.
//!
//! Everything else stays on the Rust tick: SQLite, PostgreSQL without an
//! active pg_cron, and expressions pg_cron cannot express identically.
//!
//! **Who fires a schedule.** A pg_cron-owned schedule keeps the sentinel
//! [`OWNED`] in `next_run_at`, so the Rust tick never sees it as due and
//! never races pg_cron; reads compute the real next firing from the
//! expression and report `fired_by = "pg_cron"`. Taking a schedule over and
//! handing it back are done by [`JobService::pgcron_reconcile`] in one
//! transaction together with the `cron.job` change.
//!
//! **Catch-up.** pg_cron does not run firings it missed. When the server
//! attaches ([`attach_pgcron`]), a schedule with a due time since its last
//! firing is fired once (the missed firings collapse into one, ADR-0007 §6),
//! with the first missed due time in the idempotency key.
//!
//! **Fallback.** [`attach_pgcron`] checks the layout it relies on
//! (`forgebase.fire_schedule(text, bigint)` and the columns of `cron.job`);
//! a mismatch logs a warning and leaves every schedule on the Rust tick.
//! When pg_cron is installed but no longer active (mode `off`, preload
//! removed), its jobs are removed and the schedules go back to the tick.

use std::collections::BTreeMap;
use std::time::Duration;

use forgebase_db::{Database, Feature, Statement};
use tokio::task::AbortHandle;

use crate::cron::Cron;
use crate::service::{JobService, Result, Row, finish};

/// Prefix of the `cron.job` names ForgeBase owns.
pub const JOB_PREFIX: &str = "fbsched_";
/// The notification channel `forgebase.fire_schedule()` signals on.
pub const CHANNEL: &str = "forgebase_jobs";
/// `next_run_at` of a pg_cron-owned schedule (9999-12-31T23:59:59Z): never
/// due for the Rust tick.
pub const OWNED: i64 = 253_402_300_799_000;

/// How often the server checks `cron.job` against the schedules (heals a
/// change made while the server could not reach the database).
const RECONCILE_EVERY: Duration = Duration::from_secs(60);

/// Seconds steps that tile a minute: `*/N * * * * *` fires on a grid that
/// restarts every minute, so only these are an "every N seconds" grid.
const SECOND_STEPS: [u32; 11] = [1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30];

/// The `cron.job` name of schedule `id`.
pub fn job_name(id: &str) -> String {
    format!("{JOB_PREFIX}{id}")
}

/// The command of the `cron.job` row of schedule `id`.
pub fn job_command(id: &str) -> String {
    format!("SELECT forgebase.fire_schedule('{}')", id.replace('\'', "''"))
}

/// The pg_cron schedule that fires exactly when ForgeBase's `expr` does, or
/// `None` when pg_cron would read it differently (or not at all): the
/// schedule then stays on the Rust tick.
///
/// pg_cron 1.6 (vixie cron's parser, UTC with the default `cron.timezone`)
/// takes five fields, or `N seconds` (1 to 59). Compared with
/// [`Cron::parse`]:
/// - a number with a step (`5/10`) is ForgeBase's "from 5 to the end"; in
///   pg_cron it is an error;
/// - `?` is accepted by ForgeBase only;
/// - a day field that *starts* with `*` (`*/2`) makes pg_cron AND the
///   day-of-month and day-of-week tests, ForgeBase ORs them unless the field
///   is a plain `*`; the expressions where the two readings can differ stay
///   on the tick;
/// - a 6-field expression maps when its seconds field is `0` (then it is the
///   5-field one) or it is `*/N * * * * *` with `N` dividing 60, which
///   becomes `'N seconds'`: ForgeBase's grid restarts every minute and
///   `forgebase.fire_schedule()` aligns the due time to the N-second grid.
pub fn map_expression(expr: &str) -> Option<String> {
    if Cron::parse(expr).is_err() {
        return None;
    }
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let five: &[&str] = match fields.len() {
        5 => &fields,
        6 => {
            let sec = fields[0];
            if sec == "0" {
                &fields[1..]
            } else if let Some(n) = sec.strip_prefix("*/")
                && fields[1..].iter().all(|f| *f == "*")
                && let Ok(n) = n.parse::<u32>()
                && SECOND_STEPS.contains(&n)
            {
                return Some(format!("{n} seconds"));
            } else {
                return None;
            }
        }
        _ => return None,
    };
    let names: [&[&str]; 5] = [&[], &[], &[], &MONTH_NAMES, &DAY_NAMES];
    for (field, names) in five.iter().zip(names) {
        if !pg_cron_reads(field, names) {
            return None;
        }
    }
    // Day fields: pg_cron ORs day-of-month and day-of-week unless either
    // field starts with `*`; ForgeBase ORs unless either is a plain `*`
    // part. They agree when each field is a star in both readings or in
    // neither.
    for field in [five[2], five[4]] {
        if field.starts_with('*') != has_plain_star(field) {
            return None;
        }
    }
    Some(five.join(" "))
}

const MONTH_NAMES: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAY_NAMES: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Whether the field has a list element that is exactly `*`.
fn has_plain_star(field: &str) -> bool {
    field.split(',').any(|p| p == "*")
}

/// Whether pg_cron's field grammar takes `field`: elements `*`, `n`, `a-b`,
/// `*/s` and `a-b/s` (no `n/s`, no `?`), numbers or three-letter names.
fn pg_cron_reads(field: &str, names: &[&str]) -> bool {
    field.split(',').all(|part| {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (part, None),
        };
        if let Some(s) = step
            && (s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
        let atom = |t: &str| {
            !t.is_empty() && (t.bytes().all(|b| b.is_ascii_digit()) || names.iter().any(|n| n.eq_ignore_ascii_case(t)))
        };
        match range.split_once('-') {
            Some((a, b)) => atom(a) && atom(b),
            None if range == "*" => true,
            None => step.is_none() && atom(range),
        }
    })
}

/// Keeps pg_cron's side running: the notification listener and the periodic
/// check. Dropping it stops both; `cron.job` rows stay (pg_cron keeps firing,
/// the next start finds them).
#[derive(Debug)]
pub struct PgCronHandle {
    tasks: Vec<AbortHandle>,
}

impl Drop for PgCronHandle {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Puts the schedules on pg_cron when `db` runs it (call once per server
/// start, after the migrations and `Database::setup_extensions`): checks the
/// layout, syncs `cron.job` with the schedules, fires the schedules whose due
/// time passed while the server was down, and starts the `LISTEN` wake-up.
/// Returns `None` (every schedule stays on the Rust tick) when the feature is
/// not active or its layout is not what this code expects.
pub async fn attach_pgcron(service: &JobService, db: &Database) -> Option<PgCronHandle> {
    if db.kind() != forgebase_db::BackendKind::Postgres {
        return None;
    }
    if !db.feature_active(Feature::Cron) {
        if let Err(e) = service.pgcron_release().await {
            tracing::warn!(error = %e, "schedules not handed back from pg_cron");
        }
        return None;
    }
    match service.pgcron_layout_problems().await {
        Ok(problems) if problems.is_empty() => {}
        Ok(problems) => {
            tracing::warn!(
                ?problems,
                "pg_cron is active but its layout is not what ForgeBase expects; schedules stay on the ForgeBase tick"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(error = %e, "pg_cron layout check failed; schedules stay on the ForgeBase tick");
            return None;
        }
    }
    if let Err(e) = service.pgcron_reconcile(None, true).await {
        tracing::warn!(error = %e, "schedules not synced to pg_cron; they stay on the ForgeBase tick");
        return None;
    }
    service.set_pgcron(true);
    let mut tasks = Vec::new();
    let listener = {
        let (service, db) = (service.clone(), db.clone());
        tokio::spawn(async move { listen(service, db).await })
    };
    tasks.push(listener.abort_handle());
    let check = {
        let service = service.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(RECONCILE_EVERY);
            tick.tick().await;
            loop {
                tick.tick().await;
                if let Err(e) = service.pgcron_reconcile(None, false).await {
                    tracing::warn!(error = %e, "syncing schedules to pg_cron failed");
                }
            }
        })
    };
    tasks.push(check.abort_handle());
    tracing::info!("schedules run on pg_cron where it reads their expression like ForgeBase");
    Some(PgCronHandle { tasks })
}

/// [`attach_pgcron`] on every set of the service's router (FB-172:
/// pg_cron runs per primary, each set's schedules fire on that set and its
/// `NOTIFY` wakes that set's claim loop). Without a router, or with one set,
/// this is [`attach_pgcron`] on `control`. Sets added later need another
/// call (it is idempotent).
pub async fn attach_pgcron_sets(service: &JobService, control: &Database) -> Option<PgCronHandle> {
    let sets = match service.router() {
        Some(router) if router.is_sharded() => router.sets(),
        _ => return attach_pgcron(service, control).await,
    };
    let mut tasks = Vec::new();
    for set in sets {
        let db = set.database().clone();
        let view = service.on_set(set);
        if let Some(mut handle) = attach_pgcron(&view, &db).await {
            tasks.append(&mut handle.tasks);
        }
    }
    (!tasks.is_empty()).then_some(PgCronHandle { tasks })
}

/// Wakes the worker pool on every `NOTIFY forgebase_jobs`.
async fn listen(service: JobService, db: Database) {
    let mut backoff = Duration::from_millis(500);
    loop {
        match db.listen(CHANNEL).await {
            Ok(mut listener) => {
                backoff = Duration::from_millis(500);
                loop {
                    match listener.recv().await {
                        Ok(_) => service.wake_pool(),
                        Err(e) => {
                            tracing::warn!(error = %e, "job notifications lost the connection");
                            break;
                        }
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot listen for job notifications"),
        }
        // Whatever arrived while the connection was down: look now.
        service.wake_pool();
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// What a schedule needs from pg_cron.
enum Want {
    /// Run on pg_cron with this schedule text.
    Cron(String),
    /// Run on the Rust tick.
    Tick,
}

impl JobService {
    /// Which engine fires the schedules mapped to pg_cron: `true` once
    /// [`attach_pgcron`] succeeded.
    pub fn pgcron_active(&self) -> bool {
        self.pgcron_flag()
    }

    /// After a schedule was created, changed or deleted: maps it to pg_cron
    /// (or takes it off) in a transaction of its own. A failure is logged;
    /// the periodic check repeats it, and until then the schedule stays
    /// with the tick.
    pub(crate) async fn pgcron_after_write(&self, id: &str) {
        if self.pgcron_flag()
            && let Err(e) = self.pgcron_reconcile(Some(id), false).await
        {
            tracing::warn!(schedule = id, error = %e, "schedule not synced to pg_cron");
        }
    }

    /// `s` as it reads now: with pg_cron active the sync may have changed
    /// who fires it.
    pub(crate) async fn schedule_fresh(&self, s: &crate::model::Schedule) -> Result<crate::model::Schedule> {
        if !self.pgcron_flag() {
            return Ok(s.clone());
        }
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {} FROM _forgebase_job_schedules WHERE id = ?1",
                    crate::service::SCHEDULE_COLS
                ),
                vec![s.id.as_str().into()],
            ))
            .await?;
        if rows.is_empty() {
            return Ok(s.clone());
        }
        crate::service::schedule_from(&Row::new(&rows, 0))
    }

    /// Why pg_cron cannot be used as this code expects (empty = it can):
    /// `forgebase.fire_schedule(text, bigint)` and the `cron.job` columns.
    pub(crate) async fn pgcron_layout_problems(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let f = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT pg_get_function_identity_arguments(p.oid) AS args FROM pg_proc p \
                 JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = 'forgebase' AND p.proname = 'fire_schedule'",
            ))
            .await?;
        if !(0..f.len()).any(|i| {
            Row::new(&f, i)
                .text("args")
                .replace(' ', "")
                .contains("text,p_duebigint")
        }) {
            problems.push("forgebase.fire_schedule(text, bigint) does not exist (migration 0038)".to_owned());
        }
        let cols = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT a.attname AS c, format_type(a.atttypid, a.atttypmod) AS ty FROM pg_attribute a \
                 WHERE a.attrelid = to_regclass('cron.job') AND a.attnum > 0 AND NOT a.attisdropped",
            ))
            .await?;
        let have: BTreeMap<String, String> = (0..cols.len())
            .map(|i| {
                let r = Row::new(&cols, i);
                (r.text("c"), r.text("ty"))
            })
            .collect();
        for (name, ty) in [("jobname", "text"), ("schedule", "text"), ("command", "text")] {
            match have.get(name) {
                Some(t) if t == ty => {}
                Some(t) => problems.push(format!("cron.job.{name} is {t}, expected {ty}")),
                None => problems.push(format!("cron.job has no column {name}")),
            }
        }
        Ok(problems)
    }

    /// Hands every schedule back to the Rust tick and removes ForgeBase's
    /// `cron.job` rows, when pg_cron is installed in this database. Called at
    /// start when the feature is not active.
    pub(crate) async fn pgcron_release(&self) -> Result<()> {
        self.set_pgcron(false);
        self.pgcron_reconcile_with(None, false, true).await
    }

    /// Brings `cron.job` and the schedules in line (one schedule, or all):
    /// maps the enabled schedules to pg_cron, removes what is not wanted,
    /// marks ownership in `next_run_at` and, with `catch_up`, fires the
    /// schedules whose due time passed. One transaction.
    pub(crate) async fn pgcron_reconcile(&self, only: Option<&str>, catch_up: bool) -> Result<()> {
        self.pgcron_reconcile_with(only, catch_up, false).await
    }

    /// Syncs `cron.job` with the schedules now on every set where pg_cron
    /// fires them (FB-172). A tenant move (FB-174) calls it when it freezes
    /// the tenant and after the switch: the schedules of a tenant moving in
    /// or out leave pg_cron on both sets until the move is done, then the
    /// target takes them over (the ownership mark travels with the rows).
    pub async fn sync_pgcron_now(&self) -> Result<()> {
        for view in self.views() {
            if view.pgcron_flag() {
                view.pgcron_reconcile(None, false).await?;
            }
        }
        Ok(())
    }

    async fn pgcron_reconcile_with(&self, only: Option<&str>, catch_up: bool, off: bool) -> Result<()> {
        let now = crate::service::now_ms();
        // FB-172: schedules of tenants moving in or out are left alone here
        // (their rows on this set may be a copy), except that pg_cron must
        // not fire them.
        let skip = self.excluded();
        let mut tx = self.begin().await?;
        let r = async {
            let has_cron = !off || {
                let rows = tx
                    .fetch_all(&Statement::raw(
                        "SELECT 1 AS x FROM pg_extension WHERE extname = 'pg_cron' \
                         AND pg_catalog.has_schema_privilege(current_user, 'cron', 'USAGE')",
                    ))
                    .await?;
                !rows.is_empty()
            };
            let schedules = tx
                .fetch_all(&match only {
                    Some(id) => Statement::new(
                        "SELECT id, cron, enabled, next_run_at, last_run_at, created_at, updated_at, tenant \
                         FROM _forgebase_job_schedules WHERE id = ?1",
                        vec![id.into()],
                    ),
                    None => Statement::raw(
                        "SELECT id, cron, enabled, next_run_at, last_run_at, created_at, updated_at, tenant \
                         FROM _forgebase_job_schedules",
                    ),
                })
                .await?;
            let mut jobs: BTreeMap<String, (String, String)> = BTreeMap::new();
            if has_cron {
                let rows = tx
                    .fetch_all(&Statement::raw(
                        "SELECT jobname, schedule, command FROM cron.job WHERE jobname LIKE 'fbsched\\_%'",
                    ))
                    .await?;
                for i in 0..rows.len() {
                    let r = Row::new(&rows, i);
                    jobs.insert(r.text("jobname"), (r.text("schedule"), r.text("command")));
                }
            }
            for i in 0..schedules.len() {
                let r = Row::new(&schedules, i);
                let id = r.text("id");
                let name = job_name(&id);
                let want = match map_expression(&r.text("cron")) {
                    Some(text) if r.int("enabled") == 1 && has_cron && !off => Want::Cron(text),
                    _ => Want::Tick,
                };
                let next = r.int("next_run_at");
                let existing = jobs.remove(&name);
                if r.opt_text("tenant").is_some_and(|t| skip.contains(&t)) {
                    if existing.is_some() {
                        tx.fetch_all(&Statement::new(
                            "SELECT cron.unschedule(?1::text) AS ok",
                            vec![name.as_str().into()],
                        ))
                        .await?;
                    }
                    continue;
                }
                match want {
                    Want::Cron(text) => {
                        let command = job_command(&id);
                        let (was_owned, mut due) = (next == OWNED, None);
                        if !was_owned && next <= now && !catch_up {
                            // Due for the Rust tick: let it fire first.
                            continue;
                        }
                        if catch_up {
                            due = missed_due(&r, was_owned, now);
                        }
                        if existing.as_ref() != Some(&(text.clone(), command.clone())) {
                            tx.fetch_all(&Statement::new(
                                "SELECT cron.schedule(?1::text, ?2::text, ?3::text) AS jobid",
                                vec![name.as_str().into(), text.as_str().into(), command.as_str().into()],
                            ))
                            .await?;
                        }
                        if !was_owned {
                            tx.execute(&Statement::new(
                                "UPDATE _forgebase_job_schedules SET next_run_at = ?2 WHERE id = ?1",
                                vec![id.as_str().into(), OWNED.into()],
                            ))
                            .await?;
                        }
                        if let Some(due) = due {
                            tx.fetch_all(&Statement::new(
                                "SELECT forgebase.fire_schedule(?1::text, ?2::bigint) AS job",
                                vec![id.as_str().into(), due.into()],
                            ))
                            .await?;
                        }
                    }
                    Want::Tick => {
                        if existing.is_some() {
                            tx.fetch_all(&Statement::new(
                                "SELECT cron.unschedule(?1::text) AS ok",
                                vec![name.as_str().into()],
                            ))
                            .await?;
                        }
                        if next == OWNED {
                            // Back to the tick: its next firing is the next due time from now.
                            let at = Cron::parse(&r.text("cron"))
                                .ok()
                                .and_then(|c| c.next_after(now))
                                .unwrap_or(now);
                            tx.execute(&Statement::new(
                                "UPDATE _forgebase_job_schedules SET next_run_at = ?2 WHERE id = ?1",
                                vec![id.as_str().into(), at.into()],
                            ))
                            .await?;
                        }
                    }
                }
            }
            // Jobs of schedules that no longer exist (or all of them, when only one id was asked for
            // and it is gone).
            for name in jobs.keys() {
                if only.is_some_and(|id| job_name(id) != *name) {
                    continue;
                }
                tx.fetch_all(&Statement::new(
                    "SELECT cron.unschedule(?1::text) AS ok",
                    vec![name.as_str().into()],
                ))
                .await?;
            }
            Ok(())
        }
        .await;
        finish(tx, r).await
    }
}

/// The first due time of a schedule that passed since it last ran (or since
/// it was created, changed or handed over), if any.
fn missed_due(r: &Row<'_>, was_owned: bool, now: i64) -> Option<i64> {
    if !was_owned {
        // Handed over from the tick: its recorded next firing, if it passed.
        let next = r.int("next_run_at");
        return (next <= now).then_some(next);
    }
    let base = [
        r.opt_int("last_run_at").unwrap_or(0),
        r.int("created_at"),
        r.int("updated_at"),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let due = Cron::parse(&r.text("cron")).ok()?.next_after(base)?;
    (due <= now).then_some(due)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Month, OffsetDateTime, Time};

    /// pg_cron's reading of an expression (entry.c of pg_cron 1.6 / vixie
    /// cron), written independently of [`Cron`]: minute, hour, day-of-month,
    /// month and day-of-week sets, and whether each day field starts with `*`.
    struct Oracle {
        min: u64,
        hour: u64,
        dom: u64,
        mon: u64,
        dow: u64,
        dom_star: bool,
        dow_star: bool,
    }

    fn atom(t: &str, names: &[&str], base: u32) -> Option<u32> {
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            return t.parse().ok();
        }
        names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(t))
            .map(|i| i as u32 + base)
    }

    /// vixie `get_list`/`get_range`: `*`, `n`, `a-b`, each optionally `/step`
    /// except a lone `n`.
    fn field(text: &str, lo: u32, hi: u32, names: &[&str], base: u32) -> Option<u64> {
        let mut bits = 0u64;
        for part in text.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((r, s)) => (r, Some(s.parse::<u32>().ok().filter(|s| *s > 0)?)),
                None => (part, None),
            };
            let (a, b) = if range == "*" {
                (lo, hi)
            } else if let Some((a, b)) = range.split_once('-') {
                (atom(a, names, base)?, atom(b, names, base)?)
            } else {
                if step.is_some() {
                    return None;
                }
                let n = atom(range, names, base)?;
                (n, n)
            };
            if a < lo || b > hi || a > b {
                return None;
            }
            let mut v = a;
            while v <= b {
                bits |= 1 << v;
                v += step.unwrap_or(1);
            }
        }
        Some(bits)
    }

    impl Oracle {
        fn parse(expr: &str) -> Option<Self> {
            let f: Vec<&str> = expr.split_whitespace().collect();
            if f.len() != 5 {
                return None;
            }
            let mut dow = field(f[4], 0, 7, &DAY_NAMES, 0)?;
            if dow & (1 << 7) != 0 {
                dow |= 1;
            }
            Some(Self {
                min: field(f[0], 0, 59, &[], 0)?,
                hour: field(f[1], 0, 23, &[], 0)?,
                dom: field(f[2], 1, 31, &[], 0)?,
                mon: field(f[3], 1, 12, &MONTH_NAMES, 1)?,
                dow: dow & 0x7f,
                dom_star: f[2].starts_with('*'),
                dow_star: f[4].starts_with('*'),
            })
        }

        fn day(&self, d: Date) -> bool {
            let dom = self.dom & (1 << d.day()) != 0;
            let dow = self.dow & (1 << d.weekday().number_days_from_sunday()) != 0;
            if self.dom_star || self.dow_star {
                dom && dow
            } else {
                dom || dow
            }
        }

        /// The next `n` due times (epoch ms) strictly after `after_ms`.
        fn next(&self, after_ms: i64, n: usize) -> Vec<i64> {
            let mut out = Vec::new();
            let mut date = OffsetDateTime::from_unix_timestamp(after_ms / 1000).unwrap().date();
            while out.len() < n {
                if self.mon & (1 << u8::from(date.month())) != 0 && self.day(date) {
                    for h in (0..24u8).filter(|h| self.hour & (1 << h) != 0) {
                        for m in (0..60u8).filter(|m| self.min & (1 << m) != 0) {
                            let t = date
                                .with_time(Time::from_hms(h, m, 0).unwrap())
                                .assume_utc()
                                .unix_timestamp()
                                * 1000;
                            if t > after_ms && out.len() < n {
                                out.push(t);
                            }
                        }
                    }
                }
                let Some(next) = date.next_day() else { break };
                date = next;
            }
            out
        }
    }

    fn forgebase_next(expr: &str, after_ms: i64, n: usize) -> Vec<i64> {
        let cron = Cron::parse(expr).unwrap();
        let mut out = Vec::new();
        let mut at = after_ms;
        while out.len() < n {
            // Past the horizon the expression never fires again (Feb 31st).
            let Some(t) = cron.next_after(at) else { break };
            out.push(t);
            at = t;
        }
        out
    }

    fn start() -> i64 {
        Date::from_calendar_date(2026, Month::November, 11)
            .unwrap()
            .with_time(Time::from_hms(8, 29, 7).unwrap())
            .assume_utc()
            .unix_timestamp()
            * 1000
    }

    /// The expressions the mapping accepts, and what each becomes.
    #[test]
    fn mapping_examples() {
        let cases = [
            ("* * * * *", Some("* * * * *")),
            ("*/15 * * * *", Some("*/15 * * * *")),
            ("0 9 * * MON-FRI", Some("0 9 * * MON-FRI")),
            ("  0   9 * * mon-fri ", Some("0 9 * * mon-fri")),
            ("30 2 1 JAN *", Some("30 2 1 JAN *")),
            ("0 0 * * 7", Some("0 0 * * 7")),
            ("0 0 13 * FRI", Some("0 0 13 * FRI")),
            ("1-5/2,10 */6 * * *", Some("1-5/2,10 */6 * * *")),
            ("0 0 0 * * *", Some("0 0 * * *")),
            ("*/10 * * * * *", Some("10 seconds")),
            ("*/1 * * * * *", Some("1 seconds")),
            ("*/30 * * * * *", Some("30 seconds")),
            // Not an N-second grid: ForgeBase restarts it every minute.
            ("*/7 * * * * *", None),
            ("*/45 * * * * *", None),
            ("*/10 * * * * 1", None),
            ("*/10 0 * * * *", None),
            ("5 * * * * *", None),
            ("0-30 * * * * *", None),
            // pg_cron: an error; ForgeBase: "from 5 to the end".
            ("5/10 * * * *", None),
            ("0 0 1/2 * *", None),
            // Day fields that start with `*` AND in pg_cron, OR in ForgeBase.
            ("0 0 */2 * FRI", None),
            ("0 0 13 * */2", None),
            ("0 0 *,5 * *", Some("0 0 *,5 * *")),
            ("0 0 5,* * FRI", None),
            // Never parses.
            ("", None),
            ("* * * *", None),
            ("60 * * * *", None),
        ];
        for (expr, want) in cases {
            assert_eq!(map_expression(expr).as_deref(), want, "{expr:?}");
        }
    }

    /// FB-165's differential test: for every expression the mapping accepts,
    /// pg_cron's reading (the oracle) and ForgeBase's parser produce the same
    /// next 1 000 due times; every expression the oracle rejects or reads
    /// differently is left to the tick.
    #[test]
    fn mapped_expressions_have_the_same_next_1000_due_times() {
        let mut expressions: Vec<String> = [
            "* * * * *",
            "*/5 * * * *",
            "0 * * * *",
            "0 0 * * *",
            "15 3 * * *",
            "0 9 * * MON-FRI",
            "0 0 1 * *",
            "0 0 29 2 *",
            "0 0 * * 7",
            "0 0 * * 0",
            "0 0 13 * FRI",
            "0 0 1,15 * MON",
            "30 2 1 JAN *",
            "0-59/20 */4 * jan-mar *",
            "0 0 31 * *",
            "0 12 * * SAT,SUN",
            "5,10,15 8-17 * * 1-5",
            "0 0 * 2,4,6 FRI-SUN",
            "59 23 31 DEC *",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        // A deterministic spread of the rest of the grammar.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut pick = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % n
        };
        let mins = ["*", "*/7", "0", "30", "5,35", "10-20", "0-59/15", "*/1"];
        let hours = ["*", "*/6", "0", "12", "8-17", "1,13", "6-18/4"];
        let doms = ["*", "1", "15", "31", "1,15", "10-20", "*/2", "*,5", "5,*", "1-31/7"];
        let mons = ["*", "1", "FEB", "*/3", "6-8", "dec,jan", "2-12/5"];
        let dows = ["*", "0", "7", "MON", "5-7", "mon-fri", "*/2", "SAT,sun", "1-6/2"];
        for _ in 0..400 {
            expressions.push(format!(
                "{} {} {} {} {}",
                mins[pick(mins.len())],
                hours[pick(hours.len())],
                doms[pick(doms.len())],
                mons[pick(mons.len())],
                dows[pick(dows.len())]
            ));
        }
        let (mut mapped, mut left) = (0, 0);
        for expr in &expressions {
            let Some(text) = map_expression(expr) else {
                left += 1;
                continue;
            };
            mapped += 1;
            let oracle =
                Oracle::parse(&text).unwrap_or_else(|| panic!("pg_cron would reject the mapped {text:?} ({expr:?})"));
            let want = oracle.next(start(), 1000);
            let got = forgebase_next(expr, start(), 1000);
            let n = got.len();
            assert_eq!(
                &want[..n.min(want.len())],
                &got[..n.min(want.len())],
                "due times differ for {expr:?} (mapped to {text:?})"
            );
            if n < 1000 {
                // ForgeBase's search stops at its horizon; the oracle must not have found fewer before it.
                assert!(want.len() >= n, "{expr:?}");
            }
        }
        assert!(
            mapped > 150,
            "too few expressions mapped: {mapped} of {}",
            mapped + left
        );
        assert!(left > 20, "the generator found nothing to leave to the tick");
    }

    /// The expressions the mapping leaves to the tick are the ones the
    /// oracle rejects or reads differently, not a wider set by accident.
    #[test]
    fn unmapped_expressions_really_differ_or_fail() {
        // pg_cron rejects these outright.
        for expr in ["5/10 * * * *", "0 0 1/2 * *", "? * * * *", "0 0 ? * MON"] {
            let parsed = Cron::parse(expr);
            assert!(map_expression(expr).is_none());
            if parsed.is_ok() {
                assert!(Oracle::parse(expr).is_none(), "{expr:?}");
            }
        }
        // pg_cron reads these, but differently from ForgeBase.
        for expr in ["0 0 */2 * FRI", "0 0 5,* * FRI"] {
            let o = Oracle::parse(expr).unwrap();
            assert_ne!(o.next(start(), 200), forgebase_next(expr, start(), 200), "{expr:?}");
            assert!(map_expression(expr).is_none());
        }
    }

    #[test]
    fn names_and_commands() {
        assert_eq!(job_name("sch-abc"), "fbsched_sch-abc");
        assert_eq!(job_command("sch-abc"), "SELECT forgebase.fire_schedule('sch-abc')");
        assert_eq!(job_command("a'b"), "SELECT forgebase.fire_schedule('a''b')");
    }
}
