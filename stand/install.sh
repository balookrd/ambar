#!/usr/bin/env bash
# Installs the ambar CSI driver on the stand, or removes it (--remove).
#
#   stand/install.sh            install (safe to repeat)
#   stand/install.sh --remove   remove the driver, bring the toolbox back
#
# Variables:
#   IMAGE        agent image, default ambar-drbd:dev (built by image/build.sh)
#   PULL_POLICY  imagePullPolicy of the agent, default Never: the image is
#                imported into containerd on every node below
set -euo pipefail
source "$(dirname "$0")/lib.sh"
IMAGE="${IMAGE:-ambar-drbd:dev}"
PULL_POLICY="${PULL_POLICY:-Never}"
HERE="$(cd "$(dirname "$0")" && pwd)"
DEPLOY="$HERE/../deploy"
DRIVER=ambar.balookrd.github.io
CRD=replicatedvolumes.ambar.balookrd.github.io

remove() {
    # Deleting the CRD deletes its objects, and the agents would then see
    # volumes without objects: check before anything is deleted.
    if [[ -n "$(kc get crd "$CRD" --ignore-not-found -o name)" ]]; then
        local objects
        objects="$(kc get replicatedvolumes.ambar.balookrd.github.io -o name)"
        if [[ -n "$objects" ]]; then
            echo "FAIL: ReplicatedVolume objects exist, nothing was removed:" >&2
            echo "$objects" >&2
            echo "delete the PVCs that use them first" >&2
            exit 1
        fi
    fi
    kc delete -f - --ignore-not-found < "$DEPLOY/storageclass.yaml"
    kc -n ambar-system delete ds/ambar --ignore-not-found --wait=true
    kc delete -f - --ignore-not-found < "$DEPLOY/rbac.yaml"
    kc delete csidriver "$DRIVER" --ignore-not-found
    kc delete crd "$CRD" --ignore-not-found
    kc delete namespace ambar-system --ignore-not-found --wait=true
    kc apply -f - < "$HERE/toolbox.yaml"
    kc -n ambar-spike rollout status ds/toolbox --timeout=120s
    echo "OK: ambar removed, toolbox restored"
}

if [[ "${1:-}" == "--remove" ]]; then
    remove
    exit 0
elif [[ $# -gt 0 ]]; then
    echo "usage: $0 [--remove]" >&2
    exit 2
fi

# 1. Directories the agent needs on every node. It never creates the volumes
# directory, and a hostPath mount of type Directory fails when one is missing.
for n in "${NODES[@]}"; do
    vm "$n" "install -d -m 0700 /var/lib/rancher/ambar/volumes && install -d -m 0755 /var/lib/rancher/ambar/res"
done

# 2. The toolbox and the agent must not both manage the DRBD resources.
kc -n ambar-spike delete ds/toolbox --ignore-not-found --wait=true
for _ in $(seq 60); do
    [[ -z "$(kc -n ambar-spike get pods -l app=toolbox -o name)" ]] && break
    sleep 2
done
[[ -z "$(kc -n ambar-spike get pods -l app=toolbox -o name)" ]] \
    || { echo "FAIL: toolbox pods are still there" >&2; exit 1; }

# 3. The agent image, as stand/k3s.sh does. The sidecar images are pulled by
# the nodes themselves.
for n in "${NODES[@]}"; do
    docker save "$IMAGE" | limactl shell --workdir / "$n" sudo k3s ctr images import -
done

# 4. Namespace, CRD, CSIDriver, RBAC.
kc apply -f - < "$DEPLOY/namespace.yaml"
kc apply -f - < "$DEPLOY/crd.json"
kc wait --for=condition=Established "crd/$CRD" --timeout=60s
kc apply -f - < "$DEPLOY/csidriver.yaml"
kc apply -f - < "$DEPLOY/rbac.yaml"

# 5. ConfigMap ambar-nodes: nodeId follows the order of NODES.
entries=()
id=0
for n in "${NODES[@]}"; do
    entries+=("$(printf '{"name": "%s", "address": "%s", "nodeId": %d, "capacityBytes": 8589934592}' \
        "$(vm_host "$n")" "$(vm_ip "$n")" "$id")")
    id=$((id + 1))
done
{
    printf 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: ambar-nodes\n  namespace: ambar-system\ndata:\n  nodes: |\n    ['
    sep=""
    for e in "${entries[@]}"; do
        printf '%s\n      %s' "$sep" "$e"
        sep=","
    done
    printf '\n    ]\n'
} | kc apply -f -

# 6. DaemonSet and StorageClasses. Only the image lines are substituted, the
# placeholder names in the comments stay.
existed="$(kc -n ambar-system get ds/ambar --ignore-not-found -o name)"
sed -e "s|^\( *imagePullPolicy:\) AMBAR_IMAGE_PULL_POLICY\$|\1 $PULL_POLICY|" \
    -e "s|^\( *image:\) AMBAR_IMAGE\$|\1 $IMAGE|" \
    "$DEPLOY/daemonset.yaml" | kc apply -f -
kc apply -f - < "$DEPLOY/storageclass.yaml"
# A rebuilt image under the same tag is not picked up by a running pod.
[[ -z "$existed" ]] || kc -n ambar-system rollout restart ds/ambar
kc -n ambar-system rollout status ds/ambar --timeout=180s

# 7. Every node registered the driver with kubelet.
for n in "${NODES[@]}"; do
    host="$(vm_host "$n")"
    drivers=""
    for _ in $(seq 30); do
        drivers="$(kc get csinode "$host" -o jsonpath='{.spec.drivers[*].name}' 2>/dev/null || true)"
        [[ " $drivers " == *" $DRIVER "* ]] && break
        sleep 2
    done
    [[ " $drivers " == *" $DRIVER "* ]] \
        || { echo "FAIL: $host has no CSI driver $DRIVER (drivers: '$drivers')" >&2; exit 1; }
done
echo "OK: ambar installed on ${#NODES[@]} nodes"
