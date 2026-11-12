#!/usr/bin/env bash
# Samples the writer probe twice a second: wall clock, last acked number,
# whether probe.sh is alive and its scheduler state (D = blocked in the kernel).
# usage: watch.sh <ack-file>   (runs inside the VM, prints to stdout)
set -uo pipefail

ack="$1"
while :; do
    # Anchored: an unanchored pattern also matches "sudo bash -c ... probe.sh ..." wrappers.
    pid="$(pgrep -f '^bash /usr/local/bin/probe.sh' | head -n1)"
    if [[ -n "$pid" ]]; then
        st="$(awk '/^State:/ {print $2}' "/proc/$pid/status" 2>/dev/null)"
        probe="alive:${st:-?}"
    else
        probe="gone"
    fi
    echo "$(date +%T.%3N) acked=$(cat "$ack" 2>/dev/null) probe=$probe"
    sleep 0.5
done
