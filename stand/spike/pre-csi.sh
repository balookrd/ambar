#!/usr/bin/env bash
# Pre-CSI checks on the stand. Each check cleans up before it starts and again on exit
# (EXIT trap, so also after a failed step). A resource that is still in `drbdsetup status`
# after `drbdsetup down` keeps its backing file and loop device; a message names the node.
# usage: pre-csi.sh <p7|p8|p9|clean>
#   p7  drbdadm dump-md on a file without metadata, with metadata, and on a resource that is up
#   p8  removing a node without a disk from a running resource with a writer
#   p9  blkid -p and a 2-byte read at offset 1080 on a DRBD device in four states
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

vol=/var/lib/rancher/ambar/volumes
res=/var/lib/rancher/ambar/res
names=(p7 p8 p9)

say() { printf '\n### %s\n' "$*"; }

cleanup() {
    (
        set +e
        local n r st
        for n in "${NODES[@]}"; do
            vm "$n" "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true
                for i in 1 2 3 4 5 6 7 8 9 10; do
                    pgrep -f '^bash /usr/local/bin/(probe|watch)\\.sh' >/dev/null || break
                    sleep 0.5
                done
                if mountpoint -q /mnt/pa; then umount /mnt/pa || echo 'cleanup: umount /mnt/pa failed' >&2; fi
                rmdir /mnt/pa 2>/dev/null || true"
            for r in "${names[@]}"; do
                tb "$n" "drbdsetup down $r 2>/dev/null || true"
            done
            for r in "${names[@]}"; do
                st="$(tb "$n" "drbdsetup status 2>&1; echo status-rc=\$?" 2>/dev/null)"
                if [[ "$st" != *status-rc=* ]]; then
                    echo "cleanup: cannot read drbdsetup status on $n; leaving files of $r" >&2
                    continue
                fi
                if grep -q "^$r " <<<"$st"; then
                    echo "cleanup: resource $r is still in drbdsetup status on $n; leaving $vol/$r.img and its loop device" >&2
                    continue
                fi
                vm "$n" "for d in \$(losetup -j $vol/$r.img 2>/dev/null | cut -d: -f1); do losetup -d \$d; done
                    rm -f /dev/ambar/$r $vol/$r.img $res/$r.res"
            done
            vm "$n" "rm -f /var/tmp/pa-acked /var/tmp/pa-probe.log /var/tmp/pa-watch.log"
        done
    )
    return 0
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
    vm "$1" "set -e
        f=$vol/$2.img
        truncate -s 1G \$f
        d=\$(losetup --find --show --direct-io=on \$f)
        [[ -n \$d ]] || { echo 'backing: losetup returned no device for $2 on $1' >&2; exit 1; }
        ln -sfn \$d /dev/ambar/$2"
}

# RC holds the return code printed by the last run_rc / run_split / must.
RC=""

# parse_rc <first|last> <output>: set RC from the rc= line; exit if there is none
# (the transport to the toolbox failed, the command's own code was not captured).
parse_rc() {
    local which="$1" out="$2" line
    if [[ "$which" == first ]]; then
        line="$(grep -m1 '^rc=[0-9][0-9]*$' <<<"$out" || true)"
    else
        line="$(grep '^rc=[0-9][0-9]*$' <<<"$out" | tail -n1 || true)"
    fi
    if [[ -z "$line" ]]; then
        echo "pre-csi: no rc= line in the output of '$cmd_desc': transport failure" >&2
        exit 1
    fi
    RC="${line#rc=}"
}

# run_rc <node> <command>: run in the toolbox, print output and return code (RC).
# A non-zero rc of the command is not an error here: it is what gets measured.
run_rc() {
    local node="$1" out cmd_desc; shift
    cmd_desc="[$node] $*"
    echo "\$ $cmd_desc"
    out="$(tb "$node" "$*; echo rc=\$?" || true)"
    printf '%s\n' "$out"
    parse_rc last "$out"
}

# must <node> <command>: run_rc for a setup step; stops the experiment on a non-zero rc.
must() {
    run_rc "$@"
    if [[ "$RC" != 0 ]]; then
        echo "pre-csi: setup step failed with rc=$RC: [$1] ${*:2}" >&2
        exit 1
    fi
}

# run_split <node> <command>: run in the toolbox, print rc, the first 20 lines
# of stdout and the whole stderr separately.
run_split() {
    local node="$1" out cmd_desc; shift
    cmd_desc="[$node] $*"
    echo "\$ $cmd_desc"
    out="$(tb "$node" "t=\$(mktemp -d); { set -o pipefail; $*; } >\$t/out 2>\$t/err </dev/null; rc=\$?
        echo rc=\$rc
        echo '--- stdout (first 20 lines)'; head -n 20 \$t/out
        echo '--- stderr'; cat \$t/err; rm -rf \$t" || true)"
    printf '%s\n' "$out"
    parse_rc first "$out"
}

p7() {
    say "P7: drbdadm dump-md in three states, resource p7 on dv-1"
    write_res p7 1007 7007 dv-1 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    backing dv-1 p7
    say "P7 (1): empty sparse file, no metadata"
    run_split dv-1 "drbdadm dump-md p7"
    say "P7 (2): after create-md --force --max-peers=7"
    must dv-1 "drbdadm create-md --force --max-peers=7 p7"
    run_split dv-1 "drbdadm dump-md p7"
    say "P7 (3): after drbdadm up (disk attached)"
    must dv-1 "drbdadm up p7"
    run_split dv-1 "drbdadm dump-md p7"
    run_rc dv-1 "drbdsetup status p7 --verbose"
}

p8() {
    say "P8: two copies and a diskless node, writer on dv-1; the diskless node is removed"
    write_res p8 1008 7008 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    local n
    for n in dv-1 dv-2; do
        backing "$n" p8
        must "$n" "drbdadm create-md --force --max-peers=7 p8"
    done
    for n in "${NODES[@]}"; do must "$n" "drbdadm up p8"; done
    sleep 5
    must dv-1 "drbdadm new-current-uuid --clear-bitmap p8/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p8 --verbose"
    must dv-1 "mkfs.ext4 -q /dev/drbd1008"
    vm_put dv-1 /usr/local/bin/probe.sh <"$(dirname "$0")/probe.sh"
    vm_put dv-1 /usr/local/bin/watch.sh <"$(dirname "$0")/watch.sh"
    vm dv-1 "set -e
        chmod +x /usr/local/bin/probe.sh /usr/local/bin/watch.sh
        mkdir -p /mnt/pa
        mount /dev/drbd1008 /mnt/pa
        nohup bash /usr/local/bin/probe.sh /mnt/pa /var/tmp/pa-acked >/var/tmp/pa-probe.log 2>&1 &
        nohup bash /usr/local/bin/watch.sh /var/tmp/pa-acked >/var/tmp/pa-watch.log 2>&1 &
        sleep 5; date +%T.%3N; cat /var/tmp/pa-acked"

    say "P8 step 1: down on dv-3, delete its p8.res"
    vm dv-1 "date +%T.%3N"
    run_rc dv-3 "drbdadm down p8"
    vm dv-3 "rm -f $res/p8.res; ls $res/p8.res 2>&1 || true"
    sleep 5
    run_rc dv-1 "drbdsetup status p8 --verbose"
    run_rc dv-2 "drbdsetup status p8 --verbose"

    say "P8 step 2: p8.res without dv-3 on dv-1 and dv-2, adjust"
    write_res p8 1008 7008 dv-1,dv-2 dv-1:0:disk dv-2:1:disk
    vm dv-1 "cat $res/p8.res"
    vm dv-1 "date +%T.%3N"
    run_rc dv-1 "drbdadm adjust p8"
    run_rc dv-2 "drbdadm adjust p8"
    sleep 5
    run_rc dv-1 "drbdsetup status p8 --verbose"
    run_rc dv-2 "drbdsetup status p8 --verbose"

    say "P8 step 3: writer log, kernel log, quorum"
    vm dv-1 "echo '--- watch.sh tail'; tail -n 40 /var/tmp/pa-watch.log
        echo '--- probe.sh log'; cat /var/tmp/pa-probe.log
        echo '--- dmesg'; dmesg -T | grep 'drbd p8' | tail -n 60"
    run_rc dv-1 "drbdsetup status p8 --verbose | grep -o 'quorum:[a-z]*'"
    run_rc dv-2 "drbdsetup status p8 --verbose | grep -o 'quorum:[a-z]*'"

    say "P8 step 4: dv-3 returns with the same node-id"
    write_res p8 1008 7008 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:none
    vm dv-1 "date +%T.%3N"
    run_rc dv-1 "drbdadm adjust p8"
    run_rc dv-2 "drbdadm adjust p8"
    run_rc dv-3 "drbdadm up p8"
    sleep 8
    for n in "${NODES[@]}"; do run_rc "$n" "drbdsetup status p8 --verbose --statistics"; done
    vm dv-1 "echo '--- watch.sh tail'; tail -n 20 /var/tmp/pa-watch.log
        echo '--- dmesg'; dmesg -T | grep 'drbd p8' | tail -n 40"

    say "P8 step 5: stop the writer, compare acknowledged and on-volume numbers"
    vm dv-1 "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true; sleep 1
        echo acked=\$(cat /var/tmp/pa-acked) on-volume=\$(tail -n 1 /mnt/pa/log); umount /mnt/pa"
}

p9() {
    say "P9: blkid -p and a 2-byte read at offset 1080 on /dev/drbd1009, read on dv-2"
    write_res p9 1009 7009 dv-1,dv-2,dv-3 dv-1:0:disk dv-2:1:disk dv-3:2:disk
    local n
    for n in "${NODES[@]}"; do
        backing "$n" p9
        must "$n" "drbdadm create-md --force --max-peers=7 p9"
        must "$n" "drbdadm up p9"
    done
    sleep 5
    must dv-1 "drbdadm new-current-uuid --clear-bitmap p9/0"
    sleep 3
    run_rc dv-1 "drbdsetup status p9 --verbose"
    local blk="blkid -p -o value -s TYPE /dev/drbd1009"
    local rd="dd if=/dev/drbd1009 bs=1 skip=1080 count=2 status=none | od -An -tx1"
    say "P9 (1): no filesystem"
    run_split dv-2 "$blk"
    run_split dv-2 "$rd"
    say "P9 (2): after mkfs.ext4 -q on dv-1, not mounted"
    must dv-1 "mkfs.ext4 -q /dev/drbd1009"
    sleep 3
    run_split dv-2 "$blk"
    run_split dv-2 "$rd"
    say "P9 (3): mounted on dv-1"
    vm dv-1 "set -e; mkdir -p /mnt/pa; mount /dev/drbd1009 /mnt/pa"
    run_rc dv-1 "drbdsetup status p9 --verbose"
    run_split dv-2 "$blk"
    run_split dv-2 "$rd"
    run_rc dv-2 "drbdsetup status p9 --verbose"
    vm dv-1 "umount /mnt/pa"
    say "P9 (4): /dev/drbd1999 does not exist"
    run_split dv-2 "ls -l /dev/drbd1999"
    run_split dv-2 "blkid -p -o value -s TYPE /dev/drbd1999"
    run_split dv-2 "dd if=/dev/drbd1999 bs=1 skip=1080 count=2 status=none | od -An -tx1"
}

case "${1:-}" in
    p7|p8|p9)
        trap 'exit 130' INT
        trap 'exit 143' TERM
        trap cleanup EXIT
        cleanup
        "$1"
        ;;
    clean) cleanup ;;
    *) echo "usage: $0 <p7|p8|p9|clean>" >&2; exit 2 ;;
esac
