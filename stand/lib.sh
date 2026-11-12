#!/usr/bin/env bash
# Shared helpers for the duovol test stand. Source this file.
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
    limactl shell --workdir / "$1" ip -4 -o addr show dev "$NET_IF" \
        | awk '{print $4}' | cut -d/ -f1
}

# vm_host <node>: hostname as DRBD sees it.
vm_host() {
    limactl shell --workdir / "$1" uname -n
}
