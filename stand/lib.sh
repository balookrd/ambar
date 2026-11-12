#!/usr/bin/env bash
# Shared helpers for the sklad test stand. Source this file.
set -euo pipefail

NODES=(dv-1 dv-2 dv-3)
KVER="${KVER:-6.12.116+deb13-arm64}"
# Interface of the network shared between the VMs.
NET_IF="${NET_IF:-eth0}"

# vm <node> <command...>: run a command as root inside the VM.
vm() {
    local node="$1"; shift
    limactl shell --workdir / "$node" sudo bash -c "$*"
}

# vm_put <node> <path>: write stdin to a root-owned file inside the VM.
vm_put() {
    limactl shell --workdir / "$1" sudo tee "$2" >/dev/null
}

# vm_ip <node>: IPv4 address on the shared network.
vm_ip() {
    local ip
    ip="$(limactl shell --workdir / "$1" ip -4 -o addr show dev "$NET_IF" \
        | awk '{print $4}' | cut -d/ -f1 | head -n1)"
    # The address is interpolated into shell command strings by callers:
    # accept nothing but a dotted quad.
    if [[ ! "$ip" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]; then
        echo "vm_ip: $1 has no IPv4 address on $NET_IF (got '$ip')" >&2
        return 1
    fi
    printf '%s\n' "$ip"
}

# vm_host <node>: hostname as DRBD sees it.
vm_host() {
    limactl shell --workdir / "$1" uname -n
}

# kc_node: first node of NODES that Lima reports as Running and whose
# Kubernetes API answers. stdin is closed for the probes so they cannot consume
# the input of the kubectl call that follows.
kc_node() {
    local n status
    for n in "${NODES[@]}"; do
        status="$(limactl list --format '{{.Status}}' "$n" 2>/dev/null </dev/null)" || continue
        [[ "$status" == "Running" ]] || continue
        if limactl shell --workdir / "$n" sudo timeout 5 k3s kubectl get --raw /readyz \
            >/dev/null 2>&1 </dev/null; then
            printf '%s\n' "$n"
            return 0
        fi
    done
    echo "kc: no running node with a reachable API server among: ${NODES[*]}" >&2
    return 1
}

# kc <kubectl args...>: kubectl against the stand cluster, run on the first
# running node so it keeps working while dv-1 is stopped.
kc() {
    local node
    node="$(kc_node)" || return 1
    limactl shell --workdir / "$node" sudo k3s kubectl "$@"
}

# tb <node> <command...>: run a command in the toolbox pod of that node.
# Picks the single Running pod without deletionTimestamp, so it also works
# while a DaemonSet rollout leaves the old pod Terminating.
tb() {
    local node="$1"; shift
    local host pod
    host="$(vm_host "$node")"
    pod="$(kc -n sklad-spike get pod -l app=toolbox \
        --field-selector "spec.nodeName=$host,status.phase=Running" \
        -o go-template='{{range .items}}{{if not .metadata.deletionTimestamp}}{{.metadata.name}}{{"\n"}}{{end}}{{end}}' \
        | head -n1)"
    if [[ -z "$pod" ]]; then
        echo "tb: no running toolbox pod on $node" >&2
        return 1
    fi
    kc -n sklad-spike exec "$pod" -- bash -c "$*"
}
