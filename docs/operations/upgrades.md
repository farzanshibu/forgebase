# Upgrades

A checklist for upgrading ForgeBase **without surprises**: read what changes, snapshot, prove the snapshot restores,
rehearse the migration on a copy, upgrade, verify, and keep a way back. It extends [deployment.md §6](../deployment.md#6-upgrades) (the
short form) with the rehearsal and the drill. Backups and restores themselves are in
[disaster-recovery.md](disaster-recovery.md); replicas in [ha-failover.md](ha-failover.md).

The facts the checklist rests on:

- **Migrations run automatically at `serve`** and are **forward-only**. A database migrated by a newer binary is refused by an older one.
  The only way back is a snapshot.
- Before the first pending migration the server writes a **pre-migration snapshot** (SQLite: `backups/pre-migrate-v<N>-<time>.db`; bundled
  PostgreSQL: the directory `backups/pre-migrate-v<N>-<time>/`, a `pg_basebackup` of the cluster; **external PostgreSQL: none**). If it fails,
  nothing is migrated. It is a safety net, not a substitute for the snapshot below: it is database-only (no storage, no key) and on the same disk.
- A physical PostgreSQL backup restores on the **same PostgreSQL major** only.
- A restore, and so a drill, never changes the backend.

## 1. Before the window

- [ ] **Read the release notes** of every version between yours and the target ([release-notes/](../release-notes/)): breaking changes, new
      configuration keys, backend changes (for example 0.2 → 0.3: PostgreSQL is the default for *new* projects; an existing SQLite project stays SQLite).
- [ ] **Does the bundled PostgreSQL major change?** If the notes say so, old physical snapshots can only be restored by a binary with the old major.
      Keep the old binary, and take a logical `forgebase export collections` bundle too (it moves across majors).
- [ ] **Record the current state with the old binary**: `forgebase version --json`, `forgebase migrate --status`, `forgebase doctor`,
      `forgebase db status` (PostgreSQL), `GET /health/ready`. Save the output; it is your "before".
- [ ] **Confirm the safety net exists and is recent**: `forgebase backup list`; the off-site copy ([disaster-recovery.md §3](disaster-recovery.md#3-strategy));
      `auth/master.key` escrowed apart from backups (fingerprint matches).
- [ ] **Disk**: free space for a snapshot and, for a rehearsal, a copy (`df -h forgebase-data`; the `disk_free_*` alerts are quiet).
      A pre-migration snapshot of PostgreSQL is as big as the cluster.
- [ ] **A topology with replicas**: all nodes on the same engine build after the upgrade (a join is refused on a mismatch). Plan to upgrade every
      node's binary in the same window and check `GET /api/system/database/topology` afterwards; no `needs_rebuild` should be left behind.
- [ ] **Tell people**, **silence** the alerts that will fire (`POST /api/v1/alerts/silences` for `http_5xx_ratio_high`, `/health` checks at the proxy), and
      schedule it: the single server stops, so there is an outage (the startup probe allows 15 minutes for migrations on the first start).
- [ ] **Pause scheduled work that must not overlap**: a snapshot or drill cron, the backup CronJob.

## 2. Snapshot, then prove it

Take the snapshot **with the old binary, with the server stopped or running** (it works online), label it, and drill it.

```sh
forgebase backup --label pre-upgrade-0.3.5          # a snapshot; note its id
forgebase backup verify latest                      # hashes, integrity, pg_verifybackup
forgebase backup drill latest --master-key /escrow/master.key   # restores into a scratch directory, starts a quiet server, checks, deletes
```

- Copy the snapshot **off-site** now, before you change anything.
- The drill is the check that matters: it restores the snapshot and looks at the data. A failed drill is a **stop**: fix the backup before you upgrade
  ([disaster-recovery.md §5](disaster-recovery.md#5-drills)). The drill's RTO tells you how long a rollback will take; write it down.
- On **external PostgreSQL**, ForgeBase takes no pre-migration snapshot and the drill does not load logical backups: take a provider snapshot (and a
  `pg_dump` with `forgebase backup`), and rehearse the dump on a scratch server ([disaster-recovery.md, External PostgreSQL](disaster-recovery.md#external-postgresql)).
- Kubernetes: also take a CSI **volume snapshot** of the PVC, so the rollback can be a volume restore.

## 3. Rehearse the migration on a copy

Run the **new** binary's drill against the pre-upgrade snapshot on a **scratch data directory, not the live one** (a staging host is best; another
directory on the same host does). Starting a quiescent server on the restored data **applies the pending migrations**, so this is the real migration on
your real data, without touching production:

```sh
mkdir -p /srv/rehearsal && cp forgebase.toml /srv/rehearsal/          # the same configuration, so the same engine settings
./forgebase-0.3.5 --data-dir /srv/rehearsal/data --config /srv/rehearsal/forgebase.toml \
    backup drill /path/to/forgebase-data/backups/snapshots/<pre-upgrade id> --json --master-key /escrow/master.key \
    | jq '{outcome, rto_seconds, phases, problems, schema: (.checks[] | select(.name=="schema").detail), server: (.checks[] | select(.name=="server").detail)}'
```

Read it as follows:

- `schema`: `backup at schema vN, migrated to vM at start`: the migrations ran on your data. `server` gives how many and how long the **start**
  took (`phases.start_ms`): that is your **downtime estimate** for the migration, on this hardware.
- `outcome: passed`: the new binary restores and starts on your data, `doctor` is clean, counts and samples read back.
- A failure here is a finding, not an outage: **do not upgrade**, report it or fix the data first.
- It does not exercise your application: run your own smoke tests against the scratch server with `--keep` and `forgebase serve --data-dir <kept> --port 0`
  if you need them (stop it, delete the directory).

Also: `forgebase-new migrate --status` against the scratch directory lists what is pending.

## 4. The window

1. **Stop the server**: `systemctl stop forgebase` (SIGTERM; it drains for `shutdown_timeout_secs`, checkpoints, stops PostgreSQL), or scale the
   StatefulSet to 0. Confirm `forgebase status` shows nothing running and no `postmaster` is left (`forgebase db status`).
2. **Last snapshot** if data changed since §2: `forgebase backup --label pre-upgrade-final` (offline it is quiet and exact). The master key is where it was.
3. `forgebase doctor` on the stopped data directory (it starts the engine under the lock for its checks and stops it).
4. **Replace the binary** (package, `install`, new image tag, `helm upgrade --set image.tag=…`, `kubectl apply -k …`).
   Keep the old binary (rename it) until the post-upgrade drill has passed.
5. **Start**. Watch the log for the migration lines and the pre-migration snapshot. First start of a PostgreSQL project may extract a new engine and replay WAL.
   Kubernetes: `kubectl -n forgebase rollout status statefulset/forgebase` (the startup probe waits up to 15 minutes); do **not** delete a pod that is
   migrating.

## 5. Verify

- [ ] `GET /health/ready` is `200`; `forgebase status`; `forgebase version --json` is the new version; `forgebase migrate --status` has nothing pending.
- [ ] `forgebase doctor` is clean (compare with your "before").
- [ ] The application's smoke test: sign in, read a record you know, write and read back a record, upload and download an object, run a function, publish and pull
      a queue message, a job runs. Restart the alert silence only after this.
- [ ] Replicas: all `streaming`, same engine version, epoch unchanged.
- [ ] `/metrics` and the dashboards look like "before" for an hour: error ratio, latency, queue backlog, job failures.
- [ ] **A new snapshot with the new binary, and a drill of it**: `forgebase backup --label post-upgrade && forgebase backup drill latest`. This proves the new
      binary writes backups it can restore, and gives the new RTO.
- [ ] Re-enable the schedules you paused and remove the silence.

## Rollback

Use it when the upgrade is wrong and the data written since is expendable or can be reconciled. A database migrated by the new binary **cannot** be opened
by the old one, so the way back is a restore.

1. Stop the server.
2. Restore: **the snapshot of §2** (`forgebase restore <id> --force --master-key /escrow/master.key` with the **old** binary; the replaced data goes to
   `backups/pre-restore-*`), or the **pre-migration snapshot** of the failed start:
   - SQLite: copy `backups/pre-migrate-v<N>-<time>.db` over `forgebase.db` and delete `forgebase.db-wal` and `forgebase.db-shm`.
   - Bundled PostgreSQL: `forgebase restore backups/pre-migrate-v<N>-<time> --force` (it puts the cluster back; storage and keys are left alone).
   - External PostgreSQL: restore the provider snapshot, or `forgebase restore <backup> --force` of the logical backup.
   - Kubernetes: restore the CSI volume snapshot, or run the restore in a one-off pod ([disaster-recovery.md §4.5](disaster-recovery.md#45-kubernetes)).
3. Put the **old binary** back and start it. `forgebase doctor`, `/health/ready`, the smoke test.
4. Writes made on the new version are gone: the pre-upgrade snapshot is the RPO of a rollback. If the window was long and writes mattered, **export** them
   first (`forgebase export collections` from the migrated data, with the new binary) and re-import after.
5. Keep the failed state for the bug report: `backups/pre-restore-*` has it.

The way back can be rehearsed too: §3 proves the forward path, and a drill of the pre-upgrade snapshot with the **old** binary (`forgebase backup drill`) proves the back.

## 7. After

- [ ] Delete the pre-upgrade snapshot and `backups/pre-migrate-*` / `pre-restore-*` **after** the post-upgrade drill passed and a few days of normal running,
      keeping the off-site copy until the next good one.
- [ ] Update your runbooks and the recorded RTO from the post-upgrade drill; if it grew past your budget, say so now.
- [ ] Update escrowed configuration for any new keys, and the [release checklist](../deployment.md#8-release-checklist).
- [ ] Remove the rehearsal directory (`/srv/rehearsal`) and any `--keep` scratch directories: they hold a full copy of the data.

## 8. Quick card

```text
BEFORE   notes read · state recorded · off-site backup + key escrow ok · disk ok · silence set
SNAPSHOT forgebase backup --label pre-upgrade  →  backup verify latest  →  backup drill latest   (stop if it fails)
REHEARSE new-binary `backup drill <snapshot>` on a scratch dir  →  schema "migrated to vM", phases.start_ms = downtime estimate
WINDOW   stop → final snapshot → doctor → replace binary → start → watch migration
VERIFY   ready · doctor · smoke test · metrics · NEW snapshot + drill
BACK     stop → restore the pre-upgrade snapshot with the OLD binary → start → doctor
```
