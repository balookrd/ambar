#!/usr/bin/env bash
# Deploys ambar CSI driver to the physical 3-node NanoPi R5C cluster.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HERE/.."
DEPLOY="$ROOT/deploy"
source "$HERE/r5c-lib.sh"

echo "=== 1. Ensuring host directories on physical nodes ==="
for n in "${R5C_NODES[@]}"; do
    echo "Creating /var/lib/rancher/ambar/{volumes,res} on $n..."
    r5c_ssh "$n" "install -d -m 0700 /var/lib/rancher/ambar/volumes && install -d -m 0755 /var/lib/rancher/ambar/res"
done

echo "=== 2. Building and distributing ambar image ==="
(cd "$ROOT/image" && ./build.sh)

for n in "${R5C_NODES[@]}"; do
    echo "Importing $R5C_IMAGE into containerd on $n..."
    docker save "$R5C_IMAGE" | r5c_ssh "$n" "k3s ctr images import -"
done

echo "=== 3. Applying Kubernetes core manifests ==="
kubectl apply -f "$DEPLOY/namespace.yaml"
kubectl apply -f "$DEPLOY/crd.json"
kubectl wait --for=condition=Established crd/replicatedvolumes.ambar.balookrd.github.io --timeout=60s
kubectl apply -f "$DEPLOY/csidriver.yaml"
kubectl apply -f "$DEPLOY/rbac.yaml"

echo "=== 4. Applying ambar-nodes ConfigMap ==="
cat << 'CM' | kubectl apply -f -
apiVersion: v1
kind: ConfigMap
metadata:
  name: ambar-nodes
  namespace: ambar-system
data:
  nodes: |
    [
      {"name": "k3s-1", "address": "10.10.10.51", "nodeId": 0, "capacityBytes": 107374182400},
      {"name": "k3s-2", "address": "10.10.10.52", "nodeId": 1, "capacityBytes": 107374182400},
      {"name": "k3s-3", "address": "10.10.10.53", "nodeId": 2, "capacityBytes": 107374182400}
    ]
CM

echo "=== 5. Applying DaemonSet and StorageClasses ==="
image_id="$(docker image inspect --format '{{.Id}}' "$R5C_IMAGE")"
sed -e "s|^\( *imagePullPolicy:\) AMBAR_IMAGE_PULL_POLICY\$|\1 Never|" \
    -e "s|^\( *image:\) AMBAR_IMAGE\$|\1 $R5C_IMAGE|" \
    "$DEPLOY/daemonset.yaml" \
    | awk -v id="$image_id" '
        { print }
        /^    metadata:$/ { print "      annotations:"; print "        ambar.balookrd.github.io/image-id: \"" id "\"" }' \
    | kubectl apply -f -

kubectl apply -f "$DEPLOY/storageclass.yaml"

echo "Waiting for ambar DaemonSet rollout (up to 300s)..."
kubectl -n "$R5C_SYSTEM_NS" rollout status ds/ambar --timeout=300s

echo "=== 6. Verifying CSINode driver registration ==="
for n in "${R5C_NODES[@]}"; do
    registered=0
    for _ in $(seq 30); do
        drivers="$(kubectl get csinode "$n" -o jsonpath='{.spec.drivers[*].name}' 2>/dev/null || true)"
        if [[ " $drivers " == *" ambar.balookrd.github.io "* ]]; then
            registered=1
            break
        fi
        sleep 2
    done
    if (( registered == 0 )); then
        echo "FAIL: Node $n has no ambar CSI driver registered (drivers: '$drivers')" >&2
        exit 1
    fi
    echo "Node $n registered ambar.balookrd.github.io OK"
done

echo "OK: ambar deployed successfully on all 3 R5C nodes."
