# ForgeBase on Kubernetes: plain manifests (Kustomize)

The same pod spec as the [Helm chart](../helm/forgebase/README.md), as manifests you
can read, diff and patch. `docs/deployment.md` §9 explains the design: **one
replica** (ForgeBase has a single writer), non-root, read-only root filesystem,
probes on `/health` and `/health/ready`.

```text
deploy/kubernetes/
├── base/                     ServiceAccount, Services, StatefulSet (1 replica, 1 RWO PVC)
├── overlays/
│   ├── bundled-postgres/     the default: bundled PostgreSQL in the pod, /dev/shm
│   ├── sqlite/               SQLite on the PVC, smaller requests
│   └── external-postgres/    your PostgreSQL; URL from the Secret forgebase-database
├── components/
│   ├── ingress/              Ingress with cert-manager TLS (edit host and issuer)
│   ├── networkpolicy/        ingress from your controller, egress DNS + HTTPS
│   └── backup/               nightly `forgebase backup` CronJob + exec-only RBAC
├── examples/all-components/  bundled-postgres plus every component
└── validate.sh               kustomize build + kubeconform for all of the above
```

## Use

```sh
kubectl create namespace forgebase
kubectl apply -k deploy/kubernetes/overlays/bundled-postgres -n forgebase   # or sqlite, external-postgres
kubectl -n forgebase rollout status statefulset/forgebase
kubectl -n forgebase port-forward svc/forgebase 8090:8090                   # http://127.0.0.1:8090/admin/
```

The first start takes a while (engine unpack, `initdb`, migrations); the pod is not
ready until it is done (the startup probe allows 15 minutes). With no administrator
the server is in setup mode: `kubectl -n forgebase logs forgebase-0 | grep 'First run'`
shows the one-time link. To create the administrator without it, run
`forgebase admin create --email … --password-stdin` in the pod:

```sh
printf '%s\n' "$PASSWORD" | kubectl -n forgebase exec -i forgebase-0 -- forgebase admin create --email you@example.com --password-stdin
```

For your own deployment, copy an overlay into your repository and point `resources` at
this directory (or at the Git URL), then set the namespace, the image tag, the storage
class and `public_url` there:

```yaml
resources: [github.com/farzanshibu/forgebase//deploy/kubernetes/overlays/bundled-postgres?ref=v0.3.4]
namespace: forgebase
images:
  - name: ghcr.io/farzanshibu/forgebase
    newTag: "0.3.4"
patches:
  - target: {kind: StatefulSet, name: forgebase}
    patch: |-
      - op: add
        path: /spec/volumeClaimTemplates/0/spec/storageClassName
        value: fast-ssd
```

## Overlays

* **bundled-postgres**: `forgebase.toml` with `backend = "postgres"`, `mode = "managed"`,
  plus a patch that mounts a 256 Mi memory-backed emptyDir at `/dev/shm`.
* **sqlite**: `backend = "sqlite"`, requests 100m / 256 Mi, 5 Gi volume. For a `-lite` image
  (not published by the release workflow) see the commented `images:` block.
* **external-postgres**: `mode = "external"` and `FORGEBASE_DATABASE_URL` from the Secret
  `forgebase-database`, key `url`. Create it yourself (`secret.example.yaml` is
  deliberately not part of the kustomization):

  ```sh
  kubectl -n forgebase create secret generic forgebase-database \
    --from-literal=url='postgres://forgebase:…@db.example.com:5432/forgebase?sslmode=require'
  ```

`forgebase.toml` of each overlay becomes the ConfigMap `forgebase-config-<hash>`: a
config change renames it, which rolls the pod. No secrets in it. Unknown keys stop
the server at startup.

## Components

Add them to an overlay: `components: [../../components/ingress]`. They carry
placeholders (`forgebase.example.com`, `letsencrypt-prod`, `ingress-nginx`, a database
CIDR): edit them. The backup component runs `forgebase backup` inside the pod with
`kubectl exec`, because another pod cannot attach to the bundled PostgreSQL (see the
chart README, "Backups"). `auth/master.key` is not in backups: copy it out once.

## What these manifests do not do

No Secret generator (nothing sensitive is committed), no Gateway API route, no
ServiceMonitor, no PodDisruptionBudget (with one pod `minAvailable: 1` only blocks
node drains): use the Helm chart for those, or write them against the Service
`forgebase` (port `http`, 8090). Pin `replicas: 1` and never use `ReadWriteMany`:
`validate.sh` checks both.

## Validate

```sh
go install github.com/yannh/kubeconform/cmd/kubeconform@latest
deploy/kubernetes/validate.sh
```
