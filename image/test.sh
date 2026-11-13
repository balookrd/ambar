#!/usr/bin/env bash
# Verifies the ambar-drbd image without loading the kernel module.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-ambar-drbd:dev}"

run() { docker run --rm --platform linux/arm64 "$IMAGE" bash -c "$1"; }

fail() { echo "FAIL: $1" >&2; exit 1; }

vermagic="$(run "modinfo -F vermagic /opt/ambar/modules/$KVER/drbd.ko")"
[[ "$vermagic" == "$KVER SMP "* ]] || fail "drbd.ko vermagic: $vermagic"

modver="$(run "modinfo -F version /opt/ambar/modules/$KVER/drbd.ko")"
[[ "$modver" == 9.3.4 ]] || fail "drbd.ko version: $modver"

run "test -f /opt/ambar/modules/$KVER/drbd_transport_tcp.ko" \
  || fail "drbd_transport_tcp.ko is missing"

admver="$(run "drbdadm --version | sed -n 's/^DRBDADM_VERSION=//p'")"
[[ "$admver" == 9.35.0 ]] || fail "drbdadm version: $admver"

run "grep -q 'usage-count no' /etc/drbd.conf" || fail "usage-count is not disabled"
run "command -v losetup mkfs.ext4 blkid mount umount modprobe insmod drbdadm drbdsetup drbdmeta >/dev/null" \
  || fail "host tools are missing"
# The agent formats a new volume with this exact option set.
run "f=\$(mktemp) && truncate -s 16M \$f && mkfs.ext4 -q -F -E nodiscard \$f && blkid -p -o value -s TYPE \$f | grep -qx ext4" \
  || fail "mkfs.ext4 -E nodiscard does not work"
run "test -x /opt/ambar/load-module.sh" || fail "load-module.sh is missing"

agent="$(run "ambar --version" 2>&1)" || fail "ambar binary: $agent"
[[ "$agent" == "ambar "* ]] || fail "ambar binary: $agent"
run "grep -q '^install drbd /bin/false' /etc/modprobe.d/ambar-blacklist-drbd.conf" \
  || fail "the in-tree drbd module is not blacklisted in the image"

echo "OK: $IMAGE module $modver for $KVER, drbd-utils $admver"
