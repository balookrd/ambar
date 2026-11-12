#!/usr/bin/env bash
# Loads the bundled DRBD 9 module for the running kernel.
# Refuses to continue when the in-tree DRBD 8.4 module is already loaded.
set -euo pipefail

MODULES_DIR="${MODULES_DIR:-/opt/duovol/modules}"

if [[ -d /sys/module/drbd ]]; then
    loaded="$(cat /sys/module/drbd/version)"
    case "$loaded" in
        9.*) echo "drbd $loaded is already loaded"; exit 0 ;;
        *)   echo "in-tree drbd 8.4 is loaded (version $loaded)" >&2; exit 1 ;;
    esac
fi

kver="$(uname -r)"
dir="$MODULES_DIR/$kver"
if [[ ! -f "$dir/drbd.ko" ]]; then
    echo "no drbd module for kernel $kver" >&2
    exit 1
fi

# Dependencies come from the host module tree mounted read-only at /lib/modules.
modprobe -a lru_cache libcrc32c
insmod "$dir/drbd.ko" usermode_helper=disabled
insmod "$dir/drbd_transport_tcp.ko"
echo "loaded drbd $(cat /sys/module/drbd/version) for kernel $kver"
