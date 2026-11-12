#!/usr/bin/env bash
# Verifies the sklad-drbd image without loading the kernel module.
set -euo pipefail

KVER="${KVER:-6.12.116+deb13-arm64}"
IMAGE="${IMAGE:-sklad-drbd:dev}"

run() { docker run --rm --platform linux/arm64 "$IMAGE" bash -c "$1"; }

fail() { echo "FAIL: $1" >&2; exit 1; }

vermagic="$(run "modinfo -F vermagic /opt/sklad/modules/$KVER/drbd.ko")"
[[ "$vermagic" == "$KVER SMP "* ]] || fail "drbd.ko vermagic: $vermagic"

modver="$(run "modinfo -F version /opt/sklad/modules/$KVER/drbd.ko")"
[[ "$modver" == 9.3.4 ]] || fail "drbd.ko version: $modver"

run "test -f /opt/sklad/modules/$KVER/drbd_transport_tcp.ko" \
  || fail "drbd_transport_tcp.ko is missing"

admver="$(run "drbdadm --version | sed -n 's/^DRBDADM_VERSION=//p'")"
[[ "$admver" == 9.35.0 ]] || fail "drbdadm version: $admver"

run "grep -q 'usage-count no' /etc/drbd.conf" || fail "usage-count is not disabled"
run "command -v losetup mkfs.ext4 modprobe insmod >/dev/null" || fail "host tools are missing"
run "test -x /opt/sklad/load-module.sh" || fail "load-module.sh is missing"

echo "OK: $IMAGE module $modver for $KVER, drbd-utils $admver"
