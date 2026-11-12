#!/usr/bin/env bash
# Removes everything the DRBD spike left on the stand. Safe to re-run.
# usage: clean.sh
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

RESOURCES=(spike spike2)
vol_dir=/var/lib/rancher/duovol/volumes
res_dir=/var/lib/rancher/duovol/res

for n in "${NODES[@]}"; do
    echo "== $n"
    # Patterns are anchored so they never match this wrapper's own command line.
    vm "$n" "pkill -f '^bash /usr/local/bin/probe.sh' || true
        pkill -x watch.sh || true
        for m in /mnt/spike /mnt/spike2; do
            if mountpoint -q \$m; then umount \$m; fi
            rmdir \$m 2>/dev/null || true
        done"
    for r in "${RESOURCES[@]}"; do
        tb "$n" "if drbdsetup status $r >/dev/null 2>&1; then drbdsetup down $r; fi"
    done
    vm "$n" "for r in ${RESOURCES[*]}; do
            f=$vol_dir/\$r.img
            for d in \$(losetup -j \$f 2>/dev/null | cut -d: -f1); do losetup -d \$d; done
            rm -f /dev/duovol/\$r \$f $res_dir/\$r.res
        done
        rmdir /dev/duovol 2>/dev/null || true
        if nft list table inet spike >/dev/null 2>&1; then nft delete table inet spike; fi
        rm -f /usr/local/bin/probe.sh /usr/local/bin/watch.sh /usr/local/bin/fsync-bench.sh
        rm -rf /var/tmp/acked /var/tmp/acked2 /var/tmp/probe.log /var/tmp/probe2.log \
            /var/tmp/watch.log /var/tmp/watch2.log /var/tmp/local"
    tb "$n" "drbdsetup status"
    vm "$n" "losetup -l; ls -A $vol_dir $res_dir; nft list tables | grep -c 'inet spike' || true"
done
