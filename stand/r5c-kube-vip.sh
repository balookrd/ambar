#!/usr/bin/env bash
# Deploy or inspect kube-vip static pod on NanoPi R5C cluster.
# Usage:
#   stand/r5c-kube-vip.sh [install|status|uninstall]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
[[ -f "$HERE/r5c-lib.sh" ]] && source "$HERE/r5c-lib.sh"

ACTION="${1:-install}"
VIP="${2:-${R5C_VIP:-198.18.1.30}}"
IFACE="${3:-${R5C_VIP_IFACE:-wan0}}"
POD_MANIFEST_DIR="/var/lib/rancher/k3s/agent/pod-manifests"
POD_MANIFEST_FILE="$POD_MANIFEST_DIR/kube-vip.yaml"

generate_manifest() {
    sed -e "s/__VIP_ADDRESS__/$VIP/g" \
        -e "s/__VIP_INTERFACE__/$IFACE/g" \
        "$HERE/kube-vip.yaml.template"
}

case "$ACTION" in
    install)
        echo "=== Deploying kube-vip (VIP: $VIP on $IFACE) ==="
        MANIFEST_CONTENT="$(generate_manifest)"
        for node in "${R5C_NODES[@]}"; do
            echo "Installing static pod manifest on $node..."
            r5c_ssh "$node" "
                install -d -m 0750 '$POD_MANIFEST_DIR'
                cat > '$POD_MANIFEST_FILE' <<'EOF'
$MANIFEST_CONTENT
EOF
                chmod 0640 '$POD_MANIFEST_FILE'
            "
        done
        echo "Waiting for kube-vip pods to start..."
        sleep 5
        kubectl get pods -n kube-system -l app.kubernetes.io/name=kube-vip 2>/dev/null || \
            kubectl get pods -n kube-system | grep kube-vip || true
        echo "Leader Lease status:"
        kubectl get lease -n kube-system plndr-cp-lock 2>/dev/null || true
        ;;
    status)
        echo "=== kube-vip Cluster Status ==="
        echo "Lease plndr-cp-lock in kube-system:"
        kubectl get lease -n kube-system plndr-cp-lock -o wide 2>/dev/null || echo "No lease found"
        echo ""
        echo "Static Pods on nodes:"
        kubectl get pods -n kube-system -o wide | grep kube-vip || true
        echo ""
        echo "Checking ARP on nodes:"
        for node in "${R5C_NODES[@]}"; do
            has_ip="$(r5c_ssh "$node" "ip addr show dev $IFACE | grep -w '$VIP' || true")"
            if [[ -n "$has_ip" ]]; then
                echo "-> $node holds VIP $VIP: $has_ip"
            else
                echo "   $node (standby)"
            fi
        done
        ;;
    uninstall)
        echo "=== Removing kube-vip static pods from all nodes ==="
        for node in "${R5C_NODES[@]}"; do
            echo "Removing $POD_MANIFEST_FILE on $node..."
            r5c_ssh "$node" "rm -f '$POD_MANIFEST_FILE'"
        done
        echo "Deleting lease in kube-system..."
        kubectl delete lease -n kube-system plndr-cp-lock --ignore-not-found
        echo "Done."
        ;;
    *)
        echo "Usage: $0 [install|status|uninstall] [vip] [interface]" >&2
        exit 1
        ;;
esac
