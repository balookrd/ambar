#!/usr/bin/env bash
# Creates backing files, loop devices and a three-node DRBD resource.
# usage: res.sh <name> <minor> <port> <diskless-node|-> <io-error|suspend-io>
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

name="$1"; minor="$2"; port="$3"; diskless="${4:--}"; on_no_quorum="${5:-io-error}"
size="1G"
vol_dir=/var/lib/rancher/duovol/volumes
res_dir=/var/lib/rancher/duovol/res

on_block() {
    local node="$1" id="$2" disk_line
    if [[ "$node" == "$diskless" ]]; then
        disk_line="disk none;"
    else
        disk_line="disk /dev/duovol/$name; meta-disk internal;"
    fi
    cat <<EOF
    on $(vm_host "$node") {
        node-id $id;
        address ipv4 $(vm_ip "$node"):$port;
        volume 0 { device minor $minor; $disk_line }
    }
EOF
}

res="$(cat <<EOF
resource $name {
    options { quorum majority; on-no-quorum $on_no_quorum; auto-promote yes; }
    net { protocol C; }
$(on_block dv-1 0)
$(on_block dv-2 1)
$(on_block dv-3 2)
    connection-mesh { hosts $(vm_host dv-1) $(vm_host dv-2) $(vm_host dv-3); }
}
EOF
)"

for n in "${NODES[@]}"; do
    vm "$n" "mkdir -p $vol_dir $res_dir /dev/duovol"
    printf '%s\n' "$res" | vm_put "$n" "$res_dir/$name.res"
    if [[ "$n" != "$diskless" ]]; then
        vm "$n" "f=$vol_dir/$name.img
            test -f \$f || truncate -s $size \$f
            dev=\$(losetup -j \$f | cut -d: -f1)
            test -n \"\$dev\" || dev=\$(losetup --find --show --direct-io=on \$f)
            ln -sfn \$dev /dev/duovol/$name"
        tb "$n" "drbdadm create-md --force $name"
    fi
    tb "$n" "drbdadm up $name"
done

sleep 5
tb dv-1 "drbdadm status $name"
