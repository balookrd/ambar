#!/usr/bin/env bash
# Installs the ambar CSI driver on the stand, or removes it (--remove).
#
#   stand/install.sh            install, or bring an installed stand up to date
#   stand/install.sh --remove   remove the driver, bring the toolbox back
#
# A repeated run changes nothing that is already current: `kubectl apply` of
# unchanged manifests, the node map keeps every existing nodeId (step 5), and
# the agent pods are replaced only when the DaemonSet template changes, which
# includes a rebuilt image (step 6).
#
# Variables:
#   IMAGE         agent image, default ambar-drbd:dev (built by image/build.sh)
#   PULL_POLICY   imagePullPolicy of the agent, default Never: the image is
#                 imported into containerd on every node below
#   VOLUME_GROUP  LVM volume group for backing LVs, default empty (sparse mode)
set -euo pipefail
source "$(dirname "$0")/lib.sh"
IMAGE="${IMAGE:-ambar-drbd:dev}"
PULL_POLICY="${PULL_POLICY:-Never}"
VOLUME_GROUP="${VOLUME_GROUP:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
DEPLOY="$HERE/../deploy"
DRIVER=ambar.balookrd.github.io
PLUGIN_DIR=/var/lib/kubelet/plugins/ambar
CRD=replicatedvolumes.ambar.balookrd.github.io

remove() {
    # Deleting the CRD deletes its objects, and the agents would then see
    # volumes without objects: check before anything is deleted.
    local crd_name
    crd_name="$(kc get crd "$CRD" --ignore-not-found -o name)"
    if [[ -n "$crd_name" ]]; then
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
# The output is read into a variable first: an empty output of a failed kubectl
# must not count as "no pods".
toolbox_pods="x"
for _ in $(seq 60); do
    toolbox_pods="$(kc -n ambar-spike get pods -l app=toolbox -o name)"
    [[ -z "$toolbox_pods" ]] && break
    sleep 2
done
[[ -z "$toolbox_pods" ]] \
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

# 5. ConfigMap ambar-nodes. A nodeId never changes once it is in the map: the
# existing entries are read back and kept as they are; a node that is new in
# NODES gets the smallest free id. A node of the map that is missing from NODES,
# or one whose address changed, stops the script (nothing is rewritten).
existing="$(kc -n ambar-system get cm ambar-nodes --ignore-not-found -o jsonpath='{.data.nodes}')"
wanted=()
for n in "${NODES[@]}"; do
    wanted+=("$(vm_host "$n")=$(vm_ip "$n")")
done
entries="$(python3 -I -c '
import json, sys

existing = json.loads(sys.argv[1]) if sys.argv[1].strip() else []
wanted = [w.split("=", 1) for w in sys.argv[2:]]
want = dict(wanted)
for e in existing:
    if e["name"] not in want:
        sys.exit("FAIL: node %s is in the ConfigMap ambar-nodes (nodeId %d) but not in NODES; "
                 "nothing was changed" % (e["name"], e["nodeId"]))
    if e["address"] != want[e["name"]]:
        sys.exit("FAIL: node %s has address %s in the ConfigMap ambar-nodes but %s now; "
                 "nothing was changed" % (e["name"], e["address"], want[e["name"]]))
have = set(e["name"] for e in existing)
used = set(e["nodeId"] for e in existing)
out = list(existing)
for name, address in wanted:
    if name in have:
        continue
    free = 0
    while free in used:
        free += 1
    used.add(free)
    out.append({"name": name, "address": address, "nodeId": free, "capacityBytes": 8589934592})
for e in out:
    print(json.dumps(e))
' "$existing" "${wanted[@]}")"
{
    printf 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: ambar-nodes\n  namespace: ambar-system\ndata:\n  nodes: |\n    ['
    sep=""
    while IFS= read -r e; do
        printf '%s\n      %s' "$sep" "$e"
        sep=","
    done <<<"$entries"
    printf '\n    ]\n'
} | kc apply -f -

# 6. DaemonSet and StorageClasses. Only the image lines are substituted, the
# placeholder names in the comments stay. The image id goes into the pod
# template as an annotation: a rebuilt image under the same tag changes the
# template and the DaemonSet replaces the pods; with nothing changed `apply`
# reports `unchanged` and no pod is touched.
image_id="$(docker image inspect --format '{{.Id}}' "$IMAGE")"
sed -e "s|^\( *imagePullPolicy:\) AMBAR_IMAGE_PULL_POLICY\$|\1 $PULL_POLICY|" \
    -e "s|^\( *image:\) AMBAR_IMAGE\$|\1 $IMAGE|" \
    "$DEPLOY/daemonset.yaml" \
    | awk -v id="$image_id" -v vg="$VOLUME_GROUP" '
        vg == "" && /^[[:space:]]*- --volume-group=k3s-vg$/ { next }
        vg == "" && /^[[:space:]]*- name: VOLUME_GROUP$/ { skip_vg_val = 1; next }
        skip_vg_val && /^[[:space:]]*value: "k3s-vg"$/ { skip_vg_val = 0; next }
        skip_vg_val { skip_vg_val = 0 }
        vg != "" {
            sub(/--volume-group=k3s-vg/, "--volume-group=" vg)
            sub(/value: "k3s-vg"/, "value: \"" vg "\"")
        }
        { print }
        /^    metadata:$/ { print "      annotations:"; print "        ambar.balookrd.github.io/image-id: \"" id "\"" }' \
    | kc apply -f -
kc apply -f - < "$DEPLOY/storageclass.yaml"
kc -n ambar-system rollout status ds/ambar --timeout=300s

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

# 8. The agent container must not add a second mount of the socket directory
# below the Bidirectional mount of /var/lib/kubelet: each pod start would then
# double the number of mounts of $PLUGIN_DIR on the node. A node may have one
# (the directory bind of an earlier start); more is reported, not repaired.
for n in "${NODES[@]}"; do
    mounts="$(vm "$n" "awk '\$5 == \"$PLUGIN_DIR\" {c++} END {print c + 0}' /proc/self/mountinfo")"
    if [[ "$mounts" =~ ^[0-9]+$ ]] && (( mounts > 1 )); then
        echo "WARN: $n has $mounts mounts at $PLUGIN_DIR; clean up with: limactl shell $n sudo umount -R -l $PLUGIN_DIR" >&2
    fi
done
echo "OK: ambar installed on ${#NODES[@]} nodes"
