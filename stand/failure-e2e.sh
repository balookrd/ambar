#!/usr/bin/env bash
# End-to-end check of ambar failure handling and node recovery on the stand:
# - metrics HTTP endpoint on :9770/metrics (R38);
# - DRBD quorum loss detection and no-quorum node taint (R32);
# - 30s grace period and pod eviction on persistent quorum loss (R33);
# - automatic adjust of StandAlone secondary connections (R34);
# - split-brain detection and manual_check_needed metric (R35);
# - stale Client cleanup when pods are gone (R36);
# - post-failure cluster self-healing and stand cleanliness.
#
# Needs the driver installed on four nodes:
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/install.sh
#   NODES="dv-1 dv-2 dv-3 dv-4" stand/failure-e2e.sh
set -eEuo pipefail
source "$(dirname "$0")/lib.sh"

E2E_IMAGE="${E2E_IMAGE:-docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec}"
VOL_DIR=/var/lib/rancher/ambar/volumes
RES_DIR=/var/lib/rancher/ambar/res
NS=ambar-system
APP_LABEL=ambar-failure-e2e

if (( ${#NODES[@]} < 4 )); then
    echo "FAIL: failure-e2e needs four nodes: set NODES=\"dv-1 dv-2 dv-3 dv-4\"" >&2
    exit 2
fi

N1="${NODES[0]}"
N2="${NODES[1]}"
N3="${NODES[2]}"
N4="${NODES[3]}"

H1="$(vm_host "$N1")"
H2="$(vm_host "$N2")"
H3="$(vm_host "$N3")"
H4="$(vm_host "$N4")"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ambar-failure-e2e.XXXXXX")"

# --- diagnostics & error handling --------------------------------------------

diag() {
    [[ ! -e "$WORK/diag-done" ]] || return 0
    : > "$WORK/diag-done"
    {
        echo "---- diagnostics: kc get nodes -o wide"
        kc get nodes -o wide || true
        echo "---- diagnostics: node taints"
        kc get nodes -o jsonpath='{range .items[*]}{.metadata.name}{": "}{.spec.taints}{"\n"}{end}' || true
        echo "---- diagnostics: kc get rv -o yaml"
        kc get rv -o yaml || true
        echo "---- diagnostics: kc get pods,pvc -o wide"
        kc get pods,pvc -o wide || true
        echo "---- diagnostics: events of namespace default"
        kc -n default get events --sort-by=.lastTimestamp || true
        local pod
        for pod in $(kc -n "$NS" get pods -l app=ambar -o name 2>/dev/null || true); do
            local nh
            nh="$(kc -n "$NS" get "$pod" -o jsonpath='{.spec.nodeName}' 2>/dev/null || true)"
            echo "---- diagnostics: tail of agent log $pod ($nh)"
            kc -n "$NS" logs "$pod" -c ambar --tail=80 || true
        done
        for n in "${NODES[@]}"; do
            echo "---- diagnostics: nftables on $n"
            vm "$n" "nft list tables 2>/dev/null || true"
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
    for n in "${NODES[@]}"; do
        vm "$n" "nft delete table inet ambar_test 2>/dev/null || true"
    done
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

ag() {
    local node="$1"; shift
    local pod
    pod="$(agent_pod "$node")"
    [[ -n "$pod" ]] || { echo "ag: no agent pod on $node" >&2; return 1; }
    kc -n "$NS" exec "$pod" -c ambar -- bash -c "$*"
}

rvjp() { kc get rv "$1" -o jsonpath="$2"; }
members() { rvjp "$1" '{range .spec.nodes[*]}{.name}={.kind}{"\n"}{end}'; }
has_member() { local m; m="$(members "$1")" || return 1; grep -q "^$2=$3\$" <<<"$m"; }
no_member() { local m; m="$(members "$1")" || return 1; ! grep -q "^$2=$3\$" <<<"$m"; }
no_rvs() { local o; o="$(kc get rv -o name)" || return 1; [[ -z "$o" ]]; }

rv_of_pvc() {
    local pv
    pv="$(kc get pvc "$1" -o jsonpath='{.spec.volumeName}')"
    [[ -n "$pv" ]] || return 1
    kc get pv "$pv" -o jsonpath='{.spec.csi.volumeHandle}'
}

node_taint() {
    local host="$1" key="$2"
    kc get node "$host" -o jsonpath="{.spec.taints[?(@.key==\"$key\")].effect}" 2>/dev/null || true
}

node_is_tainted() {
    local host="$1" key="$2"
    local eff
    eff="$(node_taint "$host" "$key")"
    [[ -n "$eff" ]]
}

node_is_untainted() {
    local host="$1" key="$2"
    local eff
    eff="$(node_taint "$host" "$key")"
    [[ -z "$eff" ]]
}

pvc_apply() {
    local name="$1" sc="$2" size="${3:-1Gi}"
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: $name
  labels: {app: $APP_LABEL}
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: $sc
  resources:
    requests:
      storage: $size
EOF
}

deployment_apply() {
    local name="$1" pvc="$2" node_host="$3"
    kc apply -f - <<EOF >/dev/null
apiVersion: apps/v1
kind: Deployment
metadata:
  name: $name
  labels: {app: $APP_LABEL}
spec:
  replicas: 1
  selector:
    matchLabels: {deployment: $name}
  template:
    metadata:
      labels: {app: $APP_LABEL, deployment: $name}
    spec:
      terminationGracePeriodSeconds: 1
      tolerations:
        # Tolerates unreachable/not-ready node to speed up reschedule
        - key: "node.kubernetes.io/unreachable"
          operator: "Exists"
          effect: "NoExecute"
          tolerationSeconds: 5
        - key: "node.kubernetes.io/not-ready"
          operator: "Exists"
          effect: "NoExecute"
          tolerationSeconds: 5
      containers:
        - name: app
          image: $E2E_IMAGE
          command: ["sh", "-c"]
          args:
            - |
              trap 'exit 0' TERM
              i=0
              while :; do
                i=\$((i+1))
                echo "\$(date +%s) \$i \$(hostname)" >> /data/heartbeat
                sleep 1
              done
          volumeMounts:
            - {name: d, mountPath: /data}
      volumes:
        - name: d
          persistentVolumeClaim: {claimName: $pvc}
EOF
}

pod_apply() {
    local name="$1" pvc="$2" host="$3"
    kc apply -f - <<EOF >/dev/null
apiVersion: v1
kind: Pod
metadata:
  name: $name
  labels: {app: $APP_LABEL}
spec:
  nodeSelector:
    kubernetes.io/hostname: $host
  terminationGracePeriodSeconds: 1
  containers:
    - name: reader
      image: $E2E_IMAGE
      command: ["sh", "-c"]
      args:
        - |
          trap 'exit 0' TERM
          while :; do
            if [ -f /data/heartbeat ]; then
              tail -n 1 /data/heartbeat
            fi
            sleep 1
          done
      volumeMounts:
        - {name: d, mountPath: /data}
  volumes:
    - name: d
      persistentVolumeClaim: {claimName: $pvc}
EOF
}

pod_ready() {
    kc wait --for=condition=Ready "pod/$1" --timeout="${2:-120}s" >/dev/null 2>&1 \
        || fail "pod $1 is not Ready within ${2:-120} s"
}

deployment_ready() {
    local name="$1" timeout="${2:-120}"
    kc -n default rollout status "deployment/$name" --timeout="${timeout}s" >/dev/null 2>&1 \
        || fail "deployment $name rollout not ready within ${timeout}s"
}

deployment_pod() {
    local name="$1"
    kc get pods -l "deployment=$name" -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true
}

deployment_node() {
    local name="$1"
    kc get pods -l "deployment=$name" -o jsonpath='{.items[0].spec.nodeName}' 2>/dev/null || true
}

block_drbd_port() {
    local node="$1"
    vm "$node" "nft add table inet ambar_test
        nft add chain inet ambar_test input '{ type filter hook input priority -10; policy accept; }'
        nft add rule inet ambar_test input tcp dport 7000-7050 drop
        nft add chain inet ambar_test output '{ type filter hook output priority -10; policy accept; }'
        nft add rule inet ambar_test output tcp dport 7000-7050 drop"
}

unblock_drbd_port() {
    local node="$1"
    vm "$node" "nft delete table inet ambar_test 2>/dev/null || true"
}

initial_agent_restarts() {
    kc -n "$NS" get pods -l app=ambar -o jsonpath='{range .items[*]}{.status.containerStatuses[*].restartCount}{" "}{end}'
}

# --- 0. cleanup leftovers -----------------------------------------------------

step cleanup "0. clean leftovers of earlier runs and verify initial state"
for n in "${NODES[@]}"; do
    unblock_drbd_port "$n"
done

kc delete deployment -l "app=$APP_LABEL" --ignore-not-found --wait=true --timeout=60s >/dev/null || true
kc delete pod -l "app=$APP_LABEL" --ignore-not-found --wait=true --timeout=60s >/dev/null || true
kc delete pvc -l "app=$APP_LABEL" --ignore-not-found --wait=true --timeout=60s >/dev/null || true
kc delete pvc fail-pvc --ignore-not-found --wait=true --timeout=60s >/dev/null || true

wait_until 180 "ReplicatedVolume objects of an earlier run are still there" no_rvs
kc -n "$NS" rollout status ds/ambar --timeout=120s >/dev/null

# Ensure all nodes are untainted
for n in "${NODES[@]}"; do
    h="$(vm_host "$n")"
    kc taint node "$h" ambar.balookrd.github.io/no-quorum- 2>/dev/null || true
done

RESTARTS_BEFORE="$(initial_agent_restarts)"
echo "initial agent container restart counts: $RESTARTS_BEFORE"

# --- a. Prometheus metrics server (R38) ---------------------------------------

step a "a. verify Prometheus metrics HTTP server on :9770/metrics"

for n in "${NODES[@]}"; do
    metrics="$(vm "$n" "curl -sf http://127.0.0.1:9770/metrics")"
    grep -q "ambar_node_allocated_bytes" <<<"$metrics" \
        || fail "$n: metrics missing ambar_node_allocated_bytes"
    grep -q "ambar_node_tainted" <<<"$metrics" \
        || fail "$n: metrics missing ambar_node_tainted"
    grep -q "ambar_module_info" <<<"$metrics" \
        || fail "$n: metrics missing ambar_module_info"
    
    # Verify 404 on unknown endpoint
    code="$(vm "$n" "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:9770/badpath")"
    [[ "$code" == "404" ]] || fail "$n: expected 404 for /badpath, got $code"
done
echo "metrics endpoint :9770/metrics answers with valid OpenMetrics format on all nodes"

# --- b. Deploy workload on ambar-r2 volume -----------------------------------

step b "b. create ambar-r2 volume and start writer deployment"

pvc_apply fail-pvc ambar-r2 1Gi
deployment_apply fail-writer fail-pvc ""

wait_until 120 "PVC fail-pvc is not Bound" '[[ "$(kc get pvc fail-pvc -o jsonpath="{.status.phase}")" == Bound ]]'
deployment_ready fail-writer 180

VOL="$(rv_of_pvc fail-pvc)"
echo "volume: $VOL"

POD="$(deployment_pod fail-writer)"
WRITER_NODE="$(deployment_node fail-writer)"
echo "writer pod: $POD running on $WRITER_NODE"

# Determine which VM is WRITER_NODE
WRITER_VM=""
for n in "${NODES[@]}"; do
    if [[ "$(vm_host "$n")" == "$WRITER_NODE" ]]; then
        WRITER_VM="$n"
        break
    fi
done
[[ -n "$WRITER_VM" ]] || fail "could not map $WRITER_NODE to VM"

# Verify volume is Primary and quorum=true in DRBD on writer node
drbd_st="$(ag "$WRITER_VM" "drbdsetup status $VOL --json")"
grep -q '"role": *"Primary"' <<<"$drbd_st" || fail "volume $VOL on $WRITER_VM is not Primary"
grep -q '"quorum": *true' <<<"$drbd_st" || fail "volume $VOL on $WRITER_VM has no quorum"

# Verify metrics on WRITER_VM reflect healthy quorum
m="$(vm "$WRITER_VM" "curl -sf http://127.0.0.1:9770/metrics")"
grep -q "ambar_volume_quorum{node=\"$WRITER_NODE\",volume=\"$VOL\"} 1" <<<"$m" \
    || fail "expected ambar_volume_quorum=1 in metrics on $WRITER_VM"
grep -q "ambar_node_tainted{node=\"$WRITER_NODE\"} 0" <<<"$m" \
    || fail "expected ambar_node_tainted=0 in metrics on $WRITER_VM"

# --- c. Isolate writer node network -> quorum loss & no-quorum taint (R32) ---

step c "c. isolate $WRITER_VM replication traffic -> verify no-quorum taint on $WRITER_NODE"

echo "blocking DRBD ports on $WRITER_VM..."
block_drbd_port "$WRITER_VM"
ISOLATE_TIME=$SECONDS

# Wait for quorum loss in DRBD on writer node
wait_until 30 "quorum not lost on $WRITER_VM after network isolation" \
    'ag "$WRITER_VM" "drbdsetup status $VOL --json" 2>/dev/null | grep -q "\"quorum\": *false"'
echo "quorum lost on $WRITER_VM in $WAITED s"

# Wait for no-quorum taint on writer node
wait_until 20 "taint ambar.balookrd.github.io/no-quorum not placed on $WRITER_NODE" \
    'node_is_tainted "$WRITER_NODE" "ambar.balookrd.github.io/no-quorum"'
echo "taint ambar.balookrd.github.io/no-quorum placed on $WRITER_NODE in $WAITED s"

# Verify metric ambar_node_tainted=1 and ambar_volume_quorum=0
m="$(vm "$WRITER_VM" "curl -sf http://127.0.0.1:9770/metrics")"
grep -q "ambar_node_tainted{node=\"$WRITER_NODE\"} 1" <<<"$m" \
    || fail "expected ambar_node_tainted=1 in metrics on $WRITER_VM"
grep -q "ambar_volume_quorum{node=\"$WRITER_NODE\",volume=\"$VOL\"} 0" <<<"$m" \
    || fail "expected ambar_volume_quorum=0 in metrics on $WRITER_VM"

# --- d. Grace period 30s & pod eviction (R33) ---------------------------------

step d "d. verify grace period and pod eviction from $WRITER_NODE"

# During the first 15s after isolation, pod should NOT be evicted yet
ELAPSED=$(( SECONDS - ISOLATE_TIME ))
if (( ELAPSED < 20 )); then
    P="$(deployment_pod fail-writer)"
    [[ -n "$P" ]] || fail "pod was evicted too early ($ELAPSED s < 30 s grace period)"
    echo "pod $P is still present at $ELAPSED s (grace period in progress)"
fi

# After 30s grace period, agent must evict the pod via Kubernetes API
echo "waiting for agent to evict pod from $WRITER_NODE after grace period..."
wait_until 60 "pod not evicted from $WRITER_NODE after grace period" \
    '[[ "$(deployment_node fail-writer)" != "$WRITER_NODE" ]]'
echo "pod evicted from $WRITER_NODE and rescheduled!"

# Wait for deployment to become ready on the surviving node
deployment_ready fail-writer 180
NEW_WRITER_NODE="$(deployment_node fail-writer)"
echo "workload successfully recovered and running on $NEW_WRITER_NODE"
[[ "$NEW_WRITER_NODE" != "$WRITER_NODE" ]] || fail "workload returned to tainted node $WRITER_NODE"

# --- e. Restore network & auto-adjust StandAlone (R34, R35) --------------------

step e "e. restore network on $WRITER_VM -> auto-adjust StandAlone & untaint"

echo "unblocking DRBD ports on $WRITER_VM..."
unblock_drbd_port "$WRITER_VM"

# Agent on WRITER_VM is Secondary and sees StandAlone; it should auto-adjust
wait_until 60 "connection on $WRITER_VM did not reconnect to Connected via auto-adjust" \
    'ag "$WRITER_VM" "drbdsetup status $VOL --json" 2>/dev/null | grep -q "\"connection-state\": *\"Connected\""'
echo "connection on $WRITER_VM reconnected via auto-adjust in $WAITED s"

# Disk should resync and become UpToDate
wait_until 60 "disk on $WRITER_VM not UpToDate" \
    'ag "$WRITER_VM" "drbdsetup status $VOL --json" 2>/dev/null | grep -q "\"disk-state\": *\"UpToDate\""'
echo "disk on $WRITER_VM resynced to UpToDate in $WAITED s"

# Taint must be automatically removed from WRITER_NODE
wait_until 30 "taint no-quorum was not removed from $WRITER_NODE after quorum recovery" \
    'node_is_untainted "$WRITER_NODE" "ambar.balookrd.github.io/no-quorum"'
echo "taint no-quorum automatically removed from $WRITER_NODE in $WAITED s"

# Verify metrics reflect healthy node and no split-brain
m="$(vm "$WRITER_VM" "curl -sf http://127.0.0.1:9770/metrics")"
grep -q "ambar_node_tainted{node=\"$WRITER_NODE\"} 0" <<<"$m" \
    || fail "expected ambar_node_tainted=0 after recovery on $WRITER_VM"
grep -q "ambar_volume_manual_check_needed{node=\"$WRITER_NODE\",volume=\"$VOL\"} 0" <<<"$m" \
    || fail "expected ambar_volume_manual_check_needed=0 on $WRITER_VM"

# Clean up writer deployment
kc delete deployment fail-writer --wait=true --timeout=60s >/dev/null

# --- f. Stale Client cleanup without pods (R36) --------------------------------

step f "f. attach Client on $H4 -> remove pod -> verify automatic Client cleanup"

# Check which node is not in spec.nodes of fail-pvc
CLIENT_HOST="$H4"
CLIENT_VM="$N4"
if has_member "$VOL" "$CLIENT_HOST" Replica || has_member "$VOL" "$CLIENT_HOST" Witness; then
    # Pick a node that is neither Replica nor Witness
    for n in "${NODES[@]}"; do
        h="$(vm_host "$n")"
        if ! has_member "$VOL" "$h" Replica && ! has_member "$VOL" "$h" Witness; then
            CLIENT_HOST="$h"
            CLIENT_VM="$n"
            break
        fi
    done
fi
echo "testing Client cleanup on $CLIENT_HOST ($CLIENT_VM)"

# Run reader pod on CLIENT_HOST
pod_apply client-reader fail-pvc "$CLIENT_HOST"
pod_ready client-reader 120

wait_until 30 "node $CLIENT_HOST is not listed as Client in spec.nodes" \
    'has_member "$VOL" "$CLIENT_HOST" Client'
echo "node $CLIENT_HOST successfully joined as Client"

# Verify volume is configured in DRBD on CLIENT_VM
ag "$CLIENT_VM" "drbdsetup status $VOL --json" >/dev/null

# Now delete reader pod
echo "deleting client-reader pod..."
kc delete pod client-reader --wait=true --timeout=60s >/dev/null

# Agent on CLIENT_VM in tick_failures should detect no pods, run drbdadm down, and leave membership
wait_until 45 "node $CLIENT_HOST was not cleaned up from spec.nodes after pod removal" \
    'no_member "$VOL" "$CLIENT_HOST" Client'
echo "node $CLIENT_HOST automatically cleaned up from spec.nodes in $WAITED s!"

# Verify resource is down in DRBD on CLIENT_VM
res_status="$(ag "$CLIENT_VM" "drbdsetup status $VOL 2>&1 || true")"
[[ "$res_status" =~ "No response from kernel module" || "$res_status" =~ "not configured" || "$res_status" =~ "No such resource" || "$res_status" =~ "No currently configured DRBD found" || -z "$res_status" ]] \
    || fail "resource $VOL on $CLIENT_VM is still configured after client cleanup: $res_status"
echo "DRBD resource on $CLIENT_VM is down and unconfigured"

# --- g. Clean up test volume & verify stand hygiene ---------------------------

step g "g. delete test volume and verify stand cleanliness"

kc delete pvc fail-pvc --wait=true --timeout=60s >/dev/null
wait_until 120 "volume $VOL did not delete completely" no_rvs
echo "ReplicatedVolume $VOL deleted cleanly"

# Check file leftovers and loops on all nodes
for n in "${NODES[@]}"; do
    files="$(vm "$n" "ls -A $VOL_DIR $RES_DIR 2>/dev/null || true")"
    if grep -q "$VOL" <<<"$files"; then
        fail "$n: leftover volume files found: $files"
    fi
    loops="$(vm "$n" "losetup -a 2>/dev/null || true")"
    if grep -q "$VOL" <<<"$loops"; then
        fail "$n: leftover loop device found: $loops"
    fi
    h="$(vm_host "$n")"
    if node_is_tainted "$h" "ambar.balookrd.github.io/no-quorum"; then
        fail "$n: leftover no-quorum taint found on $h"
    fi
done

# Check agent container restarts during the test
RESTARTS_AFTER="$(initial_agent_restarts)"
echo "agent container restart counts before: $RESTARTS_BEFORE"
echo "agent container restart counts after:  $RESTARTS_AFTER"
[[ "$RESTARTS_BEFORE" == "$RESTARTS_AFTER" ]] \
    || fail "agent containers crashed/restarted during test: before='$RESTARTS_BEFORE' after='$RESTARTS_AFTER'"

echo
echo "OK: all failure handling e2e tests passed cleanly!"
