#!/usr/bin/env bash
# Writer probe: every number in the ack file was fsynced to the volume first.
# usage: probe.sh <mountpoint> <ack-file>
set -uo pipefail

mnt="$1"; ack="$2"
i=0
while :; do
    i=$((i + 1))
    if echo "$i" >> "$mnt/log" && sync "$mnt/log"; then
        echo "$i" > "$ack"
    else
        echo "write $i failed" >&2
        exit 1
    fi
done
