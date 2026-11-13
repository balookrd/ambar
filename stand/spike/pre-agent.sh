#!/usr/bin/env bash
# Pre-agent checks on the stand. Each check cleans up before and after itself.
# usage: pre-agent.sh <p1|p2|p3|p4|p5|p6|clean>
#   p1  loop devices from inside the privileged container
#   p2  drbdadm create-md --max-peers and the resulting device size
#   p3  drbdsetup status --json with and without configured resources
#   p4  drbdadm adjust on a resource that is not up
#   p5  adding a node without a disk to a running resource
#   p6  drbdadm up on a backing file without metadata
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

vol=/var/lib/rancher/ambar/volumes
res=/var/lib/rancher/ambar/res
names=(p1 p2 p3 p4 p5 p6)

say() { printf '\n### %s\n' "$*"; }

cleanup() {
    local n r
    for n in "${NODES[@]}"; do
        vm "$n" "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true
            if mountpoint -q /mnt/pa; then umount /mnt/pa; fi
            rmdir /mnt/pa 2>/dev/null || true"
        for r in "${names[@]}"; do
            tb "$n" "drbdsetup down $r 2>/dev/null || true"
        done
        vm "$n" "for r in ${names[*]}; do
                for d in \$(losetup -j $vol/\$r.img 2>/dev/null | cut -d: -f1); do losetup -d \$d; done
                rm -f /dev/ambar/\$r $vol/\$r.img $res/\$r.res
            done
            rm -f /var/tmp/pa-acked /var/tmp/pa-probe.log /var/tmp/pa-watch.log"
    done
}

# on_block <node> <node-id> <disk|none> <name> <minor> <port>
on_block() {
    local disk_line="disk none;"
    if [[ "$3" == disk ]]; then disk_line="disk /dev/ambar/$4; meta-disk internal;"; fi
    cat <<EOF
    on $(vm_host "$1") {
        node-id $2;
        address ipv4 $(vm_ip "$1"):$6;
        volume 0 { device minor $5; $disk_line }
    }
EOF
}

# write_res <name> <minor> <port> <target-nodes> <member>...
#   target-nodes: comma-separated nodes that receive the file
#   member: <node>:<node-id>:<disk|none>
write_res() {
    local name="$1" minor="$2" port="$3" targets="$4"; shift 4
    local body="" hosts="" m node id kind n
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        body+="$(on_block "$node" "$id" "$kind" "$name" "$minor" "$port")"$'\n'
        hosts+=" $(vm_host "$node")"
    done
    local text
    text="resource $name {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
${body}    connection-mesh { hosts$hosts; }
}"
    for n in ${targets//,/ }; do
        vm "$n" "mkdir -p $vol $res /dev/ambar"
        printf '%s\n' "$text" | vm_put "$n" "$res/$name.res"
    done
}

# backing <node> <name>: 1 GiB sparse file, loop device with direct I/O, link.
backing() {
    vm "$1" "f=$vol/$2.img
        truncate -s 1G \$f
        ln -sfn \$(losetup --find --show --direct-io=on \$f) /dev/ambar/$2"
}

# run_rc <node> <command>: run in the toolbox, print output and return code.
run_rc() {
    local node="$1"; shift
    echo "\$ [$node] $*"
    tb "$node" "$*; echo rc=\$?" || true
}

p1() {
    say "P1: loop devices from inside the container"
    run_rc dv-1 "truncate -s 64M $vol/p1.img && losetup --find --show --direct-io=on $vol/p1.img"
    run_rc dv-1 "losetup -l -O NAME,BACK-FILE,DIO | grep p1.img"
    run_rc dv-1 "losetup -j $vol/p1.img"
    echo "\$ [dv-1 host] losetup -j $vol/p1.img"
    vm dv-1 "losetup -j $vol/p1.img; echo rc=\$?"
    run_rc dv-1 "mkdir -p /dev/ambar && ln -sfn \$(losetup -j $vol/p1.img | cut -d: -f1) /dev/ambar/p1 && ls -l /dev/ambar/p1"
    echo "\$ [dv-1 host] ls -l /dev/ambar/p1"
    vm dv-1 "ls -l /dev/ambar/p1; echo rc=\$?"
    run_rc dv-1 "losetup -d \$(losetup -j $vol/p1.img | cut -d: -f1)"
}

p2() {
    say "P2: create-md --max-peers=7, device size for a 1 GiB file"
    write_res p2 1002 7002 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    local n
    for n in "${NODES[@]}"; do
        backing "$n" p2
        run_rc "$n" "drbdadm create-md --force --max-peers=7 p2"
        run_rc "$n" "drbdadm up p2"
    done
    sleep 5
    run_rc dv-1 "drbdsetup status p2 --json | grep -E '\"size\"|\"disk-state\"|\"connection-state\"'"
    echo "expected \"size\" by the formula (ceil(sectors/2^18)*8*7+72 sectors of metadata): 1048316"
    run_rc dv-1 "drbdadm new-current-uuid --clear-bitmap p2/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p2 --json | grep -E '\"disk-state\"|\"peer-disk-state\"'"
}

p3() {
    say "P3: drbdsetup status --json without resources"
    run_rc dv-1 "drbdsetup status --json"
    run_rc dv-1 "drbdsetup status"
    run_rc dv-1 "drbdsetup status nosuch --json"
    say "P3: the same with one configured resource"
    write_res p3 1003 7003 dv-1 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    backing dv-1 p3
    run_rc dv-1 "drbdadm create-md --force p3 && drbdadm up p3"
    run_rc dv-1 "drbdsetup status --json | head -c 300"
    run_rc dv-1 "drbdsetup status nosuch --json"
}

p4() {
    say "P4: drbdadm adjust on a resource that is not up"
    write_res p4 1004 7004 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    local n
    for n in dv-1 dv-2; do
        backing "$n" p4
        run_rc "$n" "drbdadm create-md --force p4"
    done
    say "P4: node with a copy, resource never configured: adjust instead of up"
    run_rc dv-1 "drbdadm adjust p4"
    run_rc dv-1 "drbdsetup status p4"
    say "P4: second node with a copy: adjust"
    run_rc dv-2 "drbdadm adjust p4"
    say "P4: node without a disk: adjust"
    run_rc dv-3 "drbdadm adjust p4"
    sleep 5
    run_rc dv-1 "drbdsetup status p4 --verbose"
    run_rc dv-3 "drbdsetup status p4 --verbose"
}

p5() {
    say "P5: two-node resource with a writer, then a node without a disk is added"
    write_res p5 1005 7005 dv-1,dv-2 dv-1:0:disk dv-2:1:disk
    local n
    for n in dv-1 dv-2; do
        backing "$n" p5
        run_rc "$n" "drbdadm create-md --force --max-peers=7 p5"
        run_rc "$n" "drbdadm up p5"
    done
    sleep 5
    run_rc dv-1 "drbdadm new-current-uuid --clear-bitmap p5/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p5 --verbose"
    tb dv-1 "mkfs.ext4 -q /dev/drbd1005"
    cat "$(dirname "$0")/probe.sh" | vm_put dv-1 /usr/local/bin/probe.sh
    cat "$(dirname "$0")/watch.sh" | vm_put dv-1 /usr/local/bin/watch.sh
    vm dv-1 "chmod +x /usr/local/bin/probe.sh /usr/local/bin/watch.sh
        mkdir -p /mnt/pa && mount /dev/drbd1005 /mnt/pa
        nohup bash /usr/local/bin/probe.sh /mnt/pa /var/tmp/pa-acked >/var/tmp/pa-probe.log 2>&1 &
        nohup bash /usr/local/bin/watch.sh /var/tmp/pa-acked >/var/tmp/pa-watch.log 2>&1 &
        sleep 5; date +%T.%3N; cat /var/tmp/pa-acked"

    say "P5: the resource file now lists three nodes; adjust on the two running nodes"
    write_res p5 1005 7005 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    vm dv-1 "date +%T.%3N"
    run_rc dv-1 "drbdadm adjust p5"
    run_rc dv-2 "drbdadm adjust p5"
    say "P5: the new node brings the resource up without a disk"
    run_rc dv-3 "drbdadm up p5"
    sleep 8
    for n in "${NODES[@]}"; do run_rc "$n" "drbdsetup status p5 --verbose"; done
    say "P5: did the writer stall or fail while the node was added?"
    vm dv-1 "tail -n 40 /var/tmp/pa-watch.log; cat /var/tmp/pa-probe.log
        dmesg -T | grep -E 'drbd p5' | tail -n 30"
    say "P5: stop the writer, compare acknowledged and on-volume numbers, read through the new node"
    vm dv-1 "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true; sleep 1
        echo acked=\$(cat /var/tmp/pa-acked) on-volume=\$(tail -n 1 /mnt/pa/log); umount /mnt/pa"
    vm dv-3 "mkdir -p /mnt/pa && mount /dev/drbd1005 /mnt/pa && echo on-volume-via-dv-3=\$(tail -n 1 /mnt/pa/log); umount /mnt/pa"
}

p6() {
    say "P6: drbdadm up on a backing file without metadata"
    write_res p6 1006 7006 dv-1 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    backing dv-1 p6
    run_rc dv-1 "drbdadm up p6"
    run_rc dv-1 "drbdsetup status p6"
    run_rc dv-1 "drbdadm adjust p6"
}

case "${1:-}" in
    p1|p2|p3|p4|p5|p6) cleanup; "$1"; cleanup ;;
    clean) cleanup ;;
    *) echo "usage: $0 <p1|p2|p3|p4|p5|p6|clean>" >&2; exit 2 ;;
esac
