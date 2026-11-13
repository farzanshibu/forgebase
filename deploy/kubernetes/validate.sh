#!/usr/bin/env bash
# Validates the kustomize base, overlays and example: `kubectl kustomize` must
# build each of them, and the output must pass kubeconform (Kubernetes schemas,
# strict).
#
#   deploy/kubernetes/validate.sh
#
# Needs kubectl (or kustomize) and kubeconform on PATH. No cluster is contacted.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
k8s_version="${KUBERNETES_VERSION:-1.33.0}"

if command -v kustomize >/dev/null; then
  build=(kustomize build)
elif command -v kubectl >/dev/null; then
  build=(kubectl kustomize)
else
  echo "neither kustomize nor kubectl is on PATH" >&2; exit 2
fi
command -v kubeconform >/dev/null || { echo "kubeconform is not on PATH" >&2; exit 2; }

work="$(mktemp -d)"
# Schemas are downloaded once per run (and once per machine with KUBECONFORM_CACHE).
cache="${KUBECONFORM_CACHE:-$work/schemas}"
mkdir -p "$cache"
trap 'rm -rf "$work"' EXIT

for dir in base overlays/bundled-postgres overlays/sqlite overlays/external-postgres examples/all-components; do
  echo "== $dir"
  name="${dir//\//-}"
  "${build[@]}" "$here/$dir" >"$work/$name.yaml"
  kubeconform -strict -summary -cache "$cache" -kubernetes-version "$k8s_version" "$work/$name.yaml"
done

# The single-writer rule must survive every overlay: one replica, one RWO claim.
for dir in overlays/bundled-postgres overlays/sqlite overlays/external-postgres examples/all-components; do
  f="$work/${dir//\//-}.yaml"
  grep -q '^  replicas: 1$' "$f" || { echo "FAIL: $dir does not pin replicas: 1" >&2; exit 1; }
  grep -q 'ReadWriteMany' "$f" && { echo "FAIL: $dir asks for ReadWriteMany" >&2; exit 1; }
done
echo "ok: replicas: 1 and no ReadWriteMany in every overlay"
