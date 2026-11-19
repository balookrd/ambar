#!/usr/bin/env bash
# Initializes a clean or raw NanoPi R5C node with LVM storage for k3s and ambar.
# Usage:
#   stand/r5c-init-node.sh <node|all|--local>
# Examples:
#   stand/r5c-init-node.sh k3s-1
#   stand/r5c-init-node.sh all
#   stand/r5c-init-node.sh --local  # when executed directly on target node as root
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
[[ -f "$HERE/r5c-lib.sh" ]] && source "$HERE/r5c-lib.sh"

TARGET="${1:-}"
if [[ -z "$TARGET" || "$TARGET" == "-h" || "$TARGET" == "--help" ]]; then
    echo "Usage: $0 <node|all|--local>" >&2
    echo "  node:    target node name (e.g. k3s-1, k3s-2, k3s-3)" >&2
    echo "  all:     initialize all nodes in R5C_NODES sequentially" >&2
    echo "  --local: execute initialization directly on current host" >&2
    exit 1
fi

NVME_DEV="${NVME_DEV:-/dev/nvme0n1}"
VG_NAME="${VG_NAME:-k3s-vg}"
LV_NAME="${LV_NAME:-lv_k3s}"
LV_SIZE="${LV_SIZE:-35G}"
MOUNT_POINT="${MOUNT_POINT:-/var/lib/rancher}"

init_local_node() {
    echo "=== Initializing LVM on $(hostname) (device: $NVME_DEV, VG: $VG_NAME, LV: $LV_NAME ($LV_SIZE)) ==="

    # 1. Stop k3s services if active
    if systemctl is-active --quiet k3s 2>/dev/null; then
        echo "Stopping k3s service..."
        systemctl stop k3s
    fi
    if systemctl is-active --quiet k3s-agent 2>/dev/null; then
        echo "Stopping k3s-agent service..."
        systemctl stop k3s-agent
    fi

    # 2. Unmount target mountpoint if currently mounted
    if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "Unmounting $MOUNT_POINT..."
        umount -f "$MOUNT_POINT" || umount -l "$MOUNT_POINT"
    fi

    # 3. Install lvm2 package if missing
    if ! command -v pvcreate >/dev/null 2>&1 || ! command -v vgcreate >/dev/null 2>&1; then
        echo "Installing lvm2..."
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y --no-install-recommends lvm2
    fi

    # 4. Check NVMe device existence
    if [[ ! -b "$NVME_DEV" ]]; then
        echo "FAIL: Block device $NVME_DEV not found!" >&2
        exit 1
    fi

    # 5. Clean up existing VG/PV on this device if any
    if vgs "$VG_NAME" >/dev/null 2>&1; then
        echo "Removing existing VG $VG_NAME..."
        vgchange -an "$VG_NAME" >/dev/null 2>&1 || true
        vgremove -y -f "$VG_NAME" || true
    fi

    # Remove any existing PVs on $NVME_DEV and its partitions
    for p in "${NVME_DEV}"p* "${NVME_DEV}"[0-9]* "$NVME_DEV"; do
        if [[ -b "$p" ]]; then
            pvremove -y -f "$p" >/dev/null 2>&1 || true
            wipefs -a -f "$p" >/dev/null 2>&1 || true
        fi
    done
    wipefs -a -f "$NVME_DEV"

    # 6. Initialize PV and VG
    echo "Creating PV on $NVME_DEV..."
    pvcreate -f -y "$NVME_DEV"

    echo "Creating VG $VG_NAME on $NVME_DEV..."
    vgcreate -y "$VG_NAME" "$NVME_DEV"

    # 7. Create linear LV for k3s data
    echo "Creating linear LV $LV_NAME ($LV_SIZE) in VG $VG_NAME..."
    lvcreate -y -L "$LV_SIZE" -n "$LV_NAME" "$VG_NAME"

    # 8. Format filesystem
    echo "Formatting /dev/$VG_NAME/$LV_NAME as ext4..."
    mkfs.ext4 -F -L k3s-data "/dev/$VG_NAME/$LV_NAME"

    # 9. Configure /etc/fstab
    echo "Configuring /etc/fstab for $MOUNT_POINT..."
    mkdir -p "$MOUNT_POINT"
    # Remove any previous entries for $MOUNT_POINT
    sed -i "\|[[:space:]]${MOUNT_POINT}[[:space:]]|d" /etc/fstab
    echo "/dev/$VG_NAME/$LV_NAME $MOUNT_POINT ext4 defaults,noatime,nofail,x-systemd.device-timeout=30s 0 2" >> /etc/fstab

    # 10. Mount filesystem and verify
    echo "Mounting $MOUNT_POINT..."
    mount "$MOUNT_POINT"
    if ! mountpoint -q "$MOUNT_POINT"; then
        echo "FAIL: Failed to mount $MOUNT_POINT after configuration!" >&2
        exit 1
    fi

    # 11. Create required directories
    echo "Creating required k3s and ambar directory layout..."
    mkdir -p "$MOUNT_POINT/k3s" /var/lib/kubelet
    install -d -m 0700 "$MOUNT_POINT/ambar/volumes"
    install -d -m 0755 "$MOUNT_POINT/ambar/res"

    # 12. Display summary
    echo "=== Summary for $(hostname) ==="
    vgs "$VG_NAME"
    lvs "/dev/$VG_NAME/$LV_NAME"
    df -h "$MOUNT_POINT"
    echo "Node $(hostname) initialized successfully."
}

init_remote_node() {
    local node="$1"
    echo "=== Initializing remote node $node ==="
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${node}" "sudo NVME_DEV='$NVME_DEV' VG_NAME='$VG_NAME' LV_NAME='$LV_NAME' LV_SIZE='$LV_SIZE' MOUNT_POINT='$MOUNT_POINT' bash -s" << 'REMOTE_SCRIPT'
set -euo pipefail

echo "=== Initializing LVM on $(hostname) (device: $NVME_DEV, VG: $VG_NAME, LV: $LV_NAME ($LV_SIZE)) ==="

# 1. Stop k3s services if active
if systemctl is-active --quiet k3s 2>/dev/null; then
    echo "Stopping k3s service..."
    systemctl stop k3s
fi
if systemctl is-active --quiet k3s-agent 2>/dev/null; then
    echo "Stopping k3s-agent service..."
    systemctl stop k3s-agent
fi

# 2. Unmount target mountpoint if currently mounted
if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
    echo "Unmounting $MOUNT_POINT..."
    umount -f "$MOUNT_POINT" || umount -l "$MOUNT_POINT"
fi

# 3. Install lvm2 package if missing
if ! command -v pvcreate >/dev/null 2>&1 || ! command -v vgcreate >/dev/null 2>&1; then
    echo "Installing lvm2..."
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y --no-install-recommends lvm2
fi

# 4. Check NVMe device existence
if [[ ! -b "$NVME_DEV" ]]; then
    echo "FAIL: Block device $NVME_DEV not found!" >&2
    exit 1
fi

# 5. Clean up existing VG/PV on this device if any
if vgs "$VG_NAME" >/dev/null 2>&1; then
    echo "Removing existing VG $VG_NAME..."
    vgchange -an "$VG_NAME" >/dev/null 2>&1 || true
    vgremove -y -f "$VG_NAME" || true
fi

# Remove any existing PVs on $NVME_DEV and its partitions
for p in "${NVME_DEV}"p* "${NVME_DEV}"[0-9]* "$NVME_DEV"; do
    if [[ -b "$p" ]]; then
        pvremove -y -f "$p" >/dev/null 2>&1 || true
        wipefs -a -f "$p" >/dev/null 2>&1 || true
    fi
done
wipefs -a -f "$NVME_DEV"

# 6. Initialize PV and VG
echo "Creating PV on $NVME_DEV..."
pvcreate -f -y "$NVME_DEV"

echo "Creating VG $VG_NAME on $NVME_DEV..."
vgcreate -y "$VG_NAME" "$NVME_DEV"

# 7. Create linear LV for k3s data
echo "Creating linear LV $LV_NAME ($LV_SIZE) in VG $VG_NAME..."
lvcreate -y -L "$LV_SIZE" -n "$LV_NAME" "$VG_NAME"

# 8. Format filesystem
echo "Formatting /dev/$VG_NAME/$LV_NAME as ext4..."
mkfs.ext4 -F -L k3s-data "/dev/$VG_NAME/$LV_NAME"

# 9. Configure /etc/fstab
echo "Configuring /etc/fstab for $MOUNT_POINT..."
mkdir -p "$MOUNT_POINT"
# Remove any previous entries for $MOUNT_POINT
sed -i "\|[[:space:]]${MOUNT_POINT}[[:space:]]|d" /etc/fstab
echo "/dev/$VG_NAME/$LV_NAME $MOUNT_POINT ext4 defaults,noatime,nofail,x-systemd.device-timeout=30s 0 2" >> /etc/fstab

# 10. Mount filesystem and verify
echo "Mounting $MOUNT_POINT..."
mount "$MOUNT_POINT"
if ! mountpoint -q "$MOUNT_POINT"; then
    echo "FAIL: Failed to mount $MOUNT_POINT after configuration!" >&2
    exit 1
fi

# 11. Create required directories
echo "Creating required k3s and ambar directory layout..."
mkdir -p "$MOUNT_POINT/k3s" /var/lib/kubelet
install -d -m 0700 "$MOUNT_POINT/ambar/volumes"
install -d -m 0755 "$MOUNT_POINT/ambar/res"

# 12. Display summary
echo "=== Summary for $(hostname) ==="
vgs "$VG_NAME"
lvs "/dev/$VG_NAME/$LV_NAME"
df -h "$MOUNT_POINT"
echo "Node $(hostname) initialized successfully."
REMOTE_SCRIPT
}

if [[ "$TARGET" == "--local" ]]; then
    if [[ $EUID -ne 0 ]]; then
        echo "ERROR: --local execution requires root privileges (run with sudo)" >&2
        exit 1
    fi
    init_local_node
elif [[ "$TARGET" == "all" ]]; then
    for n in "${R5C_NODES[@]}"; do
        init_remote_node "$n"
    done
else
    init_remote_node "$TARGET"
fi
