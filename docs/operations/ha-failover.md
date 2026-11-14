# High availability and failover

What ForgeBase does when a database node or a host fails, what it leaves to you, and the procedures for promoting a
replica, re-pointing nodes and avoiding two writers. The design is [ADR-0029](../adr/0029-postgres-extensions-replicas-sharding.md)
(§2 replicas, §2.8 failover, §3 sharding, §4 topology); deployment is in [deployment.md §9](../deployment.md#9-kubernetes).

**Read this first.** ForgeBase is **one server process and the only writer**. Replicas and shards are
PostgreSQL nodes the server manages; the API, the workers, `auth/master.key`, the object blobs and the configuration
live with the server's data directory, not on a replica. Two consequences drive everything below:

- A replica is a **read-capacity and fast-promote target while the control server is alive**. It is not a standby
  *server*: if the host that runs `forgebase serve` is lost, promoting a database somewhere else does not bring the
  API back. That is a [restore](disaster-recovery.md) (or a re-provision of the host with its disk), and the RTO is the
  restore's.
- **Failover is manual** (ADR-0029 §2.8). Safe automatic failover needs fencing and a third voter; ForgeBase has neither
  today, so it does not promote by itself. Automatic failover is planned with multi-process servers.

## 1. Pick the topology for what you need

| You want | Use | Failure it survives | It does not survive |
|---|---|---|---|
| Simplest, smallest | One server, bundled PostgreSQL or SQLite, snapshots | nothing automatic: restore | a lost host or disk (until restored) |
| More read capacity, a fast promote target | **Local replica** (same host, `postgres/nodes/<name>`) | a corrupt or killed primary cluster, a bad upgrade of the primary | the host |
| Protection from a lost *database host* while the API host lives | **Remote replica** (`forgebase node join` on another host), or an external replica | the database host | the API host |
| Survive a lost API host *and* database | Server on a platform that reattaches its volume (Kubernetes with a CSI volume, a VM with a network disk) **plus** off-site backups; or **external PostgreSQL** run by a provider or operator that fails over itself | host/node loss, if the volume survives | losing the volume (restore) |
| More write capacity | **Shards** (control + shard sets, ADR-0029 §3) | loss of one shard affects only its tenants | the control set's loss (everything) |

The rest of this page covers the replica and shard rows. For the external PostgreSQL row, failover is your provider's
or operator's job: see §7 (item 6) and §8.

## 2. What ForgeBase automates, and what it does not

| Automated | Not automated |
|---|---|
| Starting, supervising and restarting the bundled PostgreSQL (backoff; `/health/ready` fails while it is down) | Choosing to fail over. **Nothing promotes on its own.** |
| Streaming replication to local and remote replicas; replication slots with bounded WAL (`max_slot_wal_keep_size` 8 GB, idle slot timeout 1 day) | Detecting that the primary's *host* is dead: the lag monitor sees an unreachable node, and an alert can tell you; a human decides |
| Lag monitoring (`lagging` above 2 s or 64 MiB), taking a lagging replica out of rotation, **falling back to the primary** for a failed replica read | Fencing a primary that is unreachable but may still be running (you do it: power off, security group, `kubectl cordon` and delete the instance) |
| **One confirmed action** to promote: pre-check, fence the old primary when ForgeBase can reach it, replay to its last LSN, `pg_ctl promote -w`, re-point pools, swap roles, bump the topology epoch, rewrite `topology.json` | Promoting an **external primary** (refused: it cannot be fenced) |
| After a promote: other local replicas follow the new primary and timeline without a restart; remote ones get `follow` commands through their agent | Re-pointing **anything outside ForgeBase** at the new primary: direct logins over `[database.postgres] listen`, `psql` users, monitoring, a load balancer in front of the node |
| The old primary becomes `needs_rebuild`; **rebuild** rejoins it with `pg_rewind` or re-clones it | Rebuilding **external** replicas (confirm and run it yourself) |
| In-doubt cross-shard transactions are resolved at start and every minute | Moving the API server to another host (that is a restore or a re-provision) |
| Replicas are marked `needs_rebuild` after a restore and (managed ones) rebuilt after the start | Off-site copies, key escrow, or a standby *API server* |

## 3. Should you fail over?

Fail over a **database node** when ForgeBase's control server is up and the primary is **gone or unusable**:
a dead or wedged primary cluster, a disk failure under it, planned maintenance on its host. Do not fail over for a slow
query, a transient restart (the supervisor restarts the engine with backoff), or lag: the fallback to the primary
already covers failed replica reads.

Restore instead when the control server's host or its data directory is lost, when the data is **corrupt on every node**
(a replica replays corruption and a bad `DELETE`), or when there is no healthy replica.

Look first (administrator session, `/api/system/database/*`, or the dashboard's Database topology screen):

```sh
curl -s -H "Authorization: Bearer $TOKEN" localhost:8090/api/system/database/topology | jq '.sets[] | {name, kind, state}, (.nodes[] | {name, role, kind, state, lag})'
```

A replica must be `streaming` (not `lagging`, `needs_rebuild` or `unreachable`) to be a good target.

## 4. Promote a replica

Administrators only; with MFA enabled the call needs a step-up. Every call is audit-logged. Promote is the job
`database.promote`; it checkpoints before the fence, so a restart in the middle resumes it.

### 4.1 Pre-check

```sh
NODE=<node id>
curl -s -H "Authorization: Bearer $TOKEN" localhost:8090/api/system/database/nodes/$NODE/promote-check | jq
```

It shows the target's replayed LSN against the last known primary LSN (the **data-loss estimate**) and whether the
old primary is reachable. The dashboard's confirm dialog shows the same.

### 4.2 Fence, then promote

1. **Make sure the old primary cannot take writes.** If ForgeBase can reach it, the job stops it (fast shutdown) and, when
   it stopped cleanly, waits until the target has replayed past its shutdown checkpoint: **no acknowledged commit is
   lost**. If it is unreachable, ForgeBase cannot know whether it is dead or partitioned: **fence it yourself first**
   (power it off, remove its network path, stop the unit) and only then continue.
2. Promote with the node's name as the typed confirmation:

   ```sh
   curl -s -X POST -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     localhost:8090/api/system/database/nodes/$NODE/promote \
     -d '{"confirm": "<node name>"}'                       # add "accept_data_loss": true only when the old primary is down
   forgebase jobs watch <job id> --server http://localhost:8090 --token "$TOKEN"
   ```

   `accept_data_loss: true` says: *the old primary is down and I accept losing what the target had not replayed.* It is
   required exactly when the old primary did not stop cleanly. Never set it to make an error go away.

3. What the job does (ADR-0029 §2.8): pre-checks the target; fences the old primary (closes the writer pool, stops it);
   waits for the final LSN if it stopped cleanly; `pg_ctl promote -w`; points the writer and read pools at the new
   primary; swaps the roles in the control tables and bumps the **topology epoch**; rewrites `topology.json`; moves every
   other replica of the set onto the new primary (new slots, `primary_conninfo`, no restart); marks the old primary
   `needs_rebuild`.

### 4.3 If the promoted node is the control primary

Promoting the **control set's** primary needs a **restart of the server**: the control database is the server's own
connection pool, which cannot switch while it runs. `GET …/topology` shows `restart_required`; until the restart,
requests that touch control fail. Restart (`systemctl restart forgebase`, delete the pod). On start the server opens
`postgres/nodes/<name>` from `topology.json`, the promote job resumes from its checkpoint and succeeds, and the old
primary appears as a `needs_rebuild` node named `primary`. (A self-restarting `serve` is a follow-up in ADR-0029.)

### 4.4 Then rebuild the old primary

Do not start it by hand and do not point anything at it: its data directory is on a **diverged timeline**. Ask
ForgeBase to rejoin it:

```sh
curl -s -X POST -H "Authorization: Bearer $TOKEN" localhost:8090/api/system/database/nodes/$OLD/rebuild
```

It runs `pg_rewind` (initdb uses `--data-checksums`, so it is possible) and, if the engine has no `pg_rewind` or it
fails, moves the old data aside and re-clones. The node returns as a replica, `catching_up` then `streaming`.

## 5. Re-pointing

### 5.1 Inside ForgeBase: nothing to do

After a promote ForgeBase re-points its own writer and read pools, every managed replica (local or through its agent),
the topology tables and `topology.json`. **Clients of the API need nothing**: they talk to ForgeBase, not to PostgreSQL.

### 5.2 Outside ForgeBase: you do it

- **Direct logins and `psql`** over `[database.postgres] listen` (`forgebase db login`) connect to a node's address: move
  the address (DNS name, virtual IP, a Service) to the new primary, or give users the new one. Name nodes by DNS so this
  is one record.
- **External replicas** (`[[database.nodes]]` or declared through the API) are not rebuilt or re-followed by
  ForgeBase: after a promote in *their* PostgreSQL topology, point them at the new primary (`primary_conninfo`) yourself and
  re-check `system_identifier` (a changed one is `400 node_mismatch`).
- **Remote replicas** (`forgebase node join <server url> --token … --data-dir DIR`): the agent keeps an outbound
  long-poll to the server for commands, so it follows the server's **URL**: keep that URL stable (a DNS name).
  `forgebase node status` shows what the agent knows; `forgebase node run --data-dir DIR` resumes it after a restart of its host.
- **Monitoring and backups**: `forgebase backup --cluster` takes each set from its primary (or `[backup] source = "replica"`
  from a healthy replica): after a promote, take a fresh one.
- **Firewalls**: a remote replica's PostgreSQL port must reach the new primary and the server must reach the replica's
  `listen`; promotions that move the primary to another host change which pairs talk.

### 5.3 When the server itself must move

Moving `forgebase serve` to another host is a **restore or a re-provision**, not a failover: it needs `auth/master.key`,
`storage/`, `postgres/roles.secret`, the config and (bundled PostgreSQL) the cluster, none of which a remote replica
holds. Use [disaster-recovery.md §4](disaster-recovery.md#4-restore); if the disk or volume survives, attach it to the
new host and start there (the lock and `topology.json` come with it).

## 6. After a restore or a promote

1. `GET /api/system/database/topology`: every node `streaming`, the **epoch** went up by one for a promote, no
   `needs_rebuild` left that you did not expect. A **restore rewrites the primary's history**, so every replica of that
   set is `needs_rebuild`: managed replicas rebuild themselves after the server starts (`forgebase restore` of a cluster backup names the file that lists the
   pending work), external ones wait for your confirmation (`POST …/nodes/{id}/rebuild`).
2. `forgebase doctor`, `GET /health/ready`, and the Database screen.
3. Take a fresh `forgebase backup --cluster` (the old ones describe a topology that no longer exists), then
   `forgebase backup drill` on it ([disaster-recovery.md §5](disaster-recovery.md#5-drills); a single-set backup of the
   control set, since cluster backups are not drilled yet).
4. Update DNS, direct-login hosts, and the runbook.
5. Write down what happened and the measured promote time.

## 7. Split-brain: how to not have two writers

Two nodes accepting writes for the same data lose one side's commits. The rules:

1. **One server per data directory.** `.forgebase.lock` enforces one process; do not copy a data directory to a second host
   and start both. Never mount the same volume on two hosts (RWO exists for this; never `ReadWriteMany`).
2. **Fence before you promote.** If the old primary is unreachable, it may still be running. Power it off or cut its
   network **before** you tick "accept data loss". ForgeBase fences what it can reach; it cannot fence what it cannot.
3. **Never "just start" the old primary** after a promote. Its timeline diverged. Use `rebuild`. In particular, make sure the
   old host's init (systemd unit, container restart policy) does not start a `forgebase serve` that opens its **old**
   `postgres/data` as a primary: disable it, or let it start only after `topology.json` and the cluster are the promoted ones.
4. **Do not restore an old `topology.json`** or an old data directory over a promoted one without a deliberate plan: the topology
   file is how the server finds the control primary.
5. **Promotion is one action per set at a time** (`409 operation_in_progress`): do not work around it by promoting two replicas of one
   set by hand with `pg_ctl`.
6. **External primaries cannot be fenced by ForgeBase**: promote them with their own tooling (which must fence), then change
   `FORGEBASE_DATABASE_URL` (or the node row) and restart ForgeBase.

## 8. Kubernetes

The chart and the Kustomize base deploy **one StatefulSet pod with one PVC** (`replicas: 1`; the chart's schema rejects
any other value; `ReadWriteOnce` or `ReadWriteOncePod` only). There is no packaged replica topology (it needs TLS between
hosts, which the chart does not set up). So on Kubernetes "HA" means **reschedule plus reattach plus restore**, and:

- **RTO is bound by the platform.** A node failure leaves the pod `Terminating` until the node is declared gone; then the volume
  reattaches (a CSI-dependent number of minutes), the pod starts, PostgreSQL **replays WAL** and the startup probe waits
  (up to 15 minutes allowed). Measure it: kill a node in staging and time it.
- **Do not force-delete the pod** (`--force --grace-period=0`) while the node might still be running: that breaks the
  at-most-one guarantee, and two processes could then have the same volume open. Fence the node first (cordon, power off the instance, delete the node object), then
  delete the pod.
- **Plan outages for upgrades and config changes**: they restart the single pod (the chart can add a `PodDisruptionBudget` for voluntary
  evictions; `terminationGracePeriodSeconds` of 90 s lets it drain and checkpoint).
- **Spread the failure domains of the data, not of the pod.** Use a storage class with replication across zones, or CSI volume
  snapshots, **and** off-site backups ([disaster-recovery.md §3.2](disaster-recovery.md#32-3-2-1)): a PVC is one failure
  domain.
- **Database HA on Kubernetes** is best done by an operator (CloudNativePG, Crunchy, Zalando) or a managed service, with ForgeBase in
  **external** mode: `[database.postgres] mode = "external"`, the URL (the operator's read-write Service) from a Secret as
  `FORGEBASE_DATABASE_URL`. The operator promotes and fences; the Service name does not change, and ForgeBase reconnects. The pod
  still holds `auth/master.key` and object storage on its PVC (or use S3). External primaries are never promoted by ForgeBase.
- **Remote replicas** (`forgebase node join`) belong on hosts you manage, not in the single-pod chart.
- **Backups and drills**: the chart's CronJob runs `forgebase backup` in the pod; run the drill the same way
  (`kubectl exec forgebase-0 -- forgebase backup drill`) or set `[backup] drill_schedule` in the mounted `forgebase.toml`.
  The scratch directory needs the size of a snapshot free on the volume; `--work-dir` points it at an emptyDir.

## 9. Sharding

A shard is a set with its own primary (and replicas). Tenants live on one shard; the **control set** also holds identity,
policy, topology and everything global, and is shard 0.

- **Losing a shard primary** affects only that shard's tenants (their requests fail, `503`, while the rest serve). With a healthy
  replica of that shard and the control server alive, **promote it** exactly as in §4 (the shard's pools re-point without a
  restart; only the control primary needs one). Without one, restore that shard's data from `forgebase backup --cluster`.
- **Losing the control set** is losing the system: restore it, §4.3 if a replica exists.
- A shard on another host (`kind: remote`) is backed up **there** (`pg_basebackup` or `forgebase backup` of its own data
  directory): `backup --cluster` copies the sets this host runs and says so for the rest.
- A promote or restart **resolves in-doubt cross-shard transactions** (two-phase recovery at start and every minute);
  expect a brief window of `409`/`503` for tenants whose last write spanned shards.
- Rebalancing, drains and tenant moves are separate jobs (`database.shard.drain`, `database.tenant.move`, `database.rebalance`) and do
  not run during a promote of the same set (`409 operation_in_progress`).

## 10. Rehearse it

Failover you have never done is a guess. In staging, quarterly:

1. Build the topology you run in production (a local replica is enough to rehearse the steps).
2. Write load; `kill -9` the primary's postmaster; watch `/health/ready`, the supervisor restart, the fallback of replica reads.
3. Promote the replica (§4), time it from decision to the first successful write, check no acknowledged write is missing.
4. Rebuild the old primary (§4.4). Take `backup --cluster`; drill the control set.
5. Record: detection time, decision time, promote time, and the writes you lost with `accept_data_loss`. That is your database-failover RTO and RPO.

## 11. Checklist

- [ ] You know which row of §1 you are in and what it cannot survive.
- [ ] Nodes are addressed by DNS names; direct logins and monitors use them.
- [ ] Someone on call can fence a primary (cloud console, IPMI, `kubectl`) without asking permission.
- [ ] MFA on administrator accounts is understood: promote needs a step-up.
- [ ] Off-site backups and the master key escrow exist ([disaster-recovery.md](disaster-recovery.md)): replicas do not replace them.
- [ ] The alerting rules in [alerting-runbook.md](alerting-runbook.md) page someone for `disk_free_*` (a full primary disk breaks replication) and
      `queue_push_paused`, and you added your own for replica lag if you rely on read replicas.
- [ ] A rehearsal (§10) in the last quarter.
