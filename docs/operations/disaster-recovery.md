# Disaster recovery

What to back up, how often, how to prove that a backup restores, how to restore on each backend, and how to
measure the recovery time and the data you would lose. The commands are `forgebase backup`, `forgebase backup drill`
and `forgebase restore`; the mechanics of a backup are in [database.md §5](../database.md#5-backups-and-rollback), the
design of drills in [ADR-0041](../adr/0041-backup-drills.md).

Related runbooks: [HA and failover](ha-failover.md) (a replica is not a backup), [upgrades](upgrades.md) (snapshot
and drill before every upgrade), [alerting](alerting-runbook.md) (`backup_stale`, `disk_free_*`).

## 1. Terms

| | Means | ForgeBase's number |
|---|---|---|
| **RPO** (recovery point objective) | How much recent data you accept to lose | The age of the backup you restore. Snapshots are the only recovery points (no WAL archive, no point-in-time recovery between them), so RPO = the interval between snapshots, plus the time the last one took. |
| **RTO** (recovery time objective) | How long you accept to be down | Detection, getting the backup and a host, the restore, the start, the checks, cutover. `forgebase backup drill` measures the middle three. |

## 2. What there is to lose

| Item | Where | In a backup | What to do |
|---|---|---|---|
| Database | `forgebase.db` (SQLite), `postgres/data` (bundled PostgreSQL), your server (external) | yes: `VACUUM INTO`, `pg_basebackup`, `pg_dump` | `forgebase backup` on a schedule. |
| Object blobs | `storage/` (local driver) | yes | Same. **Not** objects in S3 (`storage.driver = "s3"`): use the bucket's versioning and cross-region replication. |
| Derived indexes | `indexes/` | yes (`--no-indexes` leaves them out) | Rebuildable: skip them to shrink backups. |
| **Master key** | `auth/master.key` | **no**, unless `--include-master-key` | Escrow it (§3.3). Without it, sealed secrets (provider keys, TOTP seeds, signing keys, the ACME account) are unreadable and restore refuses to continue. |
| Sealed TLS state | `tls/` (ACME account and certificate) | yes with `[backup] include_tls` (on by default for `mode = "acme"`) | Opens only with the master key. Without it a restore requests a new certificate (mind the CA's rate limits). |
| Configuration | `forgebase.toml`, `.env`, environment | no | Keep it in version control or `forgebase export config`; secrets in your secret store. |
| Certificate files | wherever `[tls] cert_file` and `key_file` point | no | Your PKI or cert-manager owns them. |
| `roles.secret` (bundled PostgreSQL) | `postgres/roles.secret` | yes (next to the cluster) | It is a login secret for the cluster it sits with: treat the backup as sensitive. |

A backup is **not encrypted**. It holds every record, every blob and (bundled PostgreSQL) the engine's passwords;
the key that seals secrets is outside it, which is why secrets stay safe in a stolen backup and the data does not.
Encrypt copies you send off-site (§3.2).

## 3. Strategy

### 3.1 Schedule and retention

Take snapshots from cron or a systemd timer on the host that runs the server (the CLI works while the server runs):

```sh
# every 6 hours; keep the last 28 and nothing older than 30 days beyond them
0 */6 * * *  forgebase backup --keep-last 28 --max-age-days 30 --label scheduled
```

Choose the interval from the RPO you can defend: 24 h loses a day, 1 h loses an hour, and the cost of a snapshot is a
read of the whole database (SQLite `VACUUM INTO` and `pg_basebackup` copy everything; there are no incremental
backups). The built-in `backup_stale` alert fires at 26 hours; raise or lower it with a custom rule if your interval
differs ([alerting-runbook.md](alerting-runbook.md#backup_stale)).

On a cluster with replicas, `[backup] source = "replica"` and `forgebase backup --cluster` take the copy from a replica
so the primary is not loaded ([ha-failover.md](ha-failover.md)). Kubernetes: the chart's backup CronJob execs into the
pod ([deployment.md §9.5](../deployment.md#95-backups)).

### 3.2 3-2-1

Three copies of the data, on two kinds of storage, one of them off-site and one you cannot delete with the credentials
the server runs with.

1. **Live data**: the data directory (copy 1).
2. **Local snapshots** in `backups/snapshots/` (copy 2). They share the disk's fate: they answer "I broke the data",
   not "the host burned".
3. **Off-site copy** (copy 3): after each snapshot, copy the snapshot **directory** somewhere else, **encrypted**, with
   write-once or versioned storage (S3 object lock, a bucket with versioning, restic, borg):

   ```sh
   snap=$(forgebase backup --json | jq -r .path)
   id=$(basename "$snap")
   restic backup "$snap" --tag forgebase                        # encrypts and deduplicates
   # or: tar -C "$(dirname "$snap")" -cf - "$id" | age -r "$AGE_RECIPIENT" | aws s3 cp - "s3://dr-bucket/forgebase/$id.tar.age"
   ```

   Prove the copy, not the original: restore it somewhere (fetch, decrypt) and run
   `forgebase backup verify <dir>` and `forgebase backup drill <dir>` on it (§5).
4. **A second site or provider** for the copy that decides your worst case: the cloud account, not only the region.

A replica is not a backup: it replays a `DELETE`, a bad migration and corruption as faithfully as any other write.

### 3.3 Master key escrow

`auth/master.key` is created at first start, never changes, and cannot be recreated. Escrow one copy of it, **apart
from the data backups** (a key stored next to the data it protects protects nothing):

- a secret manager (Vault, AWS Secrets Manager, Kubernetes Secret restricted to a break-glass role), and
- an offline copy (printed, or an encrypted USB, in a safe) held by someone other than the person who holds the
  backups.

```sh
base64 < forgebase-data/auth/master.key        # paste into the secret manager
kubectl -n forgebase exec forgebase-0 -- cat /data/auth/master.key > master.key   # Kubernetes
```

Its fingerprint is in every manifest (`master_key.fingerprint`, the first 16 hex characters of its SHA-256), so you can
check that the escrow is the right key without opening anything:

```sh
sha256sum master.key | cut -c1-16        # equals "master_key.fingerprint" in MANIFEST.json
```

`--include-master-key` puts the key into the backup: use it only when the backup storage is as trusted and as
restricted as the key's escrow (a sealed, access-logged vault), never for a bucket several people can read.

**Prove the escrow, regularly:** `forgebase backup drill --master-key /path/from/escrow/master.key` restores with the
escrowed copy and fails if it is not the key of this data.

### 3.4 What each backend gives you

| | SQLite | Bundled PostgreSQL | External PostgreSQL |
|---|---|---|---|
| Backup | `VACUUM INTO` of `forgebase.db`: one consistent snapshot, readers are never blocked | `pg_basebackup` of the cluster, plus `roles.secret` | `pg_dump -Fc` of the `forgebase` and `public` schemas |
| RPO | snapshot interval | snapshot interval | snapshot interval for ForgeBase's own backup; **your provider's PITR is better** (see §6.3) |
| Restore needs | the same ForgeBase (or newer) | the **same PostgreSQL major** as the backup | a server to load the dump into |
| Drill | yes | yes | no: refused with a reason, see §6.3 |

## 4. Restore

Restore is offline and reversible: it takes the data-directory lock (so the server must be stopped), verifies the
backup, stages it, hashes it again, then swaps. The data it replaces is **moved** to `backups/pre-restore-<time>/`,
never deleted. A crash mid-swap is rolled back by the next `forgebase restore` (or `restore --recover`).

### 4.1 Before you restore

1. Decide **which** backup. `forgebase backup list`; the newest is not always right (a corruption you took a
   snapshot of, a bad migration). `forgebase restore <RFC 3339 time>` restores the newest snapshot at or before it.
2. **Stop the server** (`systemctl stop forgebase`, scale the StatefulSet to 0). Check nothing else holds the data:
   `forgebase status`.
3. Have the **master key**: it is in the backup only with `--include-master-key`; otherwise it is in the data
   directory (same host) or in escrow (new host): `--master-key FILE`.
4. Take the failure's evidence first if there is any: the replaced data is kept, but the log and the unhappy state are
   not.
5. If you can, **drill the backup first** on another host or directory (§5). It takes minutes and tells you whether the
   restore will work.

### 4.2 SQLite

```sh
systemctl stop forgebase
forgebase restore latest --force --master-key /secure/master.key   # omit --master-key when the data directory still has it
forgebase doctor
systemctl start forgebase
curl -fsS localhost:8090/health/ready
```

On a new host: install the **same or a newer** ForgeBase binary, create the (empty) data directory, restore
into it without `--force`, put `forgebase.toml` back, start.

### 4.3 Bundled PostgreSQL

```sh
systemctl stop forgebase
forgebase restore latest --force --master-key /secure/master.key
forgebase doctor
systemctl start forgebase     # the first start replays the copied WAL: allow it a moment longer
```

- The binary's engine must have the **same PostgreSQL major** as the backup (`forgebase restore` refuses
  otherwise, naming both). To move to another major, export and import (`forgebase export collections`, `import
  collections`) or `forgebase db convert`.
- It refuses while a postmaster still runs on the cluster: stop ForgeBase and wait for the engine to exit.
- A restore rewrites the primary's history, so every **replica** of that set becomes `needs_rebuild` and is not
  routed to until rebuilt: managed replicas rebuild themselves after the start, external ones when you confirm
  ([ha-failover.md §6](ha-failover.md#6-after-a-restore-or-a-promote)).
- A cluster backup (`forgebase backup --cluster`) restores with the same command, pointed at its directory: every set
  is restored offline, the schema is reconciled at the next start.

### External PostgreSQL

ForgeBase's backup of an external server is a logical `pg_dump` of its own schemas; restore loads it back with
`pg_restore --clean --single-transaction` and replaces the objects of those schemas in that database:

```sh
export FORGEBASE_DATABASE_URL=postgres://forgebase@db.example/forgebase    # the target
forgebase restore latest --force --master-key /secure/master.key           # dumps what it replaces to backups/pre-restore-*/postgres.dump first
```

What a **drill** can and cannot do here: `forgebase backup drill` does not restore a logical backup, because it would
have to load it into a server of its own and must never load it into yours. Refused with that reason, the drill is
still worth doing by hand, monthly:

1. Create a scratch database on a scratch server (a throwaway container is enough, same major or newer):
   `docker run --rm -d -e POSTGRES_PASSWORD=x -p 5499:5432 postgres:18`.
2. Run the restore against it, with the server stopped or on another host:
   `FORGEBASE_DATABASE_URL=postgres://postgres:x@localhost:5499/postgres forgebase restore <backup> --force --master-key …`.
3. Run `FORGEBASE_DATABASE_URL=… forgebase doctor` and `forgebase serve --port 0` against it; compare
   `SELECT count(*)` of your key collections with the numbers in the backup's `MANIFEST.json`
   (`stats` is not recorded for logical backups: count on the source at backup time).
4. Time it, write the numbers down (§6), drop the scratch server.

For an external server, your **provider's own point-in-time recovery** (RDS snapshots with PITR, Cloud SQL, Neon
branches, `pgBackRest` with WAL archiving) gives a far better RPO than a daily dump; ForgeBase's dump is the
portable second copy. A restore of a provider snapshot needs no ForgeBase restore at all: point `FORGEBASE_DATABASE_URL`
at the recovered server and start (`forgebase doctor` first).

### 4.5 Kubernetes

```sh
kubectl -n forgebase scale statefulset/forgebase --replicas=0
kubectl -n forgebase run restore --rm -it --restart=Never --image=ghcr.io/farzanshibu/forgebase:<version> \
  --overrides='{"spec":{"securityContext":{"runAsUser":10001,"runAsGroup":10001,"fsGroup":10001},
    "containers":[{"name":"restore","image":"ghcr.io/farzanshibu/forgebase:<version>",
      "command":["forgebase","restore","latest","--force","--data-dir","/data","--master-key","/escrow/master.key"],
      "volumeMounts":[{"name":"data","mountPath":"/data"},{"name":"escrow","mountPath":"/escrow"}]}],
    "volumes":[{"name":"data","persistentVolumeClaim":{"claimName":"data-forgebase-0"}},
               {"name":"escrow","secret":{"secretName":"forgebase-master-key"}}]}}'
kubectl -n forgebase scale statefulset/forgebase --replicas=1
```

(Adapt the claim and Secret names to your release.) Snapshots on the same PVC survive a pod loss but not a lost
volume: restore from the off-site copy by mounting it into that pod as well.

### 4.6 After a restore

`forgebase doctor` (it runs again as part of every drill), then `GET /health/ready`, then look at what the data
says: a recent record you know exists, the newest object, the job and queue screens. Everything written after the
backup is gone: tell the people who wrote it. Jobs that were `running` when the backup was taken are released and run
again; push subscriptions resume and re-deliver messages the backup still holds as undelivered (consumers must be
idempotent, as they are for any redelivery). Replicas: [ha-failover.md](ha-failover.md).

### 4.7 One collection, not the whole system

For "someone deleted the `orders` collection yesterday" a full restore throws away a day of everything else. Restore
the backup **beside** the live data, then move only what you need:

```sh
forgebase backup drill <id> --keep --json | jq -r .kept                 # restores into a scratch directory and leaves it
forgebase --data-dir "$SCRATCH" export collections /tmp/orders -c orders
systemctl stop forgebase
forgebase import collections /tmp/orders --on-conflict skip             # records that exist stay as they are
systemctl start forgebase
rm -rf "$SCRATCH"
```

(`$SCRATCH` is the `kept` path of the report. Ids are preserved; timestamps are reassigned. The scratch directory is
a full copy of the data: delete it when done.)

## 5. Drills

A backup that has never been restored is a hope. `forgebase backup verify` proves the files are intact; a **drill**
proves the backup restores into a server that works.

```sh
forgebase backup drill                       # the newest snapshot
forgebase backup drill <id|dir|time> --json  # a specific one; JSON for a pipeline
forgebase backup drill --keep                # leave the restored copy to look at (its path is "kept" in the report)
forgebase backup drill --work-dir /mnt/big   # the scratch directory needs about the size of the backup
forgebase backup drill --master-key /escrow/master.key    # restore with the escrowed key
```

What it does, in order (and why each failure is a failed drill):

1. Restores into a scratch directory (never the live one): manifest, every hash, `pg_verifybackup`, key fingerprint.
2. Starts a **quiescent** server on it, on `127.0.0.1` and a free port: no jobs, cron, push delivery, webhooks,
   alerts, function hooks or certificate renewal run, so a restored backup cannot email your customers.
3. `doctor`, the schema version (nothing pending; an older backup is migrated by the start, and the report says so),
   **counts** of every collection and of the storage tables against the manifest, a **sample read** of records
   (`--sample N` per collection) and of **blobs** (read from disk, SHA-256 and size compared with the database), the
   **master key**, the sealed **`tls/`** state.
4. Stops the server and deletes the scratch directory (unless `--keep`).
5. Prints the report and exits **non-zero if anything failed**; the result is also recorded.

```text
restore drill of 20261114T005254Z (postgres 18.3 (managed), schema v66, taken 2026-11-14T00:52:54Z)
[ok]   restore          postgres 18.3 (managed) restored: 4120 file(s), 312.4 MiB (hashes verified)
[ok]   master_key       the backup holds no key; the live data directory's key matches its fingerprint
[ok]   server           quiescent server answered on 127.0.0.1:49479 after 6.8s (0 migration(s) applied at start)
[ok]   schema           schema v66 as in the backup
[ok]   doctor           14 check(s), 0 warning(s), none failed
[ok]   counts           9 collection(s), 18204 record(s), 311 storage object(s): all as recorded in the manifest
[ok]   records          45 record(s) read back from 9 collection(s)
[ok]   blobs            20 blob(s) read and hashed: size and SHA-256 match
[skip] tls              no tls/ state in this backup (native ACME TLS is off)

RTO (restore + start + checks): 31.2s  (restore 11.9s, start 6.8s, checks 12.5s)
RPO (age of the backup):        2h 13m  (a restore loses what was written since)
drill PASSED: this backup restores
```

(The numbers are illustrative.) A failure names the check and the reason: `storage/blobs/ab/ab12… is corrupt (SHA-256
differs from the manifest)`, `MANIFEST.json is missing`, `collection orders has 118 record(s), the backup recorded
120`, `master key … does not match the backup`.

### 5.1 Cadence

| Drill | When | Where | Proves |
|---|---|---|---|
| **Scheduled** | weekly (`[backup] drill_schedule = "30 4 * * 0"`) or from cron | the server's own host | The newest local snapshot restores; RTO trend; counts and sample reads. |
| **After every upgrade** | each upgrade, before you delete the pre-upgrade snapshot ([upgrades.md](upgrades.md)) | same | The new binary restores and migrates the old backup. |
| **Off-site and escrow** | monthly | **a different host or a laptop**, from the off-site copy and the escrowed key | The copy you would really use is intact and decryptable, the escrow is the right key, the binary version on a fresh host works. `forgebase backup drill <fetched dir> --master-key <escrow file>`. |
| **Game day** | quarterly, or when the team or the topology changes | a new host, from nothing but the off-site copy, the escrow and the runbook | The whole procedure and its real RTO, including the parts a drill cannot see (getting a host, DNS, secrets, config). Time it; note what the runbook got wrong; fix the runbook. |
| **External PostgreSQL** | monthly | scratch server (see External PostgreSQL) | The dump loads. |

Scheduled drills three ways:

- `[backup] drill_schedule = "30 4 * * 0"` in `forgebase.toml` (UTC cron, off by default): the server runs
  `forgebase backup drill latest` as the job `system.backup_drill` in the schedule `backup-drill`.
- `POST /api/v1/system/backups/<id|latest>/drill` (administrators) queues one now; it answers `202 {job_id}`, and the
  job's result is the report.
- cron or a systemd timer on the host: `forgebase backup drill --json >> /var/log/forgebase-drill.jsonl` (the exit
  code is the verdict).

All three write the same history; read it with `GET /api/v1/system/backups/<id>/drills` or look at
`<data_dir>/backups/drills/*.json`. The drill needs the free disk of one backup: it runs on the same volume as your
data unless you pass `--work-dir`, so point `--work-dir` at another volume when the data disk is tight.

### 5.2 Watch it

Prometheus (read from the history on every scrape, whoever ran the drill):

| Metric | Meaning |
|---|---|
| `forgebase_backup_last_drill_success_timestamp_seconds` | Unix time the last **passed** drill finished (0: none has passed) |
| `forgebase_backup_last_drill_ok` | 1 if the latest drill passed, else 0 |
| `forgebase_backup_drill_duration_seconds{phase}` | the latest drill's wall time: `total` (the RTO), `restore`, `start`, `checks` |
| `forgebase_backup_drill_rpo_seconds` | the age of the backup the latest drill restored |
| `forgebase_backup_drills_total{outcome}` | drills run through the job since the process started |

Nothing is emitted until a first drill is on record. Alert on `time() - forgebase_backup_last_drill_success_timestamp_seconds > 8 * 86400`
and on `forgebase_backup_last_drill_ok == 0` in your own Prometheus (a built-in `backup_drill_stale` rule is a follow-up;
the built-in alert engine's signals do not include them yet).

### 5.3 What a drill does not cover

- It tests the **control database and the local blobs**. Tenant data on shard sets, objects in S3 and replicas are not
  exercised, and a cluster backup (`--cluster`) is refused; for those, restore on a scratch host by hand and run
  `forgebase doctor`.
- It restores on the **same binary and the same host class** that run it. A different CPU architecture, PostgreSQL
  major or ForgeBase version is a different test: run the drill there (§5.1, off-site and game day).
- Counts on PostgreSQL are a range (a physical backup is not tied to one instant). A bug that corrupts values without
  changing counts is for `doctor` (`amcheck`) and your own application checks.
- A passing drill is evidence about **that backup**. It says nothing about the next one: keep the schedule.

## 6. RTO and RPO

### 6.1 Set the targets

Start from what an hour of downtime and an hour of lost writes cost you, not from what is easy. Then check both can be
met:

| Target | You need |
|---|---|
| RPO of 24 h | one snapshot a day, off-site copy the same day |
| RPO of 1 h | hourly snapshots (watch their duration and disk), off-site copy per snapshot |
| RPO of minutes | not provided by snapshots; use an external PostgreSQL with provider PITR, and accept ForgeBase's dump as the secondary copy |
| RTO of 15 min | a warm standby host with the binary installed, the key mounted and the last snapshots fetched (or on shared storage); the drill's RTO under 5 min |
| RTO of hours | the runbook, the off-site copy, the key escrow and a rehearsed new-host procedure |

### 6.2 Measure them

`forgebase backup drill --json` reports both, per drill:

```sh
forgebase backup drill --json | jq '{backup: .backup_id, rto_seconds, rpo_seconds, phases, outcome}'
curl -s -H "Authorization: Bearer $TOKEN" \
  localhost:8090/api/v1/system/backups/latest/drills | jq '.items[] | {finished_at, outcome, rto_seconds, rpo_seconds}'
```

- **RPO as measured** is `rpo_seconds`: the age of that backup when the drill started. The worst case is the longest
  gap between two good snapshots (look at `backup list`), not the age of the newest one.
- **RTO as measured** is restore + start + checks on the drill's host: a **floor**. Real RTO adds what a drill on a
  warm host leaves out:

  ```text
  detection + decision + provisioning + fetch/decrypt of the off-site copy + restore + start + checks + cutover (DNS, load balancer)
  ```

  Get the extra terms from a game day and add them to the drill's number. For the fetch term, time
  `restic restore` or `aws s3 cp` of a real snapshot; for provisioning, your platform.
- RTO grows with the **size of the data** (restore and, on PostgreSQL, WAL replay) and not with the number of
  drills: chart `forgebase_backup_drill_duration_seconds{phase="total"}` and alert when it passes your RTO budget
  with margin: the drill that takes longer than the target is a failing target before it is a disaster.

### 6.3 External PostgreSQL

Measure your provider's restore (snapshot to a running instance) separately and add ForgeBase's start; ForgeBase's
manual drill of External PostgreSQL gives the dump's restore time. The RPO is the provider PITR window you configured.

## 7. Checklist

- [ ] Snapshots on a schedule; `backup_stale` is not firing; retention keeps enough history to go back before a
      slow-burning corruption.
- [ ] An encrypted off-site copy of every snapshot (or at least daily), on write-once or versioned storage.
- [ ] `auth/master.key` escrowed apart from the backups, by two people, with its fingerprint written down.
- [ ] `forgebase.toml` and secrets reproducible from version control and your secret store.
- [ ] `[backup] drill_schedule` set (or a cron drill), the last drill passed this week, the metric is graphed.
- [ ] A drill from the **off-site copy with the escrowed key**, on another host, in the last month.
- [ ] A game day in the last quarter; the RTO measured there is at or under the target.
- [ ] This runbook's commands were run by someone other than the person who wrote them.
