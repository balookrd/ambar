#!/usr/bin/env bash
# End-to-end check of the CSI driver on the stand: PVCs of the classes ambar-r3
# and ambar-r2, pods on chosen nodes, the diskless Client membership, a restart
# of the agents under a writer, a rejected StorageClass, deletion, and the
# refusal to delete the data of a volume whose object is gone (R10).
#
# Needs the driver installed on four nodes:
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/csi-e2e.sh
# With three nodes the script stops at once: steps e and f need a node that
# holds no part of the volume (the Client membership).
#
# The last line of a successful run is `OK: csi e2e`. On a failure the script
# prints `kc get rv -o yaml`, the events of the namespace default and the tail of
# every agent log, and exits with 1. A run starts by removing the objects of an
# earlier one: pods labelled app=ambar-e2e, PVCs e2e-r3 e2e-r2 e2e-r1 e2e-k, the
# StorageClass e2e-r1, and the orphan image of an interrupted step k (its volume
# name is kept in the ConfigMap ambar-e2e-state of the namespace default, see
# step k; nothing else is removed).
#
# Variables:
#   E2E_IMAGE   image of the test pods, pinned by digest (busybox 1.37.0, index)
set -eEuo pipefail
source "$(dirname "$0")/lib.sh"

E2E_IMAGE="${E2E_IMAGE:-docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec}"
VOL_DIR=/var/lib/rancher/ambar/volumes
RES_DIR=/var/lib/rancher/ambar/res
NS=ambar-system

if (( ${#NODES[@]} < 4 )); then
    echo "FAIL: steps e and f need a node outside the volume: set NODES to four nodes, e.g. NODES=\"dv-1 dv-2 dv-3 dv-4\"" >&2
    exit 2
fi
N1="${NODES[0]}"; N2="${NODES[1]}"
H1="$(vm_host "$N1")"; H2="$(vm_host "$N2")"

# The diagnostics print once even when `fail` runs inside $( ... ): a flag in
# the filesystem is seen by the parent shell, a variable is not.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/ambar-e2e.XXXXXX")"
SKIP_LABEL=ambar-e2e-skip
DS_PATCHED=no
LABELED=""

# --- diagnostics --------------------------------------------------------------

restore_agent_placement() {
    if [[ -n "$LABELED" ]]; then
        kc label node "$LABELED" "$SKIP_LABEL-" >/dev/null 2>&1 || true
        LABELED=""
    fi
    if [[ "$DS_PATCHED" == yes ]]; then
        kc -n "$NS" patch ds/ambar --type=json \
            -p '[{"op":"remove","path":"/spec/template/spec/affinity"}]' >/dev/null 2>&1 || true
        DS_PATCHED=no
    fi
}

diag() {
    [[ ! -e "$WORK/diag-done" ]] || return 0
    : > "$WORK/diag-done"
    {
        echo "---- diagnostics: kc get rv -o yaml"
        kc get rv -o yaml || true
        echo "---- diagnostics: kc get pods,pvc -o wide"
        kc get pods,pvc -o wide || true
        echo "---- diagnostics: events of namespace default"
        kc -n default get events --sort-by=.lastTimestamp || true
        local pod
        for pod in $(kc -n "$NS" get pods -l app=ambar -o name 2>/dev/null || true); do
            echo "---- diagnostics: tail of the agent log $pod ($(kc -n "$NS" get "$pod" -o jsonpath='{.spec.nodeName}' 2>/dev/null))"
            kc -n "$NS" logs "$pod" -c ambar --tail=60 || true
        done
    } >&2
    restore_agent_placement
}

fail() {
    echo "FAIL: [step $STEP] $1" >&2
    diag
    exit 1
}

on_err() {
    # A `fail` in a command substitution has already printed everything.
    [[ ! -e "$WORK/diag-done" ]] || exit 1
    echo "FAIL: [step $STEP] command failed at line $1: $BASH_COMMAND" >&2
    diag
    exit 1
}
trap 'on_err $LINENO' ERR
# Whatever ends the script (success, failure, Ctrl-C) puts the agent placement
# of step k back and removes the work directory.
cleanup_on_exit() {
    restore_agent_placement
    rm -rf "$WORK"
}
trap cleanup_on_exit EXIT
trap 'exit 130' INT TERM
STEP=init

step() { STEP="$1"; echo; echo "== $2"; }

# --- helpers ------------------------------------------------------------------

# wait_until <seconds> <what> <command>: poll the command (eval) every 2 s.
# Sets WAITED to the seconds it took.
wait_until() {
    local limit="$1" what="$2" cmd="$3" start=$SECONDS
    until ( trap - ERR; eval "$cmd" ); do
        (( SECONDS - start < limit )) || fail "$what: not within $limit s"
        sleep 2
    done
    WAITED=$(( SECONDS - start ))
}

# agent_pod <node>: the live agent pod of a node.
# A failed kubectl is an error, not "no pod".
agent_pod() {
    local out
    out="$(kc -n "$NS" get pod -l app=ambar \
        --field-selector "spec.nodeName=$(vm_host "$1"),status.phase=Running" \
        -o go-template='{{range .items}}{{if not .metadata.deletionTimestamp}}{{.metadata.name}}{{"\n"}}{{end}}{{end}}')" \
        || return 1
    printf '%s\n' "${out%%$'\n'*}"
}
# agent_gone <node>: true when the node has no live agent pod (false when kubectl fails).
agent_gone() {
    local p
    p="$(agent_pod "$1")" || return 1
    [[ -z "$p" ]]
}

# ag <node> <command...>: run a command in the agent container of a node.
ag() {
    local node="$1"; shift
    local pod
    pod="$(agent_pod "$node")"
    [[ -n "$pod" ]] || { echo "ag: no agent pod on $node" >&2; return 1; }
    kc -n "$NS" exec "$pod" -c ambar -- bash -c "$*"
}

rvjp() { kc get rv "$1" -o jsonpath="$2"; }
# members <rv>: lines `<node>=<kind>` of spec.nodes.
members() { rvjp "$1" '{range .spec.nodes[*]}{.name}={.kind}{"\n"}{end}'; }
# disks <rv>: lines `<node>=<disk>` of status.nodes.
disks() { rvjp "$1" '{range .status.nodes[*]}{.name}={.disk}{"\n"}{end}'; }
# The helpers below read the output into a variable first, so that a failed
# kubectl is an error and never "no members" / "no objects".
count_kind() { local m; m="$(members "$1")" || return 1; grep -c "=$2\$" <<<"$m" || true; }
has_member() { local m; m="$(members "$1")" || return 1; grep -q "^$2=$3\$" <<<"$m"; }
# no_member <rv> <node> <kind>: true only when kubectl worked and the entry is absent.
no_member() { local m; m="$(members "$1")" || return 1; ! grep -q "^$2=$3\$" <<<"$m"; }
# no_rvs: true only when `kubectl get rv` worked and printed nothing.
no_rvs() { local o; o="$(kc get rv -o name)" || return 1; [[ -z "$o" ]]; }
# files_of <node> <name>...: entries of the volumes and res directories that
# contain one of the names (a failing ls is an error); loops_of the same for loop devices.
files_of() {
    local node="$1"; shift
    local listing pat=() name
    listing="$(vm "$node" "ls -A $VOL_DIR $RES_DIR")" || return 1
    for name in "$@"; do pat+=(-e "$name"); done
    grep -F "${pat[@]}" <<<"$listing" || true
}
loops_of() {
    local node="$1"; shift
    local listing pat=() name
    listing="$(vm "$node" "losetup -a")" || return 1
    for name in "$@"; do pat+=(-e "$name"); done
    grep -F "${pat[@]}" <<<"$listing" || true
}
rv_of_pvc() {
    local pv
    pv="$(kc get pvc "$1" -o jsonpath='{.spec.volumeName}')"
    [[ -n "$pv" ]] || return 1
    kc get pv "$pv" -o jsonpath='{.spec.csi.volumeHandle}'
}

# pod_apply <name> <pvc> <node-name> <script>: a busybox pod that runs the
# script with the PVC mounted at /data. The script is wrapped to exit on TERM.
pod_apply() {
    local name="$1" pvc="$2" host="$3" body="$4"
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: Pod
metadata:
  name: $name
  labels: {app: ambar-e2e}
spec:
  nodeSelector:
    kubernetes.io/hostname: $host
  terminationGracePeriodSeconds: 2
  containers:
    - name: c
      image: $E2E_IMAGE
      command: ["sh", "-c"]
      args:
        - |
$(printf "trap 'exit 0' TERM\n%s\nwhile :; do sleep 1; done\n" "$body" | sed 's/^/          /')
      volumeMounts:
        - {name: d, mountPath: /data}
  volumes:
    - name: d
      persistentVolumeClaim: {claimName: $pvc}
EOF
}

pvc_apply() {
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: $1
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: $2
  resources:
    requests:
      storage: 1Gi
EOF
}

pod_ready() { kc wait --for=condition=Ready "pod/$1" --timeout="${2:-120}s" >/dev/null 2>&1 \
    || fail "pod $1 is not Ready within ${2:-120} s (phase: $(kc get pod "$1" -o jsonpath='{.status.phase}' 2>/dev/null))"; }
pod_gone() { kc delete pod "$1" --ignore-not-found --wait=true --timeout=120s >/dev/null; }
# pod_cat <pod> <file>: file contents, polled until non-empty (the pod writes it after start).
pod_cat() {
    local out="" start=$SECONDS
    until out="$(kc exec "$1" -- cat "$2" 2>/dev/null)" && [[ -n "$out" ]]; do
        (( SECONDS - start < 60 )) || fail "$1: $2 is empty after 60 s"
        sleep 2
    done
    printf '%s\n' "$out"
}
# drbd_status <node> <rv>: `drbdsetup status` of the resource, errors included.
drbd_status() { ag "$1" "drbdsetup status $2 2>&1 || true"; }

# --- leftovers ----------------------------------------------------------------

step cleanup "leftovers of an earlier run"
kc label node --all "$SKIP_LABEL-" >/dev/null 2>&1 || true
kc -n "$NS" patch ds/ambar --type=json -p '[{"op":"remove","path":"/spec/template/spec/affinity"}]' >/dev/null 2>&1 || true
kc delete pod -l app=ambar-e2e --ignore-not-found --wait=true --timeout=120s >/dev/null
for pvc in e2e-r3 e2e-r2 e2e-r1 e2e-k; do
    kc delete pvc "$pvc" --ignore-not-found --wait=true --timeout=120s >/dev/null
done
kc delete sc e2e-r1 --ignore-not-found >/dev/null
wait_until 300 "ReplicatedVolume objects of an earlier run are still there" no_rvs
kc -n "$NS" rollout status ds/ambar --timeout=240s >/dev/null
# An interrupted step k leaves the backing file of its volume on one node (the
# agent refuses to delete it, R10). Step k records the volume name in the
# ConfigMap ambar-e2e-state before it removes the object; only that name is
# removed here, with `ambar remove --name`, and only if it has the form of a
# volume name generated for a PVC.
orphan="$(kc -n default get cm ambar-e2e-state --ignore-not-found -o jsonpath='{.data.orphan}')"
if [[ -n "$orphan" ]]; then
    [[ "$orphan" =~ ^pvc-[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ ]] \
        || fail "the ConfigMap ambar-e2e-state holds '$orphan', not a volume name; delete the ConfigMap by hand"
    for n in "${NODES[@]}"; do
        if [[ -n "$(files_of "$n" "$orphan")" ]]; then
            echo "orphan of an interrupted step k on $n: $(ag "$n" "ambar remove --name $orphan" 2>&1)"
        fi
    done
    kc -n default delete cm ambar-e2e-state >/dev/null
fi
echo "images are pulled on every node first, so the timings below do not include the pull"
for n in "${NODES[@]}"; do
    vm "$n" "k3s crictl pull $E2E_IMAGE >/dev/null"
done
echo "image: $E2E_IMAGE"

# --- a ------------------------------------------------------------------------

step a "a. PVC e2e-r3 and pod w3 on $N1"
pvc_apply e2e-r3 ambar-r3
pod_apply w3 e2e-r3 "$H1" 'echo e2e-r3-data > /data/f; sync'
wait_until 120 "PVC e2e-r3 is not Bound" '[[ "$(kc get pvc e2e-r3 -o jsonpath="{.status.phase}")" == Bound ]]'
echo "time: e2e-r3 Bound after $WAITED s (polled every 2 s, from the apply)"
pod_ready w3 120
echo "time: pod w3 Ready, $SECONDS s into the run"
RV3="$(rv_of_pvc e2e-r3)"
echo "volume: $RV3"
[[ "$(pod_cat w3 /data/f)" == e2e-r3-data ]] || fail "w3 does not read back /data/f"
[[ "$(count_kind "$RV3" Replica)" == 3 ]] || fail "$RV3 has $(count_kind "$RV3" Replica) Replica entries, expected 3"
[[ "$(rvjp "$RV3" '{.spec.initialSync}')" == Done ]] || fail "$RV3: initialSync is not Done"
wait_until 60 "three status.nodes entries with disk UpToDate" \
    '[[ "$(disks "$RV3" | grep -c "=UpToDate\$" || true)" == 3 ]]'
echo "time: three UpToDate status entries $WAITED s after Ready"
members "$RV3"
disks "$RV3"
MINOR3="$(rvjp "$RV3" '{.spec.minor}')"

# --- b ------------------------------------------------------------------------

step b "b. device size on $N1"
size="$(vm "$N1" "blockdev --getsize64 /dev/drbd$MINOR3")"
[[ "$size" =~ ^[0-9]+$ ]] && (( size >= 1073741824 )) \
    || fail "/dev/drbd$MINOR3 on $N1 has '$size' bytes, expected at least 1073741824"
echo "/dev/drbd$MINOR3 on $N1: $size bytes"

# --- c ------------------------------------------------------------------------

step c "c. the pod moves to $N2 (writes a counter with fsync from here on)"
pod_gone w3
# The counter pod: reads /data/f back, then writes an increasing counter with fsync.
pod_apply r3 e2e-r3 "$H2" 'i=0; while :; do i=$((i+1)); echo $i | dd of=/data/counter conv=fsync 2>/dev/null; sleep 1; done'
pod_ready r3 120
echo "time: pod r3 Ready on $N2, $SECONDS s into the run"
[[ "$(pod_cat r3 /data/f)" == e2e-r3-data ]] || fail "r3 on $N2 does not read the data written on $N1"
wait_until 60 "$N1 is not Secondary" '[[ "$(drbd_status "$N1" "$RV3" | head -n1)" == *"role:Secondary"* ]]'
echo "time: $N1 Secondary $WAITED s after the check started"
echo "-- drbdsetup status $RV3 on $N1:"
drbd_status "$N1" "$RV3"
echo "-- members of $RV3 (pod on $N2):"
members "$RV3"

# --- c2: a second pod on another node while the volume is mounted ------------------

step c2 "c2. observation: a second pod with the same PVC while $N2 has it mounted"
OTHER=""
for n in "${NODES[@]}"; do
    [[ "$n" == "$N2" ]] && continue
    if ! members "$RV3" | grep -q "^$(vm_host "$n")="; then OTHER="$n"; break; fi
done
OTHER_MEMBER=no
if [[ -z "$OTHER" ]]; then
    OTHER_MEMBER=yes
    for n in "${NODES[@]}"; do [[ "$n" != "$N2" ]] && { OTHER="$n"; break; }; done
fi
echo "second pod dup3 on $OTHER (member of $RV3 before: $OTHER_MEMBER)"
before="$(members "$RV3" | tr '\n' ' ')"
pod_apply dup3 e2e-r3 "$(vm_host "$OTHER")" 'cat /data/f'
DUP_UID="$(kc get pod dup3 -o jsonpath='{.metadata.uid}')"
wait_until 150 "no FailedMount event for dup3" \
    'kc get events --field-selector involvedObject.uid=$DUP_UID,reason=FailedMount -o name 2>/dev/null | grep -q .'
echo "time: first FailedMount event for dup3 after $WAITED s"
echo "-- events of dup3:"
kc get events --field-selector involvedObject.uid=$DUP_UID -o custom-columns=TYPE:.type,REASON:.reason,MESSAGE:.message --no-headers
echo "-- phase of dup3: $(kc get pod dup3 -o jsonpath='{.status.phase}')"
echo "-- drbdsetup status on $N2:"; drbd_status "$N2" "$RV3"
echo "-- drbdsetup status on $OTHER:"; drbd_status "$OTHER" "$RV3"
echo "-- members before: $before"
echo "-- members with dup3 pending: $(members "$RV3" | tr '\n' ' ')"
pod_gone dup3
if [[ "$OTHER_MEMBER" == no ]]; then
    wait_until 120 "the Client entry of $OTHER is still in spec.nodes after dup3 was deleted" \
        'no_member "$RV3" "$(vm_host "$OTHER")" Client'
    echo "time: Client entry of $OTHER removed $WAITED s after dup3 was deleted"
fi
echo "-- members after dup3 was deleted: $(members "$RV3" | tr '\n' ' ')"
sleep 5
echo "-- drbdsetup status on $N2, 5 s later:"; drbd_status "$N2" "$RV3"
[[ "$(kc get pod r3 -o jsonpath='{.status.phase}')" == Running ]] || fail "r3 is not Running after dup3"

# --- d ------------------------------------------------------------------------

step d "d. PVC e2e-r2 and pod w2 on $N1"
pvc_apply e2e-r2 ambar-r2
pod_apply w2 e2e-r2 "$H1" 'echo e2e-r2-data > /data/f; sync'
wait_until 120 "PVC e2e-r2 is not Bound" '[[ "$(kc get pvc e2e-r2 -o jsonpath="{.status.phase}")" == Bound ]]'
echo "time: e2e-r2 Bound after $WAITED s"
pod_ready w2 120
RV2="$(rv_of_pvc e2e-r2)"
echo "volume: $RV2"
[[ "$(pod_cat w2 /data/f)" == e2e-r2-data ]] || fail "w2 does not read back /data/f"
members "$RV2"
[[ "$(count_kind "$RV2" Replica)" == 2 ]] || fail "$RV2: $(count_kind "$RV2" Replica) Replica entries, expected 2"
[[ "$(count_kind "$RV2" Witness)" == 1 ]] || fail "$RV2: $(count_kind "$RV2" Witness) Witness entries, expected 1"
[[ "$(members "$RV2" | wc -l | tr -d ' ')" == 3 ]] || fail "$RV2: spec.nodes does not have exactly three entries"
has_member "$RV2" "$H1" Replica || fail "$RV2: $H1 is not a Replica"
ABSENT=""; WITNESS=""
for n in "${NODES[@]}"; do
    h="$(vm_host "$n")"
    if ! members "$RV2" | grep -q "^$h="; then ABSENT="$n"; fi
    if has_member "$RV2" "$h" Witness; then WITNESS="$n"; fi
done
[[ -n "$ABSENT" && -n "$WITNESS" ]] || fail "no node outside $RV2 or no Witness found"
echo "outside the volume: $ABSENT, Witness: $WITNESS"
wait_until 60 "two Replica status entries UpToDate" '[[ "$(disks "$RV2" | grep -c "=UpToDate\$" || true)" == 2 ]]'
echo "-- observation 5: kc get rv $RV2 -o json --show-managed-fields"
kc get rv "$RV2" -o json --show-managed-fields

# --- e ------------------------------------------------------------------------

step e "e. pod on $ABSENT, a node outside $RV2 (Client)"
pod_gone w2
HA="$(vm_host "$ABSENT")"
pod_apply c2 e2e-r2 "$HA" 'cat /data/f'
pod_ready c2 120
echo "time: pod c2 Ready on $ABSENT, $SECONDS s into the run"
has_member "$RV2" "$HA" Client || fail "$RV2: no Client entry for $HA in spec.nodes after the pod is Running"
members "$RV2"
[[ "$(pod_cat c2 /data/f)" == e2e-r2-data ]] || fail "c2 on $ABSENT does not read the data of $RV2"
vm "$ABSENT" "test ! -e $VOL_DIR/$RV2.img" || fail "$ABSENT has $VOL_DIR/$RV2.img"
MINOR2="$(rvjp "$RV2" '{.spec.minor}')"
echo "-- observation 1 on $ABSENT (Client, volume mounted by c2):"
ag "$ABSENT" "blockdev --getsize64 /dev/drbd$MINOR2; echo rc=\$?; blkid -p -o value -s TYPE /dev/drbd$MINOR2; echo rc=\$?" 2>&1 || true
echo "-- drbdsetup status $RV2 on $ABSENT:"; drbd_status "$ABSENT" "$RV2"

# --- f ------------------------------------------------------------------------

step f "f. the pod leaves $ABSENT, the Client entry goes away"
T_DEL=$SECONDS
pod_gone c2
echo "time: pod c2 deleted in $(( SECONDS - T_DEL )) s (kubectl delete --wait)"
wait_until 60 "the Client entry of $HA is still in spec.nodes" 'no_member "$RV2" "$HA" Client'
echo "time: Client entry removed $WAITED s after the pod was deleted"
members "$RV2"
wait_until 60 "$ABSENT still has the DRBD resource $RV2" \
    '[[ "$(drbd_status "$ABSENT" "$RV2")" == *"No such resource"* || "$(drbd_status "$ABSENT" "$RV2")" == *"No currently configured DRBD"* ]]'
echo "time: no DRBD resource on $ABSENT $WAITED s after the entry was removed"
echo "-- drbdsetup status $RV2 on $ABSENT:"; drbd_status "$ABSENT" "$RV2"
for n in "${NODES[@]}"; do
    [[ "$n" == "$ABSENT" ]] && continue
    h="$(vm_host "$n")"
    members "$RV2" | grep -q "^$h=" || continue
    wait_until 60 "$n still shows a connection to $ABSENT" \
        'st="$(drbd_status "$n" "$RV2")" && ! grep -q "^ *$HA " <<<"$st"'
    echo "-- drbdsetup status $RV2 on $n (took $WAITED s):"; drbd_status "$n" "$RV2"
done

# --- g ------------------------------------------------------------------------

step g "g. pod on the Witness $WITNESS"
HW="$(vm_host "$WITNESS")"
members_before="$(members "$RV2")"
pod_apply g2 e2e-r2 "$HW" 'cat /data/f'
pod_ready g2 120
[[ "$(members "$RV2")" == "$members_before" ]] || fail "spec.nodes of $RV2 changed while a pod runs on the Witness"
[[ "$(pod_cat g2 /data/f)" == e2e-r2-data ]] || fail "g2 on $WITNESS does not read the data of $RV2"
pod_gone g2
sleep 10
[[ "$(members "$RV2")" == "$members_before" ]] || fail "spec.nodes of $RV2 changed after the pod on the Witness was deleted"
has_member "$RV2" "$HW" Witness || fail "the Witness entry of $HW is gone"
echo "-- observation 2 on the Witness $WITNESS after the pod was deleted:"
drbd_status "$WITNESS" "$RV2"

# --- h ------------------------------------------------------------------------

step h "h. restart of DaemonSet ambar under the writer r3 on $N2"
counter() { kc exec r3 -- cat /data/counter 2>/dev/null || true; }
c_before="$(counter)"
[[ "$c_before" =~ ^[0-9]+$ ]] || fail "r3 does not write a counter ('$c_before')"
echo "counter before the restart: $c_before"
roll_log="$(mktemp)"
t0=$SECONDS
( kc -n "$NS" rollout restart ds/ambar >/dev/null && kc -n "$NS" rollout status ds/ambar --timeout=240s ) >"$roll_log" 2>&1 &
roll_pid=$!
samples=("$c_before")
# Sample the counter without a pause until the rollout ends (one `kc exec` is
# about one second).
while kill -0 "$roll_pid" 2>/dev/null; do
    c="$(counter)"
    if [[ "$c" =~ ^[0-9]+$ ]]; then samples+=("$c"); fi
done
roll_rc=0
wait "$roll_pid" || roll_rc=$?
roll_s=$(( SECONDS - t0 ))
tail -n 2 "$roll_log"
rm -f "$roll_log"
[[ "$roll_rc" == 0 ]] || fail "the DaemonSet rollout failed"
echo "time: rollout restart and status took $roll_s s"
echo "counter samples during the restart: ${samples[*]}"
prev=-1
for c in "${samples[@]}"; do
    (( c >= prev )) || fail "the counter went back: $prev -> $c (samples: ${samples[*]})"
    prev="$c"
done
last="${samples[${#samples[@]}-1]}"
sleep 8
c_after="$(counter)"
total=$(( SECONDS - t0 ))
echo "counter after the restart: $c_after ($total s after the restart started)"
[[ "$c_after" =~ ^[0-9]+$ ]] && (( c_after > last + 3 )) || fail "the counter does not grow after the restart: $last -> '$c_after'"
# The writer adds one per second; a stall of the volume during the restart would show here.
(( c_after - c_before >= total / 2 )) || fail "the counter grew by $(( c_after - c_before )) in $total s, expected at least $(( total / 2 ))"
[[ "$(kc get pod r3 -o jsonpath='{.status.containerStatuses[0].restartCount}')" == 0 ]] || fail "r3 was restarted"
# The agents report status only when it differs from the object. Spoil one entry
# and wait for the new agent of that node to write it back.
idx="$(rvjp "$RV3" '{range .status.nodes[*]}{.name}{"\n"}{end}' | grep -n "^$H2\$" | cut -d: -f1 || true)"
if [[ -n "$idx" ]]; then
    kc patch rv "$RV3" --subresource=status --type=json \
        -p "[{\"op\":\"replace\",\"path\":\"/status/nodes/$((idx - 1))/message\",\"value\":\"e2e-spoiled\"}]" >/dev/null
    wait_until 60 "the agent on $N2 did not rewrite its status entry" \
        '[[ "$(rvjp "$RV3" "{.status.nodes[$((idx - 1))].message}")" != e2e-spoiled ]]'
    echo "time: agent on $N2 rewrote its spoiled status entry after $WAITED s"
else
    fail "$RV3 has no status entry for $H2"
fi
wait_until 60 "all status entries of $RV3 are UpToDate after the restart" \
    '[[ "$(disks "$RV3" | grep -c "=UpToDate\$" || true)" == 3 ]]'
disks "$RV3"

# --- i ------------------------------------------------------------------------

step i "i. StorageClass with replicas 1"
kc apply -f - <<EOF >/dev/null
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: e2e-r1
provisioner: ambar.balookrd.github.io
volumeBindingMode: WaitForFirstConsumer
reclaimPolicy: Delete
parameters:
  replicas: "1"
  protocol: "C"
  csi.storage.k8s.io/fstype: ext4
EOF
pvc_apply e2e-r1 e2e-r1
R1_UID="$(kc get pvc e2e-r1 -o jsonpath='{.metadata.uid}')"
pod_apply p1 e2e-r1 "$H1" 'cat /data/f'
wait_until 90 "no provisioning failure event for e2e-r1" \
    'kc get events --field-selector involvedObject.uid=$R1_UID,reason=ProvisioningFailed -o jsonpath="{.items[*].message}" | grep -qi "invalid.\?argument"'
echo "time: ProvisioningFailed event with the error after $WAITED s"
kc get events --field-selector involvedObject.uid=$R1_UID,reason=ProvisioningFailed -o jsonpath='{.items[0].message}'; echo
[[ "$(kc get pvc e2e-r1 -o jsonpath='{.status.phase}')" == Pending ]] || fail "PVC e2e-r1 is not Pending"
rv_list="$(kc get rv -o name)"
extra="$(grep -v -e "$RV3" -e "$RV2" <<<"$rv_list" || true)"
[[ -z "$extra" ]] || fail "a ReplicatedVolume was created for replicas: 1: $extra"
kc delete pod p1 --wait=true --timeout=60s >/dev/null
kc delete pvc e2e-r1 --wait=true --timeout=60s >/dev/null
kc delete sc e2e-r1 >/dev/null

# --- j ------------------------------------------------------------------------

step j "j. delete pods and PVCs"
kc delete pod r3 --ignore-not-found --wait=true --timeout=120s >/dev/null
J_START=$SECONDS
kc delete pvc e2e-r3 e2e-r2 --wait=true --timeout=120s >/dev/null
echo "time: PVCs gone $(( SECONDS - J_START )) s after the delete"
wait_until 120 "ReplicatedVolume objects are still there" no_rvs
echo "time: ReplicatedVolume objects gone $(( SECONDS - J_START )) s after the PVC delete"
for n in "${NODES[@]}"; do
    left="$(files_of "$n" "$RV3" "$RV2")"
    [[ -z "$left" ]] || fail "$n: files of the volumes are left: $left"
    loops="$(loops_of "$n" "$RV3" "$RV2")"
    [[ -z "$loops" ]] || fail "$n: loop devices of the volumes are left: $loops"
    st="$(ag "$n" "drbdsetup status 2>&1 || true")"
    [[ "$st" != *"$RV3"* && "$st" != *"$RV2"* ]] || fail "$n: drbdsetup status shows the volumes: $st"
    echo "$n: no files, no loop devices; drbdsetup status: ${st:-<empty>}"
done

# --- k ------------------------------------------------------------------------

step k "k. the object of a volume is deleted while the agent of a copy is away (R10)"
pvc_apply e2e-k ambar-r2
pod_apply pk e2e-k "$H1" 'echo e2e-k-data > /data/f; sync'
pod_ready pk 120
RVK="$(rv_of_pvc e2e-k)"
echo "volume: $RVK"
[[ "$(pod_cat pk /data/f)" == e2e-k-data ]] || fail "pk does not read back /data/f"
pod_gone pk
# The name goes to the cluster before anything is removed: a later run cleans
# up the orphan file of this volume if this one is interrupted (see "leftovers").
kc -n default create cm ambar-e2e-state --from-literal=orphan="$RVK" --dry-run=client -o yaml | kc apply -f - >/dev/null
members "$RVK"
# The copy that loses its agent: a Replica other than $N1.
KNODE=""
for n in "${NODES[@]}"; do
    [[ "$n" == "$N1" ]] && continue
    has_member "$RVK" "$(vm_host "$n")" Replica && { KNODE="$n"; break; }
done
[[ -n "$KNODE" ]] || fail "no second Replica found in $RVK"
HK="$(vm_host "$KNODE")"
echo "the agent leaves $KNODE"
# Method: a nodeAffinity term `$SKIP_LABEL DoesNotExist` in the DaemonSet and the
# label on the node. The template change rolls all agents once; the label then
# removes the pod of one node. Both are undone at the end of the step, and by the
# EXIT trap if the script stops earlier.
kc -n "$NS" patch ds/ambar --type=merge -p "{\"spec\":{\"template\":{\"spec\":{\"affinity\":{\"nodeAffinity\":{\"requiredDuringSchedulingIgnoredDuringExecution\":{\"nodeSelectorTerms\":[{\"matchExpressions\":[{\"key\":\"$SKIP_LABEL\",\"operator\":\"DoesNotExist\"}]}]}}}}}}}" >/dev/null
DS_PATCHED=yes
kc -n "$NS" rollout status ds/ambar --timeout=240s >/dev/null
kc label node "$HK" "$SKIP_LABEL=true" >/dev/null
LABELED="$HK"
wait_until 120 "the agent pod on $KNODE is still there" 'agent_gone "$KNODE"'
echo "time: agent pod on $KNODE gone after $WAITED s"
kc delete rv "$RVK" --wait=false >/dev/null
# The agents of the other nodes remove their copies and finalizers; only the
# finalizer of $KNODE stays. Assert that, then drop it by hand.
wait_until 120 "finalizers other than the one of $KNODE are still on $RVK" \
    '[[ "$(rvjp "$RVK" "{.metadata.finalizers[*]}")" == "ambar.balookrd.github.io/replica-$HK" ]]'
echo "time: only the finalizer of $KNODE left after $WAITED s"
echo "finalizers before the manual removal: $(rvjp "$RVK" '{.metadata.finalizers[*]}')"
for n in "${NODES[@]}"; do
    [[ "$n" == "$KNODE" ]] && continue
    left="$(files_of "$n" "$RVK")"
    [[ -z "$left" ]] || fail "$n still has files of $RVK although its finalizer is gone: $left"
done
echo "the other nodes have no files of $RVK"
kc patch rv "$RVK" --type=merge -p '{"metadata":{"finalizers":null}}' >/dev/null
wait_until 30 "$RVK is not deleted" no_rvs
kc delete pvc e2e-k --wait=true --timeout=120s >/dev/null
echo "-- on $KNODE with no agent:"
files_of "$KNODE" "$RVK"
[[ -n "$(files_of "$KNODE" "$RVK")" ]] || fail "$KNODE: the files of $RVK are gone before the agent returned"
vm "$KNODE" "test -f $VOL_DIR/$RVK.img" || fail "$KNODE: $VOL_DIR/$RVK.img is gone before the agent returned"
restore_agent_placement
wait_until 180 "the agent pod on $KNODE is not back" '[[ -n "$(agent_pod "$KNODE")" ]]'
kc -n "$NS" rollout status ds/ambar --timeout=240s >/dev/null
echo "time: agent pod on $KNODE back after $WAITED s"
back_pod="$(agent_pod "$KNODE")"
# The refusal is logged on every pass over the volume: wait for two lines, so
# that the file is checked after the agent has had a second look at it.
orphan_lines() { kc -n "$NS" logs "$back_pod" -c ambar | grep -i error | grep -cF "$RVK" || true; }
wait_until 150 "fewer than two error lines with $RVK in the agent log of $KNODE" \
    '(( $(orphan_lines) >= 2 ))'
echo "time: two error lines after $WAITED s"
echo "-- agent log of $KNODE, lines with the volume:"
kc -n "$NS" logs "$back_pod" -c ambar | grep -F "$RVK"
vm "$KNODE" "test -f $VOL_DIR/$RVK.img" || fail "$KNODE: $VOL_DIR/$RVK.img was removed by the agent without an object"
echo "$KNODE keeps $VOL_DIR/$RVK.img"
echo "-- leftovers of $RVK on all nodes:"
for n in "${NODES[@]}"; do
    left="$(files_of "$n" "$RVK")"
    echo "$n: $(tr '\n' ' ' <<<"$left")"
    if [[ "$n" != "$KNODE" ]]; then
        [[ -z "$left" ]] || fail "$n has files of $RVK: $left"
    fi
done
echo "cleanup on $KNODE: $(ag "$KNODE" "ambar remove --name $RVK" 2>&1)"
for n in "${NODES[@]}"; do
    left="$(files_of "$n" "$RVK")"
    [[ -z "$left" ]] || fail "$n: files of $RVK are left after ambar remove: $left"
    loops="$(loops_of "$n" "$RVK")"
    [[ -z "$loops" ]] || fail "$n: loop devices of $RVK are left after ambar remove: $loops"
done
kc -n default delete cm ambar-e2e-state >/dev/null
echo
echo "OK: csi e2e"
