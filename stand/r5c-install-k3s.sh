#!/usr/bin/env bash
# Installs k3s server on a prepared NanoPi R5C node with LVM partition.
# Usage:
#   stand/r5c-install-k3s.sh <node> [first_ip token]
# Examples:
#   # Bootstrap first node of cluster:
#   stand/r5c-install-k3s.sh k3s-1
#
#   # Join subsequent nodes (token auto-retrieved from first node if not provided):
#   stand/r5c-install-k3s.sh k3s-2
#   stand/r5c-install-k3s.sh k3s-3 10.10.10.51 <token>
#
#   # Direct local execution on node:
#   stand/r5c-install-k3s.sh --local [first_ip token]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
[[ -f "$HERE/r5c-lib.sh" ]] && source "$HERE/r5c-lib.sh"

TARGET="${1:-}"
FIRST_IP="${2:-}"
TOKEN="${3:-}"

if [[ -z "$TARGET" || "$TARGET" == "-h" || "$TARGET" == "--help" ]]; then
    echo "Usage: $0 <node|--local> [first_ip token]" >&2
    echo "  node:     target node name (e.g. k3s-1, k3s-2, k3s-3)" >&2
    echo "  first_ip: IP of cluster seed node on lan0 (optional for first node or auto-discovered)" >&2
    echo "  token:    cluster join token (optional for first node or auto-discovered from first node)" >&2
    exit 1
fi

K3S_VERSION="${K3S_VERSION:-v1.36.2+k3s1}"
FIRST_NODE="${R5C_NODES[0]:-k3s-1}"
DEFAULT_FIRST_IP="${R5C_IPS[0]:-10.10.10.51}"

get_node_ip() {
    local node="$1"
    for i in "${!R5C_NODES[@]}"; do
        if [[ "${R5C_NODES[$i]}" == "$node" ]]; then
            echo "${R5C_IPS[$i]}"
            return 0
        fi
    done
    # Fallback to querying lan0 directly on target
    r5c_ssh "$node" "ip -4 addr show lan0 | awk '/inet / {print \$2}' | cut -d/ -f1 | head -n 1"
}

install_k3s_remote() {
    local node="$1"
    local first_ip="$2"
    local token="$3"

    echo "=== 1. Checking prerequisites on $node ==="
    # Check that /var/lib/rancher is a mountpoint
    if ! r5c_ssh "$node" "mountpoint -q /var/lib/rancher"; then
        echo "FAIL: /var/lib/rancher is not a mount point on $node!" >&2
        echo "Please run 'stand/r5c-init-node.sh $node' before installing k3s." >&2
        exit 1
    fi

    local mount_source
    mount_source="$(r5c_ssh "$node" "findmnt -n -o SOURCE /var/lib/rancher || true")"
    echo "Node $node: /var/lib/rancher is mounted from $mount_source"

    local node_ip
    node_ip="$(get_node_ip "$node")"
    if [[ -z "$node_ip" ]]; then
        echo "FAIL: Could not determine lan0 IP address for node $node!" >&2
        exit 1
    fi
    echo "Node $node: interconnect IP on lan0 is $node_ip"

    # Blacklist kernel drbd module if needed and ensure curl
    r5c_ssh "$node" "
        printf 'blacklist drbd\ninstall drbd /bin/false\n' > /etc/modprobe.d/ambar-blacklist-drbd.conf
        command -v curl >/dev/null 2>&1 || {
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq && apt-get install -y --no-install-recommends curl
        }
    "

    echo "=== 2. Installing k3s ($K3S_VERSION) on $node ==="
    if [[ -z "$first_ip" && "$node" == "$FIRST_NODE" ]]; then
        echo "Bootstrapping first cluster node with --cluster-init..."
        r5c_ssh "$node" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' sh -s - server \
            --cluster-init \
            --node-ip '$node_ip' \
            --disable=traefik \
            --disable=servicelb"
    else
        first_ip="${first_ip:-$DEFAULT_FIRST_IP}"
        if [[ -z "$token" ]]; then
            echo "Fetching join token from seed node $FIRST_NODE..."
            token="$(r5c_ssh "$FIRST_NODE" "cat /var/lib/rancher/k3s/server/node-token 2>/dev/null || true")"
            if [[ -z "$token" ]]; then
                echo "FAIL: Could not automatically fetch node-token from $FIRST_NODE. Provide it as 3rd argument." >&2
                exit 1
            fi
        fi
        echo "Joining $node to cluster https://${first_ip}:6443..."
        r5c_ssh "$node" "curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION='$K3S_VERSION' K3S_TOKEN='$token' sh -s - server \
            --server 'https://${first_ip}:6443' \
            --node-ip '$node_ip' \
            --disable=traefik \
            --disable=servicelb"
    fi

    echo "=== 3. Verifying k3s startup on $node ==="
    local active=0
    for _ in $(seq 30); do
        if r5c_ssh "$node" "systemctl is-active --quiet k3s"; then
            active=1
            break
        fi
        sleep 2
    done

    if (( active == 0 )); then
        echo "FAIL: k3s service failed to become active on $node within 60s" >&2
        r5c_ssh "$node" "systemctl status k3s --no-pager || true" >&2
        exit 1
    fi

    # Ensure ambar host paths exist with expected permissions
    r5c_ssh "$node" "
        install -d -m 0700 /var/lib/rancher/ambar/volumes
        install -d -m 0755 /var/lib/rancher/ambar/res
    "

    if [[ "$node" == "$FIRST_NODE" ]]; then
        local cluster_token
        cluster_token="$(r5c_ssh "$node" "cat /var/lib/rancher/k3s/server/node-token")"
        echo "Seed node $node initialized. Cluster token: $cluster_token"
    fi

    echo "Node status on $node:"
    r5c_ssh "$node" "k3s kubectl get nodes -o wide || true"
    echo "OK: k3s installed and verified on $node."
}

install_k3s_local() {
    local first_ip="$1"
    local token="$2"

    echo "=== 1. Checking prerequisites on $(hostname) ==="
    if ! mountpoint -q /var/lib/rancher; then
        echo "FAIL: /var/lib/rancher is not a mount point!" >&2
        echo "Please run stand/r5c-init-node.sh --local first." >&2
        exit 1
    fi

    local node_ip
    node_ip="$(ip -4 addr show lan0 2>/dev/null | awk '/inet / {print $2}' | cut -d/ -f1 | head -n 1)"
    if [[ -z "$node_ip" ]]; then
        echo "FAIL: Could not determine lan0 IP address!" >&2
        exit 1
    fi
    echo "Interconnect IP on lan0: $node_ip"

    printf 'blacklist drbd\ninstall drbd /bin/false\n' > /etc/modprobe.d/ambar-blacklist-drbd.conf
    command -v curl >/dev/null 2>&1 || {
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq && apt-get install -y --no-install-recommends curl
    }

    echo "=== 2. Installing k3s ($K3S_VERSION) ==="
    if [[ -z "$first_ip" ]]; then
        echo "Bootstrapping seed node with --cluster-init..."
        curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION="$K3S_VERSION" sh -s - server \
            --cluster-init \
            --node-ip "$node_ip" \
            --disable=traefik \
            --disable=servicelb
    else
        if [[ -z "$token" ]]; then
            echo "FAIL: Join token must be provided for joining nodes!" >&2
            exit 1
        fi
        echo "Joining to cluster https://${first_ip}:6443..."
        curl -sfL https://get.k3s.io | INSTALL_K3S_VERSION="$K3S_VERSION" K3S_TOKEN="$token" sh -s - server \
            --server "https://${first_ip}:6443" \
            --node-ip "$node_ip" \
            --disable=traefik \
            --disable=servicelb
    fi

    echo "=== 3. Verifying k3s startup ==="
    local active=0
    for _ in $(seq 30); do
        if systemctl is-active --quiet k3s; then
            active=1
            break
        fi
        sleep 2
    done
    if (( active == 0 )); then
        echo "FAIL: k3s service failed to become active within 60s" >&2
        systemctl status k3s --no-pager || true
        exit 1
    fi

    install -d -m 0700 /var/lib/rancher/ambar/volumes
    install -d -m 0755 /var/lib/rancher/ambar/res

    k3s kubectl get nodes -o wide || true
    echo "OK: k3s installed and verified locally."
}

if [[ "$TARGET" == "--local" ]]; then
    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: --local execution requires root privileges (run with sudo)" >&2
        exit 1
    fi
    install_k3s_local "$FIRST_IP" "$TOKEN"
else
    install_k3s_remote "$TARGET" "$FIRST_IP" "$TOKEN"
fi
