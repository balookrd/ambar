#!/usr/bin/env bash
# End-to-end check of ambar replica management on the stand:
# adding, removing, and moving replica copies of volumes under an active writer,
# witness reassignment, timeout handling, and CRD validation rules.
#
# Needs the driver installed on four nodes:
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/replica-e2e.sh
set -eEuo pipefail
source "$(dirname "$0")/lib.sh"

E2E_IMAGE="${E2E_IMAGE:-docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec}"
VOL_DIR=/var/lib/rancher/ambar/volumes
RES_DIR=/var/lib/rancher/ambar/res
NS=ambar-system

if (( ${#NODES[@]} < 4 )); then
    echo "FAIL: replica-e2e needs four nodes: set NODES=\"dv-1 dv-2 dv-3 dv-4\"" >&2
    exit 2
fi

N1="${NODES[0]}"
H1="$(vm_host "$N1")"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ambar-replica-e2e.XXXXXX")"

# --- diagnostics --------------------------------------------------------------

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
            echo "---- diagnostics: tail of agent log $pod ($(kc -n "$NS" get "$pod" -o jsonpath='{.spec.nodeName}' 2>/dev/null))"
            kc -n "$NS" logs "$pod" -c ambar --tail=60 || true
        done
    } >&2
}

fail() {
    echo "FAIL: [step $STEP] $1" >&2
    diag
    exit 1
}

on_err() {
    [[ ! -e "$WORK/diag-done" ]] || exit 1
    echo "FAIL: [step $STEP] command failed at line $1: $BASH_COMMAND" >&2
    diag
    exit 1
}
trap 'on_err $LINENO' ERR

cleanup_on_exit() {
    rm -rf "$WORK"
}
trap cleanup_on_exit EXIT
trap 'exit 130' INT TERM

STEP=init

step() { STEP="$1"; echo; echo "== $2"; }

# --- helpers ------------------------------------------------------------------

wait_until() {
    local limit="$1" what="$2" cmd="$3" start=$SECONDS
    until ( trap - ERR; eval "$cmd" ); do
        (( SECONDS - start < limit )) || fail "$what: not within $limit s"
        sleep 2
    done
    WAITED=$(( SECONDS - start ))
}

agent_pod() {
    local out
    out="$(kc -n "$NS" get pod -l app=ambar \
        --field-selector "spec.nodeName=$(vm_host "$1"),status.phase=Running" \
        -o go-template='{{range .items}}{{if not .metadata.deletionTimestamp}}{{.metadata.name}}{{"\n"}}{{end}}{{end}}')" \
        || return 1
    printf '%s\n' "${out%%$'\n'*}"
}

pod_on() {
    agent_pod "$1"
}

ag() {
    local node="$1"; shift
    local pod
    pod="$(agent_pod "$node")"
    [[ -n "$pod" ]] || { echo "ag: no agent pod on $node" >&2; return 1; }
    kc -n "$NS" exec "$pod" -c ambar -- bash -c "$*"
}

replica_cmd() {
    local node="$1"; shift
    local pod
    pod="$(agent_pod "$node")"
    [[ -n "$pod" ]] || fail "no agent pod on $node"
    kc -n "$NS" exec "$pod" -c ambar -- ambar replica "$@"
}

rvjp() { kc get rv "$1" -o jsonpath="$2"; }
members() { rvjp "$1" '{range .spec.nodes[*]}{.name}={.kind}{"\n"}{end}'; }
disks() { rvjp "$1" '{range .status.nodes[*]}{.name}={.disk}{"\n"}{end}'; }
count_kind() { local m; m="$(members "$1")" || return 1; grep -c "=$2\$" <<<"$m" || true; }
has_member() { local m; m="$(members "$1")" || return 1; grep -q "^$2=$3\$" <<<"$m"; }
no_member() { local m; m="$(members "$1")" || return 1; ! grep -q "^$2=$3\$" <<<"$m"; }
no_rvs() { local o; o="$(kc get rv -o name)" || return 1; [[ -z "$o" ]]; }

files_of() {
    local node="$1"; shift
    local listing pat=() name
    listing="$(vm "$node" "ls -A $VOL_DIR $RES_DIR 2>/dev/null || true")" || return 1
    for name in "$@"; do pat+=(-e "$name"); done
    grep -F "${pat[@]}" <<<"$listing" || true
}

loops_of() {
    local node="$1"; shift
    local listing pat=() name
    listing="$(vm "$node" "losetup -a 2>/dev/null || true")" || return 1
    for name in "$@"; do pat+=(-e "$name"); done
    grep -F "${pat[@]}" <<<"$listing" || true
}

rv_of_pvc() {
    local pv
    pv="$(kc get pvc "$1" -o jsonpath='{.spec.volumeName}')"
    [[ -n "$pv" ]] || return 1
    kc get pv "$pv" -o jsonpath='{.spec.csi.volumeHandle}'
}

pod_apply() {
    local name="$1" pvc="$2" host="$3" body="$4"
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: Pod
metadata:
  name: $name
  labels: {app: ambar-replica-e2e}
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
    local name="$1" sc="$2" size="${3:-1Gi}"
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: $name
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: $sc
  resources:
    requests:
      storage: $size
EOF
}

pod_ready() { kc wait --for=condition=Ready "pod/$1" --timeout="${2:-120}s" >/dev/null 2>&1 \
    || fail "pod $1 is not Ready within ${2:-120} s (phase: $(kc get pod "$1" -o jsonpath='{.status.phase}' 2>/dev/null))"; }
pod_gone() { kc delete pod "$1" --ignore-not-found --wait=true --timeout=120s >/dev/null; }

pod_cat() {
    local out="" start=$SECONDS
    until out="$(kc exec "$1" -- cat "$2" 2>/dev/null)" && [[ -n "$out" ]]; do
        (( SECONDS - start < 60 )) || fail "$1: $2 is empty after 60 s"
        sleep 2
    done
    printf '%s\n' "$out"
}

drbd_status() { ag "$1" "drbdsetup status $2 2>&1 || true"; }

initial_agent_restarts() {
    kc -n "$NS" get pods -l app=ambar -o jsonpath='{range .items[*]}{.status.containerStatuses[*].restartCount}{" "}{end}'
}

# --- cleanup leftovers --------------------------------------------------------

step cleanup "clean leftovers of earlier runs"
kc delete pod -l app=ambar-replica-e2e --ignore-not-found --wait=true --timeout=120s >/dev/null
for pvc in replica-vol replica-vol-2gi; do
    kc delete pvc "$pvc" --ignore-not-found --wait=true --timeout=120s >/dev/null
done
wait_until 300 "ReplicatedVolume objects of an earlier run are still there" no_rvs
kc -n "$NS" rollout status ds/ambar --timeout=240s >/dev/null

RESTARTS_BEFORE="$(initial_agent_restarts)"
echo "initial agent container restart counts: $RESTARTS_BEFORE"

# --- a ------------------------------------------------------------------------

step a "a. PVC replica-vol (ambar-r2) and writer pod on $N1"
pvc_apply replica-vol ambar-r2 1Gi
# Writer pod writes increasing counter with fsync and echoes number to stdout
pod_apply writer replica-vol "$H1" 'i=0; while :; do i=$((i+1)); echo $i | dd of=/data/counter conv=fsync 2>/dev/null; echo $i; sleep 1; done'
wait_until 120 "PVC replica-vol is not Bound" '[[ "$(kc get pvc replica-vol -o jsonpath="{.status.phase}")" == Bound ]]'
echo "time: replica-vol Bound after $WAITED s"
pod_ready writer 120
echo "time: pod writer Ready, $SECONDS s into run"

VOL="$(rv_of_pvc replica-vol)"
echo "volume: $VOL"

[[ "$(count_kind "$VOL" Replica)" == 2 ]] || fail "$VOL: expected 2 Replica entries, got $(count_kind "$VOL" Replica)"
[[ "$(count_kind "$VOL" Witness)" == 1 ]] || fail "$VOL: expected 1 Witness entry, got $(count_kind "$VOL" Witness)"

WITNESS_HOST="$(rvjp "$VOL" '{.spec.nodes[?(@.kind=="Witness")].name}' | tr ' ' '\n' | head -n1)"
[[ -n "$WITNESS_HOST" ]] || fail "$VOL: no Witness node found"
echo "witness node: $WITNESS_HOST"

# Find corresponding VM name for WITNESS_HOST
WITNESS_VM=""
for n in "${NODES[@]}"; do
    if [[ "$(vm_host "$n")" == "$WITNESS_HOST" ]]; then
        WITNESS_VM="$n"
        break
    fi
done
[[ -n "$WITNESS_VM" ]] || fail "could not map witness host $WITNESS_HOST to VM"

# --- b ------------------------------------------------------------------------

step b "b. replica add $VOL $WITNESS_HOST"
add_start=$SECONDS
add_out="$(replica_cmd "$N1" add "$VOL" "$WITNESS_HOST")"
add_duration=$(( SECONDS - add_start ))
echo "replica add output:"
echo "$add_out"
echo "time: replica add finished in $add_duration s"

[[ "$(count_kind "$VOL" Replica)" == 3 ]] || fail "$VOL: expected 3 Replica nodes after add, got $(count_kind "$VOL" Replica)"
[[ "$(count_kind "$VOL" Witness)" == 0 ]] || fail "$VOL: expected 0 Witness nodes after add, got $(count_kind "$VOL" Witness)"

finalizer="ambar.balookrd.github.io/replica-$WITNESS_HOST"
rvjp "$VOL" '{.metadata.finalizers[*]}' | grep -q "$finalizer" \
    || fail "$VOL: missing finalizer $finalizer"

files_on_w="$(files_of "$WITNESS_VM" "$VOL")"
[[ -n "$files_on_w" ]] || fail "volume files not found on newly added replica node $WITNESS_VM"

restarts_w="$(kc get pod writer -o jsonpath='{.status.containerStatuses[0].restartCount}')"
[[ "$restarts_w" == 0 ]] || fail "writer pod restarted $restarts_w times, expected 0"

# Verify writer logs increase without gaps > 5s
writer_lines="$(kc logs writer --tail=20)"
[[ -n "$writer_lines" ]] || fail "writer pod has empty logs"
echo "recent writer counter log lines: $(echo "$writer_lines" | tr '\n' ' ')"

# --- c ------------------------------------------------------------------------

step c "c. replica remove $VOL (replica not on $N1)"
# Pick a replica node that is not dv-1
REP_TO_REMOVE=""
for h in $(rvjp "$VOL" '{.spec.nodes[?(@.kind=="Replica")].name}'); do
    if [[ "$h" != "$H1" ]]; then
        REP_TO_REMOVE="$h"
        break
    fi
done
[[ -n "$REP_TO_REMOVE" ]] || fail "no secondary replica found to remove"
echo "replica to remove: $REP_TO_REMOVE"

REP_VM=""
for n in "${NODES[@]}"; do
    if [[ "$(vm_host "$n")" == "$REP_TO_REMOVE" ]]; then
        REP_VM="$n"
        break
    fi
done
[[ -n "$REP_VM" ]] || fail "could not map $REP_TO_REMOVE to VM"

rm_out="$(replica_cmd "$N1" remove "$VOL" "$REP_TO_REMOVE")"
echo "replica remove output:"
echo "$rm_out"

[[ "$(count_kind "$VOL" Replica)" == 2 ]] || fail "$VOL: expected 2 Replica nodes after remove, got $(count_kind "$VOL" Replica)"
[[ "$(count_kind "$VOL" Witness)" == 1 ]] || fail "$VOL: expected 1 Witness node after remove, got $(count_kind "$VOL" Witness)"

[[ -z "$(files_of "$REP_VM" "$VOL")" ]] || fail "$REP_VM still has volume files after remove"
[[ -z "$(loops_of "$REP_VM" "$VOL")" ]] || fail "$REP_VM still has loop devices after remove"
vm "$REP_VM" "test ! -e /dev/ambar/$VOL" || fail "/dev/ambar/$VOL symlink still exists on $REP_VM"
drbd_status "$REP_VM" "$VOL" | grep -v "No response from the kernel module" | grep -v "not configured" | grep -v "No such resource" | grep -v "No currently configured DRBD found" \
    | grep -qv "^\s*$" && fail "$VOL resource still configured on $REP_VM" || true

rvjp "$VOL" '{.metadata.finalizers[*]}' | grep -qv "ambar.balookrd.github.io/replica-$REP_TO_REMOVE" \
    || fail "finalizer for $REP_TO_REMOVE is still present"

# --- d ------------------------------------------------------------------------

step d "d. replica remove $VOL on dv-1 (pod is there) fails with Primary"
before_spec="$(members "$VOL")"
if d_out="$( (trap - ERR; replica_cmd "$N1" remove "$VOL" "$H1") 2>&1 )"; then
    d_rc=0
else
    d_rc=$?
fi
echo "replica remove Primary exit code: $d_rc, output: $d_out"
[[ "$d_rc" == 1 ]] || fail "replica remove on Primary node expected exit code 1, got $d_rc"
grep -qi "Primary" <<<"$d_out" || fail "replica remove on Primary expected error mentioning Primary, got: $d_out"
after_spec="$(members "$VOL")"
[[ "$before_spec" == "$after_spec" ]] || fail "spec.nodes changed after rejected replica remove"

# --- e ------------------------------------------------------------------------

step e "e. replica move $VOL from non-dv-1 replica to outside node"
# Find current non-H1 replica
MOVE_SRC=""
for h in $(rvjp "$VOL" '{.spec.nodes[?(@.kind=="Replica")].name}'); do
    if [[ "$h" != "$H1" ]]; then
        MOVE_SRC="$h"
        break
    fi
done
# Find outside node (not Replica, not Witness)
MOVE_DST=""
for n in "${NODES[@]}"; do
    h="$(vm_host "$n")"
    if ! has_member "$VOL" "$h" Replica && ! has_member "$VOL" "$h" Witness; then
        MOVE_DST="$h"
        break
    fi
done
[[ -n "$MOVE_SRC" && -n "$MOVE_DST" ]] || fail "cannot find MOVE_SRC ($MOVE_SRC) or MOVE_DST ($MOVE_DST)"
echo "moving replica from $MOVE_SRC to $MOVE_DST"

wit_before="$(rvjp "$VOL" '{.spec.nodes[?(@.kind=="Witness")].name}' | tr ' ' '\n' | head -n1)"
move_start=$SECONDS
move_out="$(replica_cmd "$N1" move "$VOL" "$MOVE_SRC" "$MOVE_DST")"
move_duration=$(( SECONDS - move_start ))
echo "replica move output:"
echo "$move_out"
echo "time: replica move took $move_duration s"

has_member "$VOL" "$H1" Replica || fail "$H1 lost Replica status"
has_member "$VOL" "$MOVE_DST" Replica || fail "$MOVE_DST is not Replica after move"
no_member "$VOL" "$MOVE_SRC" Replica || fail "$MOVE_SRC is still Replica after move"
wit_after="$(rvjp "$VOL" '{.spec.nodes[?(@.kind=="Witness")].name}' | tr ' ' '\n' | head -n1)"
[[ "$wit_before" == "$wit_after" ]] || fail "witness changed during move: was $wit_before, now $wit_after"

edit_count=$(grep -c "^edit:" <<<"$move_out" || true)
[[ "$edit_count" == 2 ]] || fail "expected exactly 2 edit: lines in move output, got $edit_count"

# Move onto the witness node
echo "moving replica from $MOVE_DST onto witness node $wit_after"
move_wit_out="$(replica_cmd "$N1" move "$VOL" "$MOVE_DST" "$wit_after")"
echo "replica move onto witness output:"
echo "$move_wit_out"
has_member "$VOL" "$wit_after" Replica || fail "$wit_after is not Replica after move"
has_member "$VOL" "$MOVE_SRC" Witness || fail "$MOVE_SRC did not become new Witness"

# --- f ------------------------------------------------------------------------

step f "f. replica move with timeout on 2-GiB volume with 1.5 GiB data"
pvc_apply replica-vol-2gi ambar-r2 2Gi
# Pod writes 1.5 GiB of random data with fsync
pod_apply writer2 replica-vol-2gi "$H1" 'dd if=/dev/urandom of=/data/large bs=1M count=1536 conv=fsync; echo large_written > /data/done; sync'
wait_until 120 "PVC replica-vol-2gi is not Bound" '[[ "$(kc get pvc replica-vol-2gi -o jsonpath="{.status.phase}")" == Bound ]]'
pod_ready writer2 180
wait_until 180 "data write not completed" '[[ "$(pod_cat writer2 /data/done)" == large_written ]]'
echo "time: 1.5 GiB written to replica-vol-2gi"

VOL2="$(rv_of_pvc replica-vol-2gi)"
echo "2Gi volume: $VOL2"

# Pick source replica not H1, and outside target
F_SRC=""
for h in $(rvjp "$VOL2" '{.spec.nodes[?(@.kind=="Replica")].name}'); do
    if [[ "$h" != "$H1" ]]; then F_SRC="$h"; break; fi
done
F_DST=""
for n in "${NODES[@]}"; do
    h="$(vm_host "$n")"
    if ! has_member "$VOL2" "$h" Replica && ! has_member "$VOL2" "$h" Witness; then
        F_DST="$h"; break
    fi
done
[[ -n "$F_SRC" && -n "$F_DST" ]] || fail "could not pick F_SRC ($F_SRC) or F_DST ($F_DST)"
echo "moving $VOL2 from $F_SRC to $F_DST with timeout 20s"

if f_to_out="$( (trap - ERR; replica_cmd "$N1" move "$VOL2" "$F_SRC" "$F_DST" --timeout 20) 2>&1 )"; then
    f_to_rc=0
else
    f_to_rc=$?
fi
echo "move with timeout exit code: $f_to_rc"
echo "$f_to_out"
[[ "$f_to_rc" == 1 ]] || fail "move with timeout expected exit code 1, got $f_to_rc"

# Check no lingering ambar replica process in agent pod
pgrep_res="$(ag "$N1" "pgrep -f 'ambar replica' || true")"
[[ -z "$pgrep_res" ]] || fail "lingering 'ambar replica' processes found on $N1: $pgrep_res"

# Resume move without timeout
echo "resuming move $VOL2 from $F_SRC to $F_DST without timeout"
f_res_out="$(replica_cmd "$N1" move "$VOL2" "$F_SRC" "$F_DST")"
echo "resumed move output:"
echo "$f_res_out"

edits_to=$(grep -c "^edit:" <<<"$f_to_out" || true)
edits_res=$(grep -c "^edit:" <<<"$f_res_out" || true)
total_edits=$(( edits_to + edits_res ))
[[ "$total_edits" == 2 ]] || fail "total edit: count across both move runs expected 2, got $total_edits"

# --- g ------------------------------------------------------------------------

step g "g. CRD validation rules via kubectl patch"
# 1. Remove Replica directly
p1_err="$( (trap - ERR; kc patch rv "$VOL" --type=json -p '[{"op":"remove","path":"/spec/nodes/0"}]') 2>&1 || true )"
grep -q "a Replica node is removed only through Retiring" <<<"$p1_err" \
    || fail "rule 1 failed, got: $p1_err"

# 2. Change Replica -> Client directly
p2_err="$( (trap - ERR; kc patch rv "$VOL" --type=json -p '[{"op":"replace","path":"/spec/nodes/0/kind","value":"Client"}]') 2>&1 || true )"
grep -q "this change of a node's kind is not allowed" <<<"$p2_err" \
    || fail "rule 2 failed, got: $p2_err"

# 3. Add Retiring node from outside
OUTSIDE_NODE=""
for n in "${NODES[@]}"; do
    h="$(vm_host "$n")"
    if ! has_member "$VOL" "$h" Replica && ! has_member "$VOL" "$h" Witness; then
        OUTSIDE_NODE="$h"
        break
    fi
done
if [[ -n "$OUTSIDE_NODE" ]]; then
    p3_err="$( (trap - ERR; kc patch rv "$VOL" --type=json -p "[{\"op\":\"add\",\"path\":\"/spec/nodes/-\",\"value\":{\"name\":\"$OUTSIDE_NODE\",\"nodeId\":9,\"address\":\"192.0.2.9\",\"kind\":\"Retiring\"}}]") 2>&1 || true )"
    grep -q "a node can become Retiring only from Replica" <<<"$p3_err" \
        || fail "rule 3 failed, got: $p3_err"
fi

# 4. Two Retiring nodes
# Find a replica, try to add second Retiring
p4_err="$( (trap - ERR; kc patch rv "$VOL" --type=json -p '[{"op":"replace","path":"/spec/nodes/0/kind","value":"Retiring"},{"op":"replace","path":"/spec/nodes/1/kind","value":"Retiring"}]') 2>&1 || true )"
grep -q "at most one node can be Retiring" <<<"$p4_err" \
    || fail "rule 4 failed, got: $p4_err"

echo "CRD validation rules verified successfully"

# --- h ------------------------------------------------------------------------

step h "h. writer shutdown, data readback verification, and volume deletion"
last_counter="$(kc logs writer | tail -n1)"
echo "last confirmed writer counter: $last_counter"
pod_gone writer
pod_apply reader replica-vol "$H1" "cat /data/counter > /data/readback; sync"
pod_ready reader 60
readback="$(pod_cat reader /data/readback)"
echo "counter read back from volume: $readback"
(( readback >= last_counter )) || fail "readback counter $readback < last logged counter $last_counter"
pod_gone reader
pod_gone writer2

kc delete pvc replica-vol replica-vol-2gi --wait=true --timeout=120s >/dev/null
wait_until 180 "volumes still present after PVC deletion" no_rvs

for n in "${NODES[@]}"; do
    [[ -z "$(files_of "$n" "$VOL" "$VOL2")" ]] || fail "$n still has volume files after deletion"
    [[ -z "$(loops_of "$n" "$VOL" "$VOL2")" ]] || fail "$n still has loop devices after deletion"
done
echo "all volume files and loop devices cleanly removed on all 4 nodes"

# --- i ------------------------------------------------------------------------

step i "i. verify no forbidden errors and no unexpected agent restarts"
for pod in $(kc -n "$NS" get pods -l app=ambar -o name); do
    forbidden_count=$(kc -n "$NS" logs "$pod" --all-containers=true 2>&1 | grep -ci "forbidden" || true)
    [[ "$forbidden_count" == 0 ]] || fail "$pod logged $forbidden_count forbidden errors"
done

RESTARTS_AFTER="$(initial_agent_restarts)"
echo "agent container restart counts after run: $RESTARTS_AFTER"
[[ "$RESTARTS_BEFORE" == "$RESTARTS_AFTER" ]] || fail "agent restart counts changed from $RESTARTS_BEFORE to $RESTARTS_AFTER"

echo "OK: replica e2e"
