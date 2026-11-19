#!/usr/bin/env bash
# Migrates a NanoPi R5C cluster node storage from a raw partition to LVM (k3s-vg/lv_k3s).
# Usage:
#   stand/r5c-migrate-lvm.sh <node> [--yes|-y] [--force|-f]
#   stand/r5c-migrate-lvm.sh --local [--yes|-y] [--force|-f]
# Examples:
#   stand/r5c-migrate-lvm.sh k3s-3
#   stand/r5c-migrate-lvm.sh k3s-2 -y
#   stand/r5c-migrate-lvm.sh --local
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
[[ -f "$HERE/r5c-lib.sh" ]] && source "$HERE/r5c-lib.sh"

NVME_DEV="${NVME_DEV:-/dev/nvme0n1}"
VG_NAME="${VG_NAME:-k3s-vg}"
LV_NAME="${LV_NAME:-lv_k3s}"
LV_SIZE="${LV_SIZE:-35G}"
MOUNT_POINT="${MOUNT_POINT:-/var/lib/rancher}"
BACKUP_DIR="${BACKUP_DIR:-/root}"
K3S_TIMEOUT="${K3S_TIMEOUT:-180s}"

show_help() {
    cat << EOF
Usage: $0 <node|--local> [options]

Safely migrates a k3s cluster node to LVM layout with backup & restore:
  1. Verifies cluster quorum and health (etcd readiness, all nodes Ready).
  2. Cordons the target node.
  3. Drains workload pods from the node.
  4. Stops k3s service.
  5. Creates full sparse compressed backup on root eMMC (${BACKUP_DIR}/k3s-backup-<node>.tar.gz).
  6. Verifies archive integrity with tar -tzf.
  7. Unmounts ${MOUNT_POINT}.
  8. Initializes LVM (PV on ${NVME_DEV}, VG ${VG_NAME}, LV ${LV_NAME} ${LV_SIZE}, ext4).
  9. Updates /etc/fstab and mounts ${MOUNT_POINT}.
 10. Restores data from archive and starts k3s.
 11. Waits for node Ready condition and uncordons node.

Arguments:
  node          Name of node to migrate (e.g. k3s-1, k3s-2, k3s-3)
  --local       Run migration directly on local host as root

Options:
  -y, --yes     Automatic yes to prompts (run non-interactively)
  -f, --force   Force migration even if already on LVM
  -h, --help    Show this help message
EOF
}

TARGET=""
ASSUME_YES=0
FORCE_MIGRATE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        -y|--yes)
            ASSUME_YES=1
            shift
            ;;
        -f|--force)
            FORCE_MIGRATE=1
            shift
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        --local)
            TARGET="--local"
            shift
            ;;
        *)
            if [[ -z "$TARGET" ]]; then
                TARGET="$1"
            else
                echo "ERROR: Unknown argument '$1'" >&2
                show_help >&2
                exit 1
            fi
            shift
            ;;
    esac
done

if [[ -z "$TARGET" ]]; then
    echo "ERROR: Target node must be specified." >&2
    show_help >&2
    exit 1
fi

IS_LOCAL=0
if [[ "$TARGET" == "--local" ]]; then
    IS_LOCAL=1
    NODE="$(hostname)"
    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: --local execution requires root privileges (run with sudo)" >&2
        exit 1
    fi
else
    NODE="$TARGET"
fi

BACKUP_FILE="${BACKUP_DIR}/k3s-backup-${NODE}.tar"

# Resilient cluster kubectl runner: tries local kubectl first, falls back to surviving peer node via SSH
cluster_kubectl() {
    if [[ "$IS_LOCAL" -eq 1 ]]; then
        if command -v k3s >/dev/null 2>&1 && systemctl is-active --quiet k3s 2>/dev/null; then
            k3s kubectl "$@"
            return $?
        elif command -v kubectl >/dev/null 2>&1 && kubectl version --request-timeout=3s >/dev/null 2>&1; then
            kubectl "$@"
            return $?
        fi
        # If local k3s is stopped during migration, query surviving peers
        for peer in "${R5C_NODES[@]}"; do
            if [[ "$peer" != "$NODE" ]]; then
                local remote_cmd="k3s kubectl"
                for arg in "$@"; do
                    remote_cmd+=" $(printf '%q' "$arg")"
                done
                if r5c_ssh "$peer" "$remote_cmd" 2>/dev/null; then
                    return 0
                fi
            fi
        done
        # Fallback to direct local kubectl invocation
        kubectl "$@"
    else
        # Remote execution mode: try local kubectl first
        if command -v kubectl >/dev/null 2>&1 && kubectl version --request-timeout=3s >/dev/null 2>&1; then
            kubectl "$@"
            return $?
        fi

        # Fallback to surviving peer nodes via SSH
        for peer in "${R5C_NODES[@]}"; do
            if [[ "$peer" != "$NODE" ]]; then
                local remote_cmd="k3s kubectl"
                for arg in "$@"; do
                    remote_cmd+=" $(printf '%q' "$arg")"
                done
                if r5c_ssh "$peer" "$remote_cmd" 2>/dev/null; then
                    return 0
                fi
            fi
        done

        # Final attempt via local kubectl
        kubectl "$@"
    fi
}

run_on_node() {
    local cmd="$1"
    if [[ "$IS_LOCAL" -eq 1 ]]; then
        bash -c "$cmd"
    else
        r5c_ssh "$NODE" "$cmd" < /dev/null
    fi
}

echo "======================================================================"
echo " Starting LVM Migration Orchestrator for node: $NODE"
echo " Target block device:  $NVME_DEV"
echo " Volume group:         $VG_NAME"
echo " Logical volume:       $LV_NAME ($LV_SIZE)"
echo " Mount point:          $MOUNT_POINT"
echo " Backup destination:   $BACKUP_FILE"
echo " Execution mode:       $([[ $IS_LOCAL -eq 1 ]] && echo 'LOCAL' || echo 'REMOTE via SSH')"
echo "======================================================================"

# ==============================================================================
# Phase 1: Preflight & Safety Checks
# ==============================================================================
echo "=== Step 1/8: Preflight checks & cluster quorum verification ==="

# 1.1 Node connectivity check
if [[ "$IS_LOCAL" -eq 0 ]]; then
    echo "Checking SSH connectivity to $NODE..."
    if ! r5c_ssh "$NODE" "true" 2>/dev/null; then
        echo "FAIL: Cannot connect to node $NODE via SSH!" >&2
        exit 1
    fi
    echo "SSH connectivity to $NODE OK."
fi

# 1.2 Cluster readiness and etcd check
echo "Checking Kubernetes API readiness (/readyz)..."
readyz="$(cluster_kubectl get --raw=/readyz 2>/dev/null || echo 'failed')"
if [[ "$readyz" != *"ok"* ]]; then
    echo "FAIL: Cluster API readyz check failed (status: $readyz)!" >&2
    echo "Cluster must be healthy before performing node migration." >&2
    exit 1
fi
echo "Kubernetes API readyz: OK."

# 1.3 Check status of peer nodes in cluster
echo "Checking cluster peer nodes condition..."
not_ready_nodes="$(cluster_kubectl get nodes --no-headers 2>/dev/null | awk -v node="$NODE" '$1 != node && $2 !~ /^Ready/ {print $1}' || true)"
if [[ -n "$not_ready_nodes" ]]; then
    echo "FAIL: The following peer nodes are NotReady:" >&2
    echo "$not_ready_nodes" >&2
    echo "Cannot safely migrate $NODE while other nodes are unhealthy." >&2
    exit 1
fi
ready_peers="$(cluster_kubectl get nodes --no-headers 2>/dev/null | awk -v node="$NODE" '$1 != node && $2 ~ /^Ready/ {print $1}' || true)"
if [[ -z "$ready_peers" ]]; then
    echo "FAIL: No healthy peer nodes available in cluster!" >&2
    exit 1
fi
echo "All cluster peer nodes are Ready."

# 1.4 Check target NVMe block device
echo "Checking block device $NVME_DEV on $NODE..."
if ! run_on_node "test -b '$NVME_DEV'"; then
    echo "FAIL: Block device $NVME_DEV not found on node $NODE!" >&2
    exit 1
fi
echo "Block device $NVME_DEV exists."

# 1.5 Check current mount status of /var/lib/rancher
if run_on_node "mountpoint -q '$MOUNT_POINT'"; then
    current_source="$(run_on_node "findmnt -n -o SOURCE '$MOUNT_POINT' || true")"
    echo "Node $NODE: $MOUNT_POINT is currently mounted from $current_source"
    if [[ "$current_source" == *"$VG_NAME"* || "$current_source" == *"lv_k3s"* ]]; then
        if [[ "$FORCE_MIGRATE" -eq 0 ]]; then
            echo "Node $NODE: $MOUNT_POINT is ALREADY on LVM ($current_source)!"
            echo "Migration already completed. Use --force to re-initialize."
            exit 0
        else
            echo "WARNING: --force specified, re-initializing existing LVM layout..."
        fi
    fi
else
    echo "WARNING: $MOUNT_POINT is not currently a mountpoint on $NODE."
fi

# 1.6 Verify free space on eMMC root fs for backup
echo "Checking free space on root filesystem for backup..."
root_avail_kb="$(run_on_node "df -k '$BACKUP_DIR' | awk 'NR==2 {print \$4}'")"
rancher_used_kb="$(run_on_node "du -sk '$MOUNT_POINT' 2>/dev/null | awk '{print \$1}' || df -k '$MOUNT_POINT' | awk 'NR==2 {print \$3}'")"
echo "Root filesystem available: $(( root_avail_kb / 1024 )) MiB, $MOUNT_POINT used: $(( rancher_used_kb / 1024 )) MiB"

if run_on_node "test -s '$BACKUP_FILE'"; then
    echo "Existing backup archive found at $BACKUP_FILE, will reuse if verified."
else
    # Require at least used space + 2 GiB safety margin, with absolute minimum of 8 GiB
    needed_backup_kb=$(( rancher_used_kb + 2097152 ))
    if (( needed_backup_kb < 8388608 )); then
        needed_backup_kb=8388608
    fi
    if (( root_avail_kb < needed_backup_kb )); then
        echo "FAIL: Insufficient free space on $BACKUP_DIR: $(( root_avail_kb / 1024 )) MiB available, need >= $(( needed_backup_kb / 1024 )) MiB!" >&2
        exit 1
    fi
    echo "Sufficient free space available on root eMMC for backup ($(( root_avail_kb / 1024 )) MiB >= $(( needed_backup_kb / 1024 )) MiB)."
fi

# Verify that existing data fits within the target LV size
lv_size_unit="${LV_SIZE: -1}"
lv_size_num="${LV_SIZE%[a-zA-Z]*}"
case "$lv_size_unit" in
    g|G) lv_size_kb=$(( lv_size_num * 1024 * 1024 )) ;;
    m|M) lv_size_kb=$(( lv_size_num * 1024 )) ;;
    t|T) lv_size_kb=$(( lv_size_num * 1024 * 1024 * 1024 )) ;;
    *)   lv_size_kb=$(( lv_size_num / 1024 )) ;;
esac

if (( rancher_used_kb > lv_size_kb * 85 / 100 )); then
    echo "FAIL: Existing data in $MOUNT_POINT ($(( rancher_used_kb / 1024 )) MiB) exceeds 85% of target LV size $LV_SIZE ($(( lv_size_kb / 1024 )) MiB)!" >&2
    exit 1
fi
echo "Target LV size $LV_SIZE is sufficient for existing data ($(( rancher_used_kb / 1024 )) MiB / $(( lv_size_kb / 1024 )) MiB)."

# 1.7 Ensure lvm2 package is installed on target node
echo "Checking lvm2 tools on $NODE..."
run_on_node "
    if ! command -v pvcreate >/dev/null 2>&1 || ! command -v vgcreate >/dev/null 2>&1; then
        echo 'Installing lvm2 on node...'
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq && apt-get install -y --no-install-recommends lvm2
    fi
"
echo "lvm2 tools verified."

# 1.8 Confirmation prompt (if not -y)
if [[ "$ASSUME_YES" -eq 0 ]]; then
    echo "----------------------------------------------------------------------"
    echo " ATTENTION: This operation will repartition $NVME_DEV on node '$NODE'!"
    echo " Data in $MOUNT_POINT will be backed up to $BACKUP_FILE,"
    echo " the drive will be wiped and converted to LVM ($VG_NAME/$LV_NAME),"
    echo " and data will be restored before restarting k3s."
    echo "----------------------------------------------------------------------"
    confirm=""
    if ! read -r -p "Are you sure you want to proceed with migration of '$NODE'? [y/N]: " confirm || [[ ! "$confirm" =~ ^[Yy]$ ]]; then
        echo "Migration cancelled by user."
        exit 0
    fi
fi

# ==============================================================================
# Phase 2: Cordon Node
# ==============================================================================
echo "=== Step 2/8: Cordon node $NODE ==="
cluster_kubectl cordon "$NODE"
echo "Node $NODE cordoned."

# ==============================================================================
# Phase 3: Drain Node
# ==============================================================================
echo "=== Step 3/8: Drain node $NODE ==="
cluster_kubectl drain "$NODE" \
    --ignore-daemonsets \
    --delete-emptydir-data \
    --force \
    --timeout=180s
echo "Node $NODE drained successfully."

# ==============================================================================
# Phase 4: Stop k3s and Create Backup on eMMC
# ==============================================================================
echo "=== Step 4/8: Stop k3s and backup $MOUNT_POINT to eMMC ==="
if [[ "$IS_LOCAL" -eq 1 ]]; then
    BACKUP_FILE="$BACKUP_FILE" MOUNT_POINT="$MOUNT_POINT" bash -s << 'LOCAL_BACKUP_SCRIPT'
set -euo pipefail
echo "Stopping k3s services..."
systemctl stop k3s 2>/dev/null || true
systemctl stop k3s-agent 2>/dev/null || true

for i in $(seq 1 30); do
    if ! systemctl is-active --quiet k3s && ! systemctl is-active --quiet k3s-agent; then
        break
    fi
    sleep 1
done

if [[ -s "$BACKUP_FILE" ]] && tar -tf "$BACKUP_FILE" >/dev/null 2>&1; then
    echo "Existing valid backup archive verified at $BACKUP_FILE, reusing."
else
    if ! mountpoint -q "$MOUNT_POINT"; then
        echo "FAIL: $MOUNT_POINT is not mounted, cannot create fresh backup!" >&2
        exit 1
    fi
    echo "Creating sparse backup: $BACKUP_FILE..."
    tar --sparse --numeric-owner -cpf "$BACKUP_FILE" "$MOUNT_POINT"

    echo "Verifying backup archive with tar -tf..."
    if ! tar -tf "$BACKUP_FILE" >/dev/null; then
        echo "FAIL: Backup verification failed!" >&2
        exit 1
    fi
fi
if [[ ! -s "$BACKUP_FILE" ]]; then
    echo "FAIL: Backup file is empty!" >&2
    exit 1
fi
size=$(ls -lh "$BACKUP_FILE" | awk '{print $5}')
echo "Backup verified successfully (size: $size)."
LOCAL_BACKUP_SCRIPT
else
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${NODE}" "sudo BACKUP_FILE='$BACKUP_FILE' MOUNT_POINT='$MOUNT_POINT' bash -s" << 'REMOTE_BACKUP_SCRIPT'
set -euo pipefail
echo "Stopping k3s services..."
systemctl stop k3s 2>/dev/null || true
systemctl stop k3s-agent 2>/dev/null || true

for i in $(seq 1 30); do
    if ! systemctl is-active --quiet k3s && ! systemctl is-active --quiet k3s-agent; then
        break
    fi
    sleep 1
done

if [[ -s "$BACKUP_FILE" ]] && tar -tf "$BACKUP_FILE" >/dev/null 2>&1; then
    echo "Existing valid backup archive verified at $BACKUP_FILE, reusing."
else
    if ! mountpoint -q "$MOUNT_POINT"; then
        echo "FAIL: $MOUNT_POINT is not mounted, cannot create fresh backup!" >&2
        exit 1
    fi
    echo "Creating sparse backup: $BACKUP_FILE..."
    tar --sparse --numeric-owner -cpf "$BACKUP_FILE" "$MOUNT_POINT"

    echo "Verifying backup archive with tar -tf..."
    if ! tar -tf "$BACKUP_FILE" >/dev/null; then
        echo "FAIL: Backup verification failed!" >&2
        exit 1
    fi
fi
if [[ ! -s "$BACKUP_FILE" ]]; then
    echo "FAIL: Backup file is empty!" >&2
    exit 1
fi
size=$(ls -lh "$BACKUP_FILE" | awk '{print $5}')
echo "Backup verified successfully (size: $size)."
REMOTE_BACKUP_SCRIPT
fi

# ==============================================================================
# Phase 5: Unmount /var/lib/rancher
# ==============================================================================
echo "=== Step 5/8: Unmount $MOUNT_POINT ==="
run_on_node "
    set -euo pipefail

    if [[ -x /usr/local/bin/k3s-killall.sh ]]; then
        echo 'Executing k3s-killall.sh to cleanup containerd and mounts...'
        /usr/local/bin/k3s-killall.sh >/dev/null 2>&1 || true
    fi

    while read -r _ path _; do
        case \"\$path\" in /run/k3s/containerd/*) umount -l \"\$path\" 2>/dev/null || true ;; esac
        case \"\$path\" in /var/lib/kubelet/*) umount -l \"\$path\" 2>/dev/null || true ;; esac
    done < /proc/self/mounts

    if mountpoint -q '$MOUNT_POINT'; then
        echo 'Unmounting $MOUNT_POINT...'
        sync
        if ! umount '$MOUNT_POINT'; then
            echo 'Warning: initial umount failed, terminating active processes holding $MOUNT_POINT...'
            fuser -km -15 '$MOUNT_POINT' 2>/dev/null || true
            sleep 2
            if ! umount '$MOUNT_POINT'; then
                fuser -km -9 '$MOUNT_POINT' 2>/dev/null || true
                sleep 2
                umount '$MOUNT_POINT' || {
                    echo 'FAIL: Cannot unmount $MOUNT_POINT cleanly! Active openers:' >&2
                    lsof +f -- '$MOUNT_POINT' 2>/dev/null || fuser -v -m '$MOUNT_POINT' 2>/dev/null || true
                    exit 1
                }
            fi
        fi
    fi
    if mountpoint -q '$MOUNT_POINT'; then
        echo 'FAIL: Failed to unmount $MOUNT_POINT!' >&2
        exit 1
    fi
    echo '$MOUNT_POINT successfully unmounted.'
"

# ==============================================================================
# Phase 6: Initialize LVM (PV, VG, LV, ext4)
# ==============================================================================
echo "=== Step 6/8: Initialize LVM on $NVME_DEV (VG: $VG_NAME, LV: $LV_NAME ($LV_SIZE)) ==="
if [[ "$IS_LOCAL" -eq 1 ]]; then
    NVME_DEV="$NVME_DEV" VG_NAME="$VG_NAME" LV_NAME="$LV_NAME" LV_SIZE="$LV_SIZE" bash -s << 'LOCAL_LVM_SCRIPT'
set -euo pipefail

if findmnt -n --source "$NVME_DEV" >/dev/null 2>&1; then
    echo "FAIL: Block device $NVME_DEV or its partitions are still mounted!" >&2
    findmnt --source "$NVME_DEV" >&2 || true
    exit 1
fi

if vgs "$VG_NAME" >/dev/null 2>&1; then
    echo "Removing existing VG $VG_NAME..."
    vgchange -an "$VG_NAME" >/dev/null 2>&1 || true
    vgremove -y -f "$VG_NAME" || true
fi

for p in "${NVME_DEV}"p* "${NVME_DEV}"[0-9]* "$NVME_DEV"; do
    if [[ -b "$p" ]]; then
        pvremove -y -f "$p" >/dev/null 2>&1 || true
        wipefs -a -f "$p" >/dev/null 2>&1 || true
    fi
done
wipefs -a -f "$NVME_DEV"
partx -d "$NVME_DEV" >/dev/null 2>&1 || true
blockdev --rereadpt "$NVME_DEV" >/dev/null 2>&1 || true
udevadm settle 2>/dev/null || true
sleep 1

echo "Creating Physical Volume on $NVME_DEV..."
pvcreate -f -y "$NVME_DEV"

echo "Creating Volume Group $VG_NAME on $NVME_DEV..."
vgcreate -y "$VG_NAME" "$NVME_DEV"

echo "Creating Logical Volume $LV_NAME ($LV_SIZE) in $VG_NAME..."
lvcreate -y -L "$LV_SIZE" -n "$LV_NAME" "$VG_NAME"

echo "Formatting /dev/$VG_NAME/$LV_NAME as ext4 (label: k3s-data)..."
mkfs.ext4 -F -L k3s-data "/dev/$VG_NAME/$LV_NAME"
echo "LVM partition layout initialized."
LOCAL_LVM_SCRIPT
else
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${NODE}" "sudo NVME_DEV='$NVME_DEV' VG_NAME='$VG_NAME' LV_NAME='$LV_NAME' LV_SIZE='$LV_SIZE' bash -s" << 'REMOTE_LVM_SCRIPT'
set -euo pipefail

if findmnt -n --source "$NVME_DEV" >/dev/null 2>&1; then
    echo "FAIL: Block device $NVME_DEV or its partitions are still mounted!" >&2
    findmnt --source "$NVME_DEV" >&2 || true
    exit 1
fi

if vgs "$VG_NAME" >/dev/null 2>&1; then
    echo "Removing existing VG $VG_NAME..."
    vgchange -an "$VG_NAME" >/dev/null 2>&1 || true
    vgremove -y -f "$VG_NAME" || true
fi

for p in "${NVME_DEV}"p* "${NVME_DEV}"[0-9]* "$NVME_DEV"; do
    if [[ -b "$p" ]]; then
        pvremove -y -f "$p" >/dev/null 2>&1 || true
        wipefs -a -f "$p" >/dev/null 2>&1 || true
    fi
done
wipefs -a -f "$NVME_DEV"
partx -d "$NVME_DEV" >/dev/null 2>&1 || true
blockdev --rereadpt "$NVME_DEV" >/dev/null 2>&1 || true
udevadm settle 2>/dev/null || true
sleep 1

echo "Creating Physical Volume on $NVME_DEV..."
pvcreate -f -y "$NVME_DEV"

echo "Creating Volume Group $VG_NAME on $NVME_DEV..."
vgcreate -y "$VG_NAME" "$NVME_DEV"

echo "Creating Logical Volume $LV_NAME ($LV_SIZE) in $VG_NAME..."
lvcreate -y -L "$LV_SIZE" -n "$LV_NAME" "$VG_NAME"

echo "Formatting /dev/$VG_NAME/$LV_NAME as ext4 (label: k3s-data)..."
mkfs.ext4 -F -L k3s-data "/dev/$VG_NAME/$LV_NAME"
echo "LVM partition layout initialized."
REMOTE_LVM_SCRIPT
fi

# ==============================================================================
# Phase 7: Update /etc/fstab and Mount
# ==============================================================================
echo "=== Step 7/8: Update /etc/fstab and mount $MOUNT_POINT ==="
run_on_node "
    set -euo pipefail
    mkdir -p '$MOUNT_POINT'

    # Remove any existing entries for $MOUNT_POINT
    sed -i \"\|[[:space:]]${MOUNT_POINT}[[:space:]]|d\" /etc/fstab

    # Add new LVM mount entry
    echo \"/dev/$VG_NAME/$LV_NAME $MOUNT_POINT ext4 defaults,noatime,nofail,x-systemd.device-timeout=30s 0 2\" >> /etc/fstab

    echo 'Mounting $MOUNT_POINT from /dev/$VG_NAME/$LV_NAME...'
    mount '$MOUNT_POINT'

    if ! mountpoint -q '$MOUNT_POINT'; then
        echo 'FAIL: Failed to mount $MOUNT_POINT after configuration!' >&2
        exit 1
    fi
    echo '$MOUNT_POINT mounted successfully.'
"

# ==============================================================================
# Phase 8: Restore Data from Backup & Restart k3s
# ==============================================================================
echo "=== Step 8/8: Restore data from backup and restart k3s ==="
run_on_node "
    set -euo pipefail
    echo 'Restoring data from $BACKUP_FILE to $MOUNT_POINT...'
    tar --same-owner -xpf '$BACKUP_FILE' -C /

    if [[ ! -d '$MOUNT_POINT/k3s' ]]; then
        echo 'FAIL: Expected directory $MOUNT_POINT/k3s missing after restore!' >&2
        exit 1
    fi

    # Ensure required runtime directories exist with proper permissions
    install -d -m 0700 '$MOUNT_POINT/ambar/volumes'
    install -d -m 0755 '$MOUNT_POINT/ambar/res'
    mkdir -p /var/lib/kubelet

    echo 'Starting k3s service...'
    systemctl start k3s

    active=0
    for i in \$(seq 1 45); do
        if systemctl is-active --quiet k3s; then
            active=1
            break
        fi
        sleep 2
    done

    if [[ \$active -eq 0 ]]; then
        echo 'FAIL: k3s service failed to become active after restart!' >&2
        systemctl status k3s --no-pager || true
        exit 1
    fi
    echo 'k3s service is active.'
"

# ==============================================================================
# Wait for Node Ready & Uncordon
# ==============================================================================
echo "Waiting for node $NODE to reach Ready condition (timeout: $K3S_TIMEOUT)..."
cluster_kubectl wait --for=condition=Ready "node/$NODE" --timeout="$K3S_TIMEOUT"

echo "Uncordoning node $NODE..."
cluster_kubectl uncordon "$NODE"

# ==============================================================================
# Verification and Summary
# ==============================================================================
echo "======================================================================"
echo " Migration of node '$NODE' to LVM successfully completed!"
echo "======================================================================"
echo "--- LVM Volume Group status on $NODE ---"
run_on_node "vgs '$VG_NAME'"
echo "--- LVM Logical Volume status on $NODE ---"
run_on_node "lvs '/dev/$VG_NAME/$LV_NAME'"
echo "--- Mount status on $NODE ---"
run_on_node "df -h '$MOUNT_POINT'"
echo "--- Backup archive preserved on $NODE ---"
run_on_node "ls -lh '$BACKUP_FILE'"
echo "--- Cluster Nodes Status ---"
cluster_kubectl get nodes -o wide
echo "OK: Node $NODE is Ready and uncordoned."
