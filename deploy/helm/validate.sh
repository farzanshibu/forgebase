#!/usr/bin/env bash
# Validates the ForgeBase Helm chart: helm lint and helm template for every
# values file in forgebase/ci/, the rendered manifests against the Kubernetes
# and CRD JSON schemas (kubeconform), and a few renders that must fail.
#
#   deploy/helm/validate.sh
#
# Needs helm (3.14+ or 4) and kubeconform on PATH. The CRD schemas
# (ServiceMonitor, HTTPRoute) come from the datreeio CRDs catalog over HTTPS.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chart="$here/forgebase"
k8s_version="${KUBERNETES_VERSION:-1.33.0}"
crd_catalog='https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json'

for tool in helm kubeconform; do
  command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 2; }
done

work="$(mktemp -d)"
# Schemas are downloaded once per run (and once per machine with KUBECONFORM_CACHE).
cache="${KUBECONFORM_CACHE:-$work/schemas}"
mkdir -p "$cache"
trap 'rm -rf "$work"' EXIT
fail=0

echo "== helm lint (default values)"
helm lint --strict --kube-version "$k8s_version" "$chart"

for values in "$chart"/ci/*.yaml; do
  name="$(basename "$values" .yaml)"
  echo "== $name"
  helm lint --strict --kube-version "$k8s_version" "$chart" -f "$values" >/dev/null
  helm template forgebase "$chart" --kube-version "$k8s_version" --namespace forgebase -f "$values" >"$work/$name.yaml"
  kubeconform -strict -summary -cache "$cache" -kubernetes-version "$k8s_version" \
    -schema-location default -schema-location "$crd_catalog" \
    "$work/$name.yaml"
done

# Renders that must be refused.
expect_fail() {
  local why="$1"; shift
  if helm template forgebase "$chart" --kube-version "$k8s_version" "$@" >/dev/null 2>"$work/err.txt"; then
    echo "FAIL: rendered although $why" >&2
    fail=1
  else
    echo "ok (refused): $why"
  fi
}
echo "== negative checks"
expect_fail "replicaCount is 2 (single writer)"              --set replicaCount=2
expect_fail "two access modes (single writer)"               --set 'persistence.accessModes={ReadWriteOnce,ReadWriteMany}'
expect_fail "ReadWriteMany access mode"                      --set 'persistence.accessModes={ReadWriteMany}'
expect_fail "lite image with the bundled PostgreSQL"         --set image.variant=lite
expect_fail "external PostgreSQL without a URL"              --set database.postgres.mode=external
expect_fail "postgres mode with sqlite"                      --set database.backend=sqlite --set database.postgres.mode=external
expect_fail "admin bootstrap without a password"             --set admin.bootstrap.enabled=true --set admin.bootstrap.email=a@example.com
expect_fail "ServiceMonitor without credentials"             --set serviceMonitor.enabled=true
expect_fail "HTTPRoute without parentRefs"                   --set httpRoute.enabled=true
expect_fail "backup without persistence"                     --set backup.enabled=true --set persistence.enabled=false
expect_fail "unknown top-level value"                        --set replicas=1
expect_fail "unknown opsAccess"                              --set opsAccess=everyone

exit "$fail"
