#!/usr/bin/env bash
# Library of common functions and variables for NanoPi R5C cluster operations.
set -euo pipefail

R5C_NODES=(k3s-1 k3s-2 k3s-3)
R5C_IPS=("10.10.10.51" "10.10.10.52" "10.10.10.53")
R5C_VIP="${R5C_VIP:-198.18.1.30}"
R5C_VIP_IFACE="${R5C_VIP_IFACE:-wan0}"
R5C_USER="${R5C_USER:-mmv}"
R5C_SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new)
R5C_IMAGE="ambar-drbd:dev"
R5C_BENCH_IMAGE="ambar-fio:local"
R5C_BENCH_NS="ambar-bench"
R5C_SYSTEM_NS="ambar-system"

r5c_ssh() {
    local node="$1"
    shift
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${node}" "sudo bash -c $(printf '%q' "$*")"
}

r5c_ssh_user() {
    local node="$1"
    shift
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${node}" "$@"
}

r5c_k3s_rss_kb() {
    local node="$1"
    r5c_ssh "$node" "ps -o rss= -C k3s-server | awk '{s+=\$1} END {print s+0}'"
}

r5c_pod_anon_bytes() {
    local node="$1"
    local pod_uid="$2"
    local uid_underscore="${pod_uid//-/_}"
    r5c_ssh "$node" "
        path=\$(find /sys/fs/cgroup/kubepods.slice -name \"*${uid_underscore}*\" 2>/dev/null | head -n 1)
        if [[ -n \"\$path\" && -f \"\$path/memory.stat\" ]]; then
            awk '\$1 == \"anon\" {print \$2}' \"\$path/memory.stat\"
        else
            echo 0
        fi
    "
}

r5c_ambar_total_rss_kb() {
    local node="$1"
    # node-driver-reg is truncated to 15 chars due to Linux kernel TASK_COMM_LEN limit for ps -C
    r5c_ssh "$node" "ps -o rss= -C ambar,csi-provisioner,node-driver-reg | awk '{s+=\$1} END {print s+0}'"
}
