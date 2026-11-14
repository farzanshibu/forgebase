# ADR-0041: Restore drills, a quiescent start mode and the sealed TLS state in backups

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-backup` (`drill.rs`, `stats.rs`, `manifest.rs`, `backup.rs`, `restore.rs`, `http.rs`),
  `crates/forgebase-cli` (`drill.rs`, `dataops.rs`, `doctor.rs`), `crates/forgebase-server` (`start_quiescent`,
  drill wiring), `crates/forgebase-core` (`[backup] include_tls`, `[backup] drill_schedule`), `docs/operations/`.
- Builds on: ADR-0024 (backup, restore, verify, doctor), ADR-0028 (bundled PostgreSQL), ADR-0029 §2.9 (backups with
  replicas), ADR-0033 (native TLS and ACME state), ADR-0035 (alert rules), ADR-0007 (jobs and schedules).
- Docs: [operations/disaster-recovery.md](../operations/disaster-recovery.md),
  [operations/ha-failover.md](../operations/ha-failover.md),
  [operations/alerting-runbook.md](../operations/alerting-runbook.md),
  [operations/upgrades.md](../operations/upgrades.md).

## Context

`forgebase backup verify` proves that a backup's files are intact: every hash matches, the SQLite copy passes
`integrity_check`, `pg_verifybackup` is happy. It does not prove that the backup **restores into a server that
works**: that the restored database starts and migrates, that the data the application relies on is all there, that
the blobs the database names exist, that the master key opens what it sealed. An operator finds that out when the
real disaster happens, which is the worst time. The built-in `backup_stale` alert (ADR-0035) only says that a backup
is recent. Nothing measured how long a recovery takes (RTO) or how much a recovery loses (RPO).

ADR-0033 also left a gap: snapshots did not carry `<data_dir>/tls/`, so a restore on an ACME server requested a new
certificate, against the CA's rate limits when restores are frequent (which is what drills are).

## Decisions

### 1. `forgebase backup drill`

`forgebase backup drill [<backup id|dir|latest|time>] [--json] [--keep] [--sample N] [--work-dir DIR]
[--master-key FILE] [--no-record]`. It never touches the live data directory:

1. **Restore** into `<data_dir>/tmp/drill-<id>/` (or `--work-dir`) with the ordinary `restore::restore`: the manifest
   and every hash are verified, `pg_verifybackup` runs on PostgreSQL, the key fingerprint is compared. A corrupt
   file, a damaged or missing manifest and a wrong key fail here with the reason `restore` gives
   (`storage/blobs/ab/ab12… is corrupt (SHA-256 differs from the manifest)`).
2. **Start a quiescent server** on the restored directory (decision 2), on `127.0.0.1` and a free port, with the live
   configuration pointed at the scratch directory and everything that reaches outside turned off: TLS, RESP, the
   public URL, replicas and sharding, an external database URL from the environment, S3 storage.
3. **Check**: `/health`; the schema version (`migration_status`: nothing pending, no problems, and whether the start
   migrated an older backup); `doctor` (the same checks as `forgebase doctor`, run against the restored directory,
   attached to the running quiescent server); **counts** against the manifest (decision 3); a **sample read** of up
   to `--sample` records per collection (default 5) through the data service, listing then fetching each by id; a
   **blob sample** (up to 20 `_forgebase_storage_blobs` rows: the file is read, its size and SHA-256 compared with
   the row); the **master key** (present, with the fingerprint the manifest records; a backup that holds no key is
   restored with the live key, or the escrow copy named by `--master-key`, which is how a drill proves the escrow);
   the sealed **`tls/`** state when the backup carries it.
4. **Stop** the server and delete the scratch directory unless `--keep`.
5. **Report**, human or `--json`: every check with `ok`, `warn`, `fail` or `skip`; the **RTO** (wall time of
   restore + start + checks, with the three phases) and the **RPO** (the backup's age when the drill started). The
   result is recorded (decision 5) and the exit code is non-zero when any check failed.

**Supported**: SQLite and the bundled PostgreSQL (physical `pg_basebackup` backups). **Refused with a reason**: a
logical `pg_dump` backup of an external PostgreSQL server (the drill does not own a scratch server to load it
into; the manual procedure is in the disaster-recovery runbook), a cluster backup with replicas or shards (follow-up),
and a database-only pre-migration snapshot (no storage or key to test).

A snapshot id of this data directory wins over a directory of the same name in the working directory (the API
passes ids). `latest` means the newest snapshot **directory**, not the newest with a readable manifest: a damaged newest backup is
the one a restore would use, so it is the one the drill must fail on. A directory path without a manifest is
accepted and fails with `MANIFEST.json is missing`.

Free space is checked first (the backup's size plus 20 % and 256 MiB); scratch directories of killed drills older than
six hours are swept.

### 2. A quiescent start mode

`forgebase_server::start_quiescent(&Config)` (and `start_quiescent_with_jobs`, for a test that registers a handler to
prove it never runs) starts the database, the API and the listener and **nothing that acts on its own**:

| Not started | Which means |
|---|---|
| job worker pool and scheduler (`start_pool`) | no scheduled job, no cron, no claim of a queued job |
| pg_cron attachment | schedules stay in the database; nothing wakes a pool |
| queue background task | no push delivery to webhooks or functions, no retention sweeps |
| function background task | no after-hook runs, no outbound `fetch` from a hook |
| alert engine | no notification to a webhook, Slack, PagerDuty or SMTP |
| realtime dispatch | `dispatch = false` (the outbox is written, nothing is delivered) |
| vector warm-up and saver | |
| TLS renewal and file watcher, the plain HTTP listener | the drill config has TLS off; the guard is for any caller |
| the `[backup] drill_schedule` | and the drill API answers `503` in this mode |

It is a Rust API, not a CLI flag or a configuration key: nothing a production `serve` could be started in by
mistake. The ordinary start is unchanged (the `quiescent` argument of the private `start_full` is `false`). The
test `tests/quiescent.rs` seeds a one-second schedule, a queued job and a push subscription with a message, shows
that the ordinary server runs all three, and that the quiescent one runs none and still answers `/health`.

pg_cron, when the restored PostgreSQL cluster has the extension, is the one thing that lives in the database: its
worker may enqueue rows into the restored job table. It cannot run them (no pool) and the table is thrown away.

### 3. Counts in the manifest

`Manifest.stats` (new, `serde(default, skip_serializing_if = "Option::is_none")`, so **format 1 and 2 are
unchanged** and older manifests read as `None`): per table-backed collection the record count, and the row counts of
`_forgebase_storage_objects` and `_forgebase_storage_blobs`, each as a `CountRange {min, max}`.

- SQLite: counted on the finished `VACUUM INTO` copy, opened read-only (so exact: `min == max`).
- Bundled PostgreSQL: counted on the live database before and after `pg_basebackup`; a write in between widens the
  range, and the drill accepts any restored count inside it. A physical backup is not tied to one instant, so an exact
  count does not exist.
- External PostgreSQL (logical) and database-only snapshots: `None`.
- A collection that cannot be counted is left out; a failure to count never fails the backup.

A drill compares each recorded collection and the two storage tables. A backup without `stats` still drills; the
`counts` check is a `warn` that says so.

### 4. The sealed `tls/` directory in backups

`[backup] include_tls` (`FORGEBASE_BACKUP_INCLUDE_TLS`; unset means **on when `[tls] mode = "acme"`**, off
otherwise). The files of `<data_dir>/tls/` are listed in the manifest with the new `FileKind::Tls`, hashed like
every file, and `restore` moves the directory in (the replaced one goes to `backups/pre-restore-*`). A backup without
TLS state leaves the target's own alone. The state is sealed with the master key, so it is only readable with the
key the backup is restored with. The snapshot API and `forgebase backup` both apply the setting; the pre-migration
snapshot and cluster members do not carry it (a cluster restore does not install it yet).

This closes the ADR-0033 note "snapshots do not include `<data_dir>/tls/`"; that ADR and `deployment.md` now point
here. **Compatibility**: a build older than this ADR refuses a manifest that lists a `tls` file ("unknown variant"),
as it refuses any manifest it cannot read. Backups made without `include_tls` stay readable by every build.

### 5. History, API, metrics, schedule

- **History**: one JSON file per drill in `<data_dir>/backups/drills/<drill id>.json` (the last 100), written by the
  drill itself, whoever ran it. No migration; it works on both backends; it survives a restore of the data it
  describes (the restore replaces the database, not the history); it is not part of a backup. `DrillStore` lists it.
- **API** (administrators): `POST /api/v1/system/backups/{id}/drill[?sample=N]` (`id` may be `latest`) answers
  `202 {job_id, backup_id}`; `409 drill_running` while one is queued or running; `503 drill_unavailable` when the
  process is not the `forgebase` command. The work is the job `system.backup_drill`, which runs
  `forgebase backup drill <id> --json --trigger api` **in a child process of the server's own executable**: a
  drill shares no memory, metrics recorder, tracing subscriber or signal handlers with the live server, a crash in it
  cannot take the server down, and what restores is the same code path an operator runs. The job's result is the
  report. `GET /api/v1/system/backups/{id}/drills` returns the history of a backup, newest first, with RTO and RPO.
- **Metrics** (read from the history on every scrape, so drills run from cron are included):
  `forgebase_backup_last_drill_success_timestamp_seconds`, `forgebase_backup_last_drill_ok` (1 or 0),
  `forgebase_backup_drill_duration_seconds{phase="total|restore|start|checks"}`,
  `forgebase_backup_drill_rpo_seconds` (the latest drill), and the counter `forgebase_backup_drills_total{outcome}`
  (drills run through the job, since the process started). Nothing is emitted before a first drill.
- **Schedule**: `[backup] drill_schedule` (`FORGEBASE_BACKUP_DRILL_SCHEDULE`, a UTC cron expression, off by
  default) creates or updates the job schedule `backup-drill` at start (and disables it when the setting is removed);
  it drills `latest`. It shows in `GET /api/v1/jobs/schedules` like any schedule.

## Consequences

- No migration, **no new dependency** (`forgebase-backup` gains `forgebase-jobs`, already a workspace crate, and
  `metrics`, already used by other crates; both MIT or Apache-2.0).
- `BackupSettings` is no longer `Copy` (it holds the schedule string). `BackupOptions` gains `include_tls`
  (default `false`); struct literals without `..Default::default()` need it.
- A drill costs as much disk as the backup and about the time a real restore takes. It is meant for a schedule of
  days to weeks, not minutes.
- The drill checks the **control** database and the local blobs. Tenant data on shard sets, object-storage in S3 and
  replicas are not exercised (cluster backups are refused).
- RTO measured on a scratch directory on the same host is a floor: it excludes fetching the backup from off-site
  storage, provisioning a host and DNS. The disaster-recovery runbook says how to add those.

## Not done (follow-ups)

- A dashboard "Run drill" button and drill history card; an alert rule on
  `forgebase_backup_last_drill_success_timestamp_seconds` (the alert engine's signals are fixed to
  `forgebase:backup_*`; a `backup_drill_stale` rule is a small addition there); a global `GET /system/backups/drills`.
- Drilling a cluster backup (replicas and shards), and an external PostgreSQL logical backup against a scratch server
  supplied by the operator (`--scratch-url`).
- Carrying `tls/` in cluster backups and the pre-migration snapshot.

## Rollback

Remove `include_tls` and `drill_schedule` from the configuration (both default off, `include_tls` on for ACME only);
the `stats` and `tls` manifest entries are ignored by this build's readers if absent. Delete
`<data_dir>/backups/drills/` to drop the history.
