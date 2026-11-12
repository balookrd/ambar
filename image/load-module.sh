#!/usr/bin/env bash
# Loads the bundled DRBD 9 module for the running kernel.
# Refuses to continue when the in-tree DRBD 8.4 module is already loaded.
set -euo pipefail

MODULES_DIR="${MODULES_DIR:-/opt/duovol/modules}"
SYS_MODULE_DIR="${SYS_MODULE_DIR:-/sys/module}"

kver="$(uname -r)"
dir="$MODULES_DIR/$kver"

# Resources cannot connect without the TCP transport, so a loaded drbd core
# alone is not a finished state: a previous run may have died between the
# two insmod calls.
load_transport() {
    [[ -d "$SYS_MODULE_DIR/drbd_transport_tcp" ]] && return 0
    if [[ ! -f "$dir/drbd_transport_tcp.ko" ]]; then
        echo "no drbd_transport_tcp module for kernel $kver" >&2
        exit 1
    fi
    insmod "$dir/drbd_transport_tcp.ko"
}

if [[ -d "$SYS_MODULE_DIR/drbd" ]]; then
    loaded="$(cat "$SYS_MODULE_DIR/drbd/version")"
    case "$loaded" in
        9.*)
            load_transport
            echo "drbd $loaded is already loaded"
            exit 0
            ;;
        *)
            echo "in-tree drbd 8.4 is loaded (version $loaded)" >&2
            exit 1
            ;;
    esac
fi

if [[ ! -f "$dir/drbd.ko" ]]; then
    echo "no drbd module for kernel $kver" >&2
    exit 1
fi

# Dependencies come from the host module tree mounted read-only at /lib/modules.
modprobe -a lru_cache libcrc32c
insmod "$dir/drbd.ko" usermode_helper=disabled
load_transport
echo "loaded drbd $(cat "$SYS_MODULE_DIR/drbd/version") for kernel $kver"
