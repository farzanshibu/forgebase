# ForgeBase Helm chart

Runs [ForgeBase](https://github.com/farzanshibu/forgebase), a self-hosted
backend-as-a-service in one binary, on Kubernetes: one StatefulSet pod with its
data directory on a PVC, a Service, and optional Ingress or Gateway API route,
metrics, network policy and backups.

```sh
helm install forgebase ./deploy/helm/forgebase --namespace forgebase --create-namespace
kubectl -n forgebase port-forward svc/forgebase 8090:8090
```

The chart is in this repository only (not published to a chart repository yet).
Plain manifests for Kustomize users are in [`deploy/kubernetes`](../../kubernetes/README.md).
Background, flags and configuration keys: [`docs/deployment.md`](../../../docs/deployment.md).

## Why one replica

ForgeBase has a **single writer**. One process owns one data directory: it
holds `.forgebase.lock`, runs the bundled PostgreSQL (or the SQLite file) and the
background workers, keeps rate limits in memory, and stores objects and
`auth/master.key` on local disk. Two pods on the same data would corrupt it or
refuse to start; two pods on different data would be two different servers. The
read replicas of ADR-0029 (`forgebase node`) need TLS between hosts that this
version does not set up, so the chart does not offer them.

The chart enforces this:

* the StatefulSet has `replicas: 1`, written into the template;
* there is no `replicaCount` value, and `values.schema.json` rejects
  `replicaCount` other than `1` and any unknown top-level key;
* `persistence.accessModes` must be exactly one of `ReadWriteOnce` or
  `ReadWriteOncePod`;
* a StatefulSet never runs two pods of the same ordinal, and the default
  `RollingUpdate` stops the old pod before it starts the new one.

Consequences to plan for: an upgrade or a config change restarts the pod, so
expect a short outage (the startup probe allows up to 15 minutes for migrations);
if a node dies, Kubernetes does not move a StatefulSet pod until the node is
declared gone (delete the pod, or the node, to force it); and the storage class
decides how fast the volume re-attaches.

## Database modes

| Mode | Values | Where the data lives |
|---|---|---|
| Bundled PostgreSQL (default) | `database.backend=postgres`, `database.postgres.mode=managed` | Inside the pod, on the PVC, behind a Unix socket. Needs the standard image. |
| External PostgreSQL | `database.postgres.mode=external`, URL from a Secret | Your server. The PVC still holds object storage, `auth/master.key` and backups. |
| SQLite | `database.backend=sqlite` | One file on the PVC. Works with the standard image and the `-lite` image. |

```sh
# External PostgreSQL
kubectl -n forgebase create secret generic forgebase-db \
  --from-literal=url='postgres://forgebase:…@db.example.com:5432/forgebase?sslmode=require'
helm install forgebase ./deploy/helm/forgebase -n forgebase \
  --set database.postgres.mode=external --set database.postgres.external.existingSecret=forgebase-db
```

The data directory records its engine in `forgebase.backend`, and a setting that
contradicts it stops startup. Choose once, and move with `forgebase db convert`.

**The `-lite` image.** The release workflow publishes `ghcr.io/farzanshibu/forgebase`
(the standard image, tags `0.3.4`, `0.3`, `sha-…`) and no `-lite` image. To use one,
build the Dockerfile without `ENGINE_ARCHIVE`, push it as `<repository>:<version>-lite`
and set `image.variant=lite` (and `image.repository` if it lives elsewhere). The
standard image also runs SQLite (`database.backend=sqlite`) and is what you want
unless image size matters. The chart refuses `variant=lite` with the bundled PostgreSQL.

## Security context and the bundled PostgreSQL

The pod runs as uid/gid 10001 (PostgreSQL refuses to run as root), with
`runAsNonRoot`, seccomp `RuntimeDefault`, `allowPrivilegeEscalation: false`, all
capabilities dropped and a **read-only root filesystem**. It meets the
`restricted` Pod Security Standard. Writable paths:

| Path | Volume | Why |
|---|---|---|
| `/data` | the PVC | database or PostgreSQL cluster, extracted engine (`postgres/engine`, about 30 MB), Unix socket (`postgres/run`), storage, keys, backups |
| `/tmp` | emptyDir (512 Mi) | scratch; PostgreSQL's fallback socket directory |
| `/dev/shm` | memory emptyDir (`shm.sizeLimit`, 256 Mi) | PostgreSQL shared memory; only with the bundled engine. Docker and Kubernetes default to 64 MB, which parallel queries outgrow. It counts against the memory limit. |

`fsGroup: 10001` with `fsGroupChangePolicy: OnRootMismatch` makes the empty volume
writable once and never walks it again. Do not use `Always`: a recursive change
would add group bits to `roles.secret` (0600) and the PostgreSQL data directory
(0700), which ForgeBase and PostgreSQL then refuse. If your CSI driver ignores
`fsGroup`, pre-provision the volume owned by 10001 (an `existingClaim`).

The chart sets `enableServiceLinks: false`. Kubernetes would otherwise inject
`FORGEBASE_PORT=tcp://…` for a Service called `forgebase`, and the binary rejects
unknown `FORGEBASE_*` variables.

## Configuration and secrets

`forgebase.toml` is rendered into a ConfigMap from a few first-class values
(`publicUrl`, `opsAccess`, `trustForwardedFor`, `logLevel`, `database.*`, `resp.*`)
deep-merged with the free-form `config` value, which wins. Keys are those of
[`forgebase.example.toml`](../../../forgebase.example.toml). The binary treats an
unknown key as an error, so a typo stops the pod at startup (`kubectl logs`).
Changing the config rolls the pod (checksum annotation).

```yaml
config:
  database:
    postgres:
      pool_size: 32
      settings:
        shared_buffers: 512MB
  storage:
    driver: s3
    s3_bucket: forgebase-blobs
  billing:
    enabled: false
```

**Secrets never go into the ConfigMap.** They come from Secrets, referenced by
name (`existingSecret`), and reach the pod as environment variables:

| Secret | Value | Becomes |
|---|---|---|
| `database.postgres.external.existingSecret` | PostgreSQL URL | `FORGEBASE_DATABASE_URL` |
| `s3.existingSecret` | access key, secret key, optional session token | `FORGEBASE_STORAGE_S3_ACCESS_KEY`, `…_SECRET_KEY`, `…_SESSION_TOKEN` |
| `admin.bootstrap.existingSecret` | administrator password | `ADMIN_PASSWORD` of the init container only |
| `serviceMonitor.authorization.secretName` | admin API key | Prometheus' bearer token |
| `tls.existingSecret` | certificate and key | mounted at `/etc/forgebase-tls` |

An inline value (`…external.url`, `admin.bootstrap.password`) makes the chart write
a Secret, but the value then sits in your values file and in Helm's release record:
use it for tests only. Everything else the ForgeBase UI stores (provider keys,
billing) is sealed with `auth/master.key` inside the data directory.

## First administrator

A server with no administrator is in *setup mode* and prints a one-time setup link
to standard output: `kubectl logs forgebase-0 | grep 'First run'` (replace the
host and port with your public address). Anyone who can read the pod's logs can
read it. To avoid that, bootstrap the administrator:

```sh
kubectl -n forgebase create secret generic forgebase-admin --from-literal=password='at-least-10-characters'
helm upgrade --install forgebase ./deploy/helm/forgebase -n forgebase \
  --set admin.bootstrap.enabled=true --set admin.bootstrap.email=you@example.com \
  --set admin.bootstrap.existingSecret=forgebase-admin
```

An init container runs `forgebase init --admin-email … --admin-password-stdin` with
the password piped on standard input. `init` migrates the database and keeps an
existing administrator, so it is safe on every restart. After the first login,
remove the Secret or rotate the password in the dashboard.

## Probes

| Probe | Path | Meaning | Timing |
|---|---|---|---|
| startup | `/health/ready` | database reachable and fully migrated, storage writable | every 5 s, 180 failures (15 min): first start extracts the engine, runs `initdb` and every migration |
| liveness | `/health` | the process answers HTTP (it does not look at the database, so a database outage does not restart the pod) | every 10 s, 6 failures |
| readiness | `/health/ready` | as startup; fails while the bundled PostgreSQL is restarting | every 10 s, 3 failures |

Both endpoints are open to everybody regardless of `opsAccess`.

## Exposing it

* **Ingress** (`ingress.enabled`): set `className`, `hosts`, and either `tls` with a
  secret name or `certManager.clusterIssuer` / `certManager.issuer`, which adds the
  cert-manager annotation (and a `tls` block for all hosts when `tls` is empty). Set
  `publicUrl`. Realtime (WebSocket, SSE) connections last for hours: raise the
  controller's read timeout (ingress-nginx annotations are in `values.yaml`). Set
  `trustForwardedFor: true` only if the controller appends the client address to
  `X-Forwarded-For`, and pair it with a `networkPolicy` so nobody else can reach the
  pod: otherwise a client can spoof its address and escape the rate limits. Leave
  it `false` and all clients share one rate-limit bucket.
* **Gateway API** (`httpRoute.enabled`): an `HTTPRoute` that attaches to the Gateways in
  `parentRefs`. TLS is configured on the Gateway's listener, which this chart does not
  manage.
* **Native TLS** (`tls.mode`): the binary is getting a `[tls]` section (`off`,
  `files`, `acme`). The values are wired (the section is rendered when `mode` is not
  `off`, with `tls.settings` merged in and `tls.existingSecret` mounted at
  `/etc/forgebase-tls`), but ForgeBase 0.3.4 rejects an unknown `[tls]` section, so use
  them only with a release that has it, and set `probes.scheme` to match. The default
  is TLS at the Ingress.
* The plain HTTP Service stays inside the cluster. Do not publish the RESP port
  (`resp.exposeService`) outside a trusted network: it has no TLS.

## Monitoring

`serviceMonitor.enabled` creates a Prometheus Operator `ServiceMonitor` for
`/metrics` (metrics are listed in `docs/deployment.md` §5.1). By default
`/metrics` answers administrators and callers on the same host only (`opsAccess:
loopback`), which excludes another pod. Create an admin service-account API key
with the `system:read` scope and give its Secret to `serviceMonitor.authorization`:

```sh
kubectl -n forgebase create secret generic forgebase-metrics --from-literal=token='fb_…'
helm upgrade forgebase ./deploy/helm/forgebase -n forgebase --reuse-values \
  --set serviceMonitor.enabled=true --set serviceMonitor.authorization.secretName=forgebase-metrics
```

The chart refuses `serviceMonitor.enabled` with neither credentials nor
`unauthenticated` (which needs `opsAccess=public`).

## Backups

`backup.enabled` creates a CronJob (default `17 3 * * *`) and a ServiceAccount whose
Role may `get` and `exec` into **this release's pod and nothing else**. The job runs

```sh
kubectl exec forgebase-0 -c forgebase -- forgebase backup --keep-last 14 --max-age-days 30
```

It uses `exec` rather than a second pod mounting the PVC because the CLI attaches to
the running bundled PostgreSQL through its `postmaster.pid`, and another pod cannot
see that process. The backup is online and consistent on every backend (for an
external PostgreSQL server it is a logical backup of that server). The exec'd
process counts against the server container's memory limit.

* `backup.destination.type=snapshots` (default): snapshots in `/data/backups/snapshots`
  on the data PVC, with the retention flags above.
* `backup.destination.type=volume`: a second PVC (kept on `helm uninstall`) mounted at
  `/backups`; each run writes `/backups/<UTC time>` and keeps the newest `keepLast`.

**A backup on the same storage is not a disaster backup.** Copy snapshots off the
cluster, put the backup PVC on another storage class, or use CSI VolumeSnapshots of
the data PVC. Restoring needs a stopped server (it takes the data-directory lock). The outline,
which this chart's authors have not run against a cluster, is: scale the StatefulSet
to 0 (`kubectl -n forgebase scale statefulset/forgebase --replicas=0`), run a
one-off pod from the same image, with the same security context and the data PVC
(`data-forgebase-0`) mounted at `/data`, that runs `forgebase restore latest --force`
(see `docs/database.md` §5 for `restore` and `backup verify`), delete that pod, and
scale back to 1. Run `forgebase backup verify latest` first.

**The master key is not in the backup.** `auth/master.key` seals every stored
secret. Copy it out once, to a password manager or a sealed secret, apart from the
backups:

```sh
kubectl -n forgebase exec forgebase-0 -- cat /data/auth/master.key > master.key
```

(`backup.includeMasterKey=true` puts it in each backup, next to the data: no protection.)

## Network policy

`networkPolicy.enabled` allows ingress to the HTTP port from `networkPolicy.ingress.from`
(empty means anywhere, so name your ingress controller's and Prometheus' namespaces).
`networkPolicy.egress.enabled` restricts egress to DNS, outbound HTTPS (OAuth/OIDC,
SMS, S3, webhooks, payment providers) and `networkPolicy.egress.extra`, for example
the external database. The backup job is not affected: it only talks to the API server
and runs in a different pod.

## Disruptions

`podDisruptionBudget.enabled` (off by default) creates a PDB with
`maxUnavailable: 1`. With a single pod, `minAvailable: 1` would block every voluntary
eviction and hang node drains, so the default budget is a statement of intent rather
than a lock. Set `minAvailable: 1` if you want drains to stop and wait for you.

## Validation

[`../validate.sh`](../validate.sh) lints and renders the chart with every file in
[`ci/`](ci), validates the output with kubeconform (Kubernetes and CRD schemas), and
checks that invalid combinations (two replicas, `ReadWriteMany`, `lite` with the
bundled PostgreSQL, and so on) are refused. CI runs it.

```sh
go install github.com/yannh/kubeconform/cmd/kubeconform@latest
deploy/helm/validate.sh
```

## Values

| Key | Default | Description |
|---|---|---|
| `nameOverride` | `""` | Replaces the chart name in resource names. |
| `fullnameOverride` | `""` | Replaces the whole resource name. |
| `commonLabels` | `{}` | Labels added to every object. |
| `image.repository` | `"ghcr.io/farzanshibu/forgebase"` | Image repository. |
| `image.tag` | `""` | Image tag. Empty: the chart's `appVersion`. |
| `image.variant` | `"standard"` | `standard` (bundled PostgreSQL engine) or `lite` (SQLite only, tag gets a `-lite` suffix). The release workflow publishes only the standard image. |
| `image.digest` | `""` | Pin by digest (`sha256:…`); the tag stays for readability. |
| `image.pullPolicy` | `"IfNotPresent"` | Image pull policy. |
| `imagePullSecrets` | `[]` | Pull secrets for a private registry. |
| `database.backend` | `"postgres"` | `postgres` or `sqlite`. The data directory's `forgebase.backend` marker wins over this value. |
| `database.postgres.mode` | `"managed"` | `managed` (bundled engine in the pod) or `external` (your server). |
| `database.postgres.external.existingSecret` | `""` | Secret holding the external database URL. Preferred. |
| `database.postgres.external.existingSecretKey` | `"url"` | Key in that Secret. |
| `database.postgres.external.url` | `""` | Inline URL; the chart writes a Secret. The URL ends up in your values and in Helm's release record. |
| `shm.sizeLimit` | `"256Mi"` | Size of the memory-backed `/dev/shm` for the bundled PostgreSQL. Counts against the memory limit. |
| `publicUrl` | `""` | `server.public_url`: the address users reach the server at (no path). |
| `opsAccess` | `"loopback"` | `server.ops_access`: `loopback`, `admin` or `public` for `/metrics` and `/api/system/*`. |
| `trustForwardedFor` | `false` | `auth.trust_forwarded_for`. Only behind your own proxy that appends the client address. |
| `logLevel` | `"info"` | `log.level`. The log format is always `json`. |
| `containerPort` | `8090` | Port the server listens on. |
| `resp.enabled` | `false` | Redis protocol listener (no TLS; clients must `AUTH`). |
| `resp.port` | `6380` | RESP port. |
| `resp.exposeService` | `false` | Publish the RESP port on the Service. |
| `tls.mode` | `"off"` | Native TLS in the binary: `off`, `files` or `acme`. Rendered as a `[tls]` section only when not `off`; the 0.3.4 binary rejects that section. Default: TLS at the Ingress or Gateway. |
| `tls.settings` | `{}` | Further keys of the `[tls]` table. |
| `tls.existingSecret` | `""` | `kubernetes.io/tls` Secret mounted at `/etc/forgebase-tls`. |
| `config` | `{}` | Free-form `forgebase.toml` keys (deep-merged over the generated ones). No secrets. |
| `s3.existingSecret` | `""` | Secret with S3 credentials for `storage.driver = "s3"`. |
| `s3.accessKeyKey` | `"access-key"` | Key of the access key. |
| `s3.secretKeyKey` | `"secret-key"` | Key of the secret key. |
| `s3.sessionTokenKey` | `""` | Key of the optional session token; empty: none. |
| `admin.bootstrap.enabled` | `false` | Create the first administrator with an init container (`forgebase init --admin-email … --admin-password-stdin`). |
| `admin.bootstrap.email` | `""` | Administrator email. |
| `admin.bootstrap.existingSecret` | `""` | Secret holding the password (10+ characters). Preferred. |
| `admin.bootstrap.existingSecretKey` | `"password"` | Key in that Secret. |
| `admin.bootstrap.password` | `""` | Inline password; the chart writes a Secret. |
| `persistence.enabled` | `true` | Keep the data directory on a PVC. Off: emptyDir, lost with the pod. |
| `persistence.existingClaim` | `""` | Use a PVC you created instead of a volumeClaimTemplate. |
| `persistence.storageClass` | `""` | Storage class; empty: the cluster default. |
| `persistence.accessModes` | `["ReadWriteOnce"]` | Exactly one of `ReadWriteOnce` or `ReadWriteOncePod`. |
| `persistence.size` | `"10Gi"` | PVC size. |
| `persistence.annotations` | `{}` | PVC annotations. |
| `persistence.retentionPolicy.whenDeleted` | `"Retain"` | `Retain` or `Delete` the PVC when the StatefulSet is deleted. |
| `persistence.retentionPolicy.whenScaled` | `"Retain"` | `Retain` or `Delete` the PVC when scaled down. |
| `resources` | `*see values.yaml*` | Container resources. |
| `terminationGracePeriodSeconds` | `90` | Time between SIGTERM and SIGKILL. |
| `podSecurityContext` | `*see values.yaml*` | Pod security context (non-root uid 10001, `fsGroup`, seccomp `RuntimeDefault`). |
| `securityContext` | `*see values.yaml*` | Container security context (no privilege escalation, read-only root, drop `ALL`). |
| `probes.scheme` | `"HTTP"` | `HTTP` or `HTTPS` for all three probes. |
| `probes.startup.path` | `"/health/ready"` | Startup probe path. |
| `probes.startup.periodSeconds` | `5` | Startup probe period. |
| `probes.startup.timeoutSeconds` | `3` | Startup probe timeout. |
| `probes.startup.failureThreshold` | `180` | Startup probe failures allowed (180 × 5 s = 15 minutes). |
| `probes.liveness.path` | `"/health"` | Liveness probe path. |
| `probes.liveness.periodSeconds` | `10` | Liveness probe period. |
| `probes.liveness.timeoutSeconds` | `5` | Liveness probe timeout. |
| `probes.liveness.failureThreshold` | `6` | Liveness failures before a restart. |
| `probes.readiness.path` | `"/health/ready"` | Readiness probe path. |
| `probes.readiness.periodSeconds` | `10` | Readiness probe period. |
| `probes.readiness.timeoutSeconds` | `5` | Readiness probe timeout. |
| `probes.readiness.failureThreshold` | `3` | Readiness failures before the pod leaves the Service. |
| `serviceAccount.create` | `true` | Create the ServiceAccount. |
| `serviceAccount.name` | `""` | ServiceAccount name; empty: the fullname. |
| `serviceAccount.annotations` | `{}` | ServiceAccount annotations. |
| `podAnnotations` | `{}` | Pod annotations. |
| `podLabels` | `{}` | Pod labels. |
| `priorityClassName` | `""` | Pod priority class. |
| `nodeSelector` | `{}` | Node selector. |
| `tolerations` | `[]` | Tolerations. |
| `affinity` | `{}` | Affinity. |
| `topologySpreadConstraints` | `[]` | Topology spread constraints. |
| `updateStrategy` | `{"type": "RollingUpdate"}` | StatefulSet update strategy. |
| `extraEnv` | `[]` | Extra container environment. Unknown `FORGEBASE_*` names stop startup. |
| `extraEnvFrom` | `[]` | Extra `envFrom` sources. |
| `extraVolumes` | `[]` | Extra pod volumes. |
| `extraVolumeMounts` | `[]` | Extra mounts of the server container. |
| `initContainers` | `[]` | Extra init containers. |
| `sidecars` | `[]` | Extra containers. |
| `service.type` | `"ClusterIP"` | `ClusterIP`, `NodePort` or `LoadBalancer`. |
| `service.port` | `8090` | Service port. |
| `service.annotations` | `{}` | Service annotations. |
| `service.loadBalancerClass` | `""` | `loadBalancerClass` (LoadBalancer). |
| `service.loadBalancerSourceRanges` | `[]` | Allowed source ranges (LoadBalancer). |
| `service.externalTrafficPolicy` | `""` | `Cluster` or `Local` (LoadBalancer, NodePort). |
| `service.nodePort` | `null` | Fixed node port (NodePort, LoadBalancer). |
| `ingress.enabled` | `false` | Create an Ingress. |
| `ingress.className` | `""` | Ingress class. |
| `ingress.annotations` | `{}` | Ingress annotations (for example nginx timeouts for realtime). |
| `ingress.hosts` | `*see values.yaml*` | Hosts and paths. |
| `ingress.tls` | `[]` | TLS blocks. Empty with cert-manager set: one block for all hosts, secret `<fullname>-tls`. |
| `ingress.certManager.clusterIssuer` | `""` | Adds `cert-manager.io/cluster-issuer`. |
| `ingress.certManager.issuer` | `""` | Adds `cert-manager.io/issuer`. |
| `httpRoute.enabled` | `false` | Create a Gateway API `HTTPRoute`. |
| `httpRoute.annotations` | `{}` | HTTPRoute annotations. |
| `httpRoute.labels` | `{}` | HTTPRoute labels. |
| `httpRoute.parentRefs` | `[]` | Gateways to attach to (required). |
| `httpRoute.hostnames` | `[]` | Hostnames. |
| `httpRoute.pathPrefix` | `"/"` | Path prefix matched. |
| `httpRoute.timeouts` | `{}` | Rule `timeouts`, for example `{request: 0s}`. |
| `podDisruptionBudget.enabled` | `false` | Create a PodDisruptionBudget. |
| `podDisruptionBudget.maxUnavailable` | `1` | Evictions allowed. With one pod, `minAvailable: 1` blocks node drains. |
| `podDisruptionBudget.minAvailable` | `null` | When set, used instead of `maxUnavailable`. |
| `serviceMonitor.enabled` | `false` | Create a Prometheus Operator `ServiceMonitor`. |
| `serviceMonitor.namespace` | `""` | Namespace of the ServiceMonitor; empty: the release namespace. |
| `serviceMonitor.labels` | `{}` | Labels (for the Prometheus `serviceMonitorSelector`). |
| `serviceMonitor.interval` | `"15s"` | Scrape interval. |
| `serviceMonitor.scrapeTimeout` | `"10s"` | Scrape timeout. |
| `serviceMonitor.path` | `"/metrics"` | Metrics path. |
| `serviceMonitor.authorization.secretName` | `""` | Secret with an admin service-account API key (`system:read`), sent as a bearer token. |
| `serviceMonitor.authorization.secretKey` | `"token"` | Key in that Secret. |
| `serviceMonitor.unauthenticated` | `false` | Scrape without credentials; needs `opsAccess=public`. |
| `serviceMonitor.relabelings` | `[]` | Relabelings. |
| `serviceMonitor.metricRelabelings` | `[]` | Metric relabelings. |
| `networkPolicy.enabled` | `false` | Create a NetworkPolicy. |
| `networkPolicy.ingress.from` | `[]` | Peers allowed to reach the HTTP port; empty: any source. |
| `networkPolicy.ingress.respFrom` | `[]` | Peers allowed to reach the RESP port. |
| `networkPolicy.egress.enabled` | `false` | Restrict egress to DNS plus the rules below. |
| `networkPolicy.egress.allowHttps` | `true` | Allow outbound 443 (OAuth/OIDC, SMS, S3, webhooks). |
| `networkPolicy.egress.extra` | `[]` | Further egress rules, for example the external database. |
| `networkPolicy.egress.dnsNamespaceSelector` | `*see values.yaml*` | Namespace selector of cluster DNS. |
| `networkPolicy.egress.dnsPodSelector` | `{"matchLabels": {"k8s-app": "kube-dns"}}` | Pod selector of cluster DNS. |
| `backup.enabled` | `false` | Create the backup CronJob and its RBAC. |
| `backup.schedule` | `"17 3 * * *"` | Cron schedule. |
| `backup.timeZone` | `""` | IANA time zone of the schedule. |
| `backup.keepLast` | `14` | Keep the newest N backups (0: no limit). |
| `backup.maxAgeDays` | `30` | Delete snapshots older than D days (`snapshots` only; 0: no limit). |
| `backup.noIndexes` | `false` | Leave derived index files out. |
| `backup.includeMasterKey` | `false` | Copy `auth/master.key` into the backup. Off: keep it apart. |
| `backup.destination.type` | `"snapshots"` | `snapshots` (on the data PVC) or `volume` (a second PVC at `/backups`). |
| `backup.destination.volume.existingClaim` | `""` | Use a PVC you created for `volume`. |
| `backup.destination.volume.storageClass` | `""` | Storage class of the backup PVC. |
| `backup.destination.volume.size` | `"20Gi"` | Size of the backup PVC. |
| `backup.destination.volume.accessModes` | `["ReadWriteOnce"]` | Access modes of the backup PVC. |
| `backup.concurrencyPolicy` | `"Forbid"` | CronJob concurrency policy. |
| `backup.startingDeadlineSeconds` | `600` | CronJob starting deadline. |
| `backup.activeDeadlineSeconds` | `7200` | Job deadline. |
| `backup.successfulJobsHistoryLimit` | `3` | Finished jobs kept. |
| `backup.failedJobsHistoryLimit` | `3` | Failed jobs kept. |
| `backup.image.repository` | `"registry.k8s.io/kubectl"` | kubectl image used for the exec. |
| `backup.image.tag` | `"v1.33.9"` | kubectl tag; match your cluster's minor version. |
| `backup.image.pullPolicy` | `"IfNotPresent"` | kubectl image pull policy. |
| `backup.resources` | `*see values.yaml*` | Resources of the kubectl container. |
| `backup.podAnnotations` | `{}` | Backup pod annotations. |
| `backup.nodeSelector` | `{}` | Backup pod node selector. |
| `backup.tolerations` | `[]` | Backup pod tolerations. |
