#!/usr/bin/env bash
# Pre-2b checks on the stand: changing the replica set of a running resource.
# Each check cleans up before it starts and again on exit (EXIT trap, so also after a
# failed step). A resource that is still in `drbdsetup status` after `drbdsetup down`
# keeps its backing file and loop device; a message names the node.
# usage: pre-2b.sh <p11|p12|p13|p14|clean|toolbox-off>
#   p11 a diskless node becomes a node with a copy, two orders of commands, writer on dv-1
#   p12 a node with a copy leaves; a diskless node leaves in the reverse order
#   p13 bitmap slots when the set of peers changes (--max-peers=1), with and without forget-peer
#   p14 quorum of dv-1 while the third copy is Inconsistent, with and without a witness
#   toolbox-off  delete the toolbox DaemonSet this script creates
#
# The driver (stand/install.sh) is installed on the stand and its agent treats every
# <name>.img, <name>.res and /dev/ambar/<name> as its own volume (src/agent/local.rs):
# an orphan image is refused, an orphan resource file is cleaned. The experiments
# therefore keep their files in separate directories (exp, below) that the agent does not
# list, and run in a toolbox DaemonSet that mounts those directories. The agent's
# DaemonSet, CRD and namespace are not touched.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

HERE="$(cd "$(dirname "$0")" && pwd)"
exp=/var/lib/rancher/ambar-exp
vol=$exp/volumes
res=$exp/res
dev=/dev/ambar-exp
# The same host directories as seen from the toolbox pod.
podvol=/var/lib/rancher/ambar/volumes
names=(p11 p12 p13 p14)

say() { printf '\n### %s\n' "$*"; }

# mark <label>: the clock of dv-1 (the quorum log and the writer log use it).
mark() { echo "@ $(vm dv-1 'date +%T.%3N') $*"; }

ensure_toolbox() {
    local n
    for n in "${NODES[@]}"; do
        vm "$n" "mkdir -p $vol $res $dev && chmod 700 $vol"
    done
    kc apply -f - >/dev/null <<EOF
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: toolbox
  namespace: ambar-spike
spec:
  selector:
    matchLabels: {app: toolbox}
  template:
    metadata:
      labels: {app: toolbox}
    spec:
      hostNetwork: true
      tolerations:
        - operator: Exists
      containers:
        - name: toolbox
          image: ambar-drbd:dev
          imagePullPolicy: Never
          command: ["sleep", "infinity"]
          securityContext:
            privileged: true
          volumeMounts:
            - {name: dev, mountPath: /dev}
            - {name: modules, mountPath: /lib/modules, readOnly: true}
            - {name: res, mountPath: /etc/drbd.d}
            - {name: volumes, mountPath: $podvol}
      volumes:
        - name: dev
          hostPath: {path: /dev}
        - name: modules
          hostPath: {path: /lib/modules}
        - name: res
          hostPath: {path: $res, type: Directory}
        - name: volumes
          hostPath: {path: $vol, type: Directory}
EOF
    kc -n ambar-spike rollout status ds/toolbox --timeout=180s
}

cleanup() {
    (
        set +e
        local n r st
        ensure_toolbox >/dev/null || { echo "cleanup: toolbox is not available" >&2; exit 0; }
        for n in "${NODES[@]}"; do
            vm "$n" "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true
                pkill -f 'qlog\\.sh' || true
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
                    rm -f $dev/$r $vol/$r.img $res/$r.res $vol/q-$r.txt $vol/stop-$r"
            done
            vm "$n" "rm -f $vol/qlog.sh /var/tmp/pa-acked /var/tmp/pa-probe.log /var/tmp/pa-watch.log"
        done
    )
    return 0
}

# on_block <node> <node-id> <disk|none> <name> <minor> <port>
on_block() {
    local disk_line="disk none;"
    if [[ "$3" == disk ]]; then disk_line="disk $dev/$4; meta-disk internal;"; fi
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
    # A connection-mesh needs at least two hosts; a one-member file has no connection section.
    local mesh=""
    if [[ $# -gt 1 ]]; then mesh="    connection-mesh { hosts$hosts; }"$'\n'; fi
    local text
    text="resource $name {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
${body}${mesh}}"
    for n in ${targets//,/ }; do
        vm "$n" "mkdir -p $vol $res $dev"
        printf '%s\n' "$text" | vm_put "$n" "$res/$name.res"
    done
}

# backing <node> <name> [size]: sparse file (default 1 GiB), loop device with direct I/O, link.
backing() {
    vm "$1" "set -e
        f=$vol/$2.img
        truncate -s ${3:-1G} \$f
        d=\$(losetup --find --show --direct-io=on \$f)
        [[ -n \$d ]] || { echo 'backing: losetup returned no device for $2 on $1' >&2; exit 1; }
        ln -sfn \$d $dev/$2"
}

# RC holds the return code printed by the last run_rc / must.
RC=""

parse_rc() {
    local out="$1" line
    line="$(grep '^rc=[0-9][0-9]*$' <<<"$out" | tail -n1 || true)"
    if [[ -z "$line" ]]; then
        echo "pre-2b: no rc= line in the output of '$cmd_desc': transport failure" >&2
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
    out="$(tb "$node" "$*; echo rc=\$?" 2>&1 || true)"
    printf '%s\n' "$out"
    parse_rc "$out"
}

# must <node> <command>: run_rc for a setup step; stops the experiment on a non-zero rc.
must() {
    run_rc "$@"
    if [[ "$RC" != 0 ]]; then
        echo "pre-2b: setup step failed with rc=$RC: [$1] ${*:2}" >&2
        exit 1
    fi
}

# wait_status <node> <name> <description> <command> [limit-seconds]: poll the toolbox command
# every 2 s until it succeeds; the limit (default 120 s) is wall-clock time.
wait_status() {
    local limit="${5:-120}" t0 now
    t0=$(date +%s)
    while :; do
        if tb "$1" "$4" >/dev/null 2>&1; then
            echo "wait: $3 after $(( $(date +%s) - t0 )) s"
            return 0
        fi
        now=$(date +%s)
        if (( now - t0 > limit )); then break; fi
        sleep 2
    done
    echo "pre-2b: timeout (${limit} s) waiting for $3" >&2
    tb "$1" "drbdsetup status $2 --verbose" >&2 || true
    exit 1
}

# status_all <name> <node>...: verbose status with statistics on each node.
status_all() {
    local name="$1" n; shift
    for n in "$@"; do run_rc "$n" "drbdsetup status $name --verbose --statistics"; done
}

# connected_all <name> <node>...: every node sees all the others Connected.
connected_all() {
    local name="$1" n count=$(($# - 2)); shift
    for n in "$@"; do
        wait_status "$n" "$name" "$n: $count connections Connected" \
            "[ \$(drbdsetup status $name --verbose | grep -c 'connection:Connected') = $count ]"
    done
}

# build <name> <minor> <port> <size> <member>...: the initial state of an experiment. Members
# with a disk get a file, a loop device and metadata (--max-peers=$MAXPEERS, default 7); all
# members are brought up; the initial sync is skipped on dv-1.
build() {
    local name="$1" minor="$2" port="$3" size="$4"; shift 4
    local targets="" m node id kind
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        targets+="${targets:+,}$node"
    done
    write_res "$name" "$minor" "$port" "$targets" "$@"
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        if [[ "$kind" == disk ]]; then
            backing "$node" "$name" "$size"
            must "$node" "drbdadm create-md --force --max-peers=${MAXPEERS:-7} $name"
        fi
    done
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        must "$node" "drbdadm up $name"
    done
    local nodes=()
    for m in "$@"; do
        IFS=: read -r node id kind <<<"$m"
        nodes+=("$node")
    done
    connected_all "$name" "${nodes[@]}"
    sleep 3
    must dv-1 "drbdadm new-current-uuid --clear-bitmap $name/0"
    sleep 3
    run_rc dv-1 "drbdsetup status $name --verbose"
}

# The quorum logger runs inside the toolbox of the node: once a second, the clock and the
# "quorum" field of `drbdsetup status <name> --json`.
qlog_put() {
    vm_put dv-1 "$vol/qlog.sh" <<'EOF'
#!/usr/bin/env bash
# usage: qlog.sh <resource> <stop-file>
r="$1"; stop="$2"
while [ ! -e "$stop" ]; do
    q="$(drbdsetup status "$r" --json 2>&1 | grep -o '"quorum": *[a-z]*' | head -n1)"
    echo "$(date +%T.%3N) ${q:-no-quorum-field}"
    sleep 1
done
EOF
}

qlog_start() {
    vm dv-1 "rm -f $vol/stop-$1 $vol/q-$1.txt"
    qlog_put
    # The toolbox clock is UTC, the VM clock is local: give the logger the offset of the VM so
    # that its timestamps match the writer log and the marks (POSIX TZ has the opposite sign).
    local z tz
    z="$(vm dv-1 'date +%z')"
    if [[ "${z:0:1}" == "+" ]]; then tz="UTC-${z:1:2}"; else tz="UTC+${z:1:2}"; fi
    tb dv-1 "TZ=$tz setsid nohup bash $podvol/qlog.sh $1 $podvol/stop-$1 >$podvol/q-$1.txt 2>&1 </dev/null & echo started"
}

# qsum <name>: the quorum log of dv-1 as runs of equal values: first time, last time, value, samples.
qsum() {
    vm dv-1 "cat $vol/q-$1.txt" | awk '
        { v = $2 " " $3; if (v != p) { if (NR > 1) print s " - " e "  " p "  x" n; s = $1; n = 0; p = v } e = $1; n++ }
        END { if (NR > 0) print s " - " e "  " p "  x" n }'
}

# writer_start <name> <minor>: ext4 on dv-1, mounted, probe.sh and watch.sh running, quorum logger.
writer_start() {
    must dv-1 "mkfs.ext4 -q -F -E nodiscard /dev/drbd$2"
    vm_put dv-1 /usr/local/bin/probe.sh <"$HERE/probe.sh"
    vm_put dv-1 /usr/local/bin/watch.sh <"$HERE/watch.sh"
    vm dv-1 "set -e
        chmod +x /usr/local/bin/probe.sh /usr/local/bin/watch.sh
        mkdir -p /mnt/pa
        mount /dev/drbd$2 /mnt/pa
        nohup bash /usr/local/bin/probe.sh /mnt/pa /var/tmp/pa-acked >/var/tmp/pa-probe.log 2>&1 &
        nohup bash /usr/local/bin/watch.sh /var/tmp/pa-acked >/var/tmp/pa-watch.log 2>&1 &
        sleep 5; date +%T.%3N; cat /var/tmp/pa-acked"
    qlog_start "$1"
}

# writer_report <name>: stop the writer; acknowledged number against the number on the volume;
# summary of the sampling log (states of probe.sh, longest time the acknowledged number stood
# still); probe.sh log; quorum log of dv-1.
writer_report() {
    mark "stop the writer"
    vm dv-1 "pkill -f '^bash /usr/local/bin/(probe|watch)\\.sh' || true; sleep 1
        echo acked=\$(cat /var/tmp/pa-acked) on-volume=\$(tail -n 1 /mnt/pa/log)" || echo "writer_report: reading the volume failed"
    vm dv-1 "echo '--- probe.sh log (stdout and stderr)'; cat /var/tmp/pa-probe.log
        echo '--- watch.sh log'; cat /var/tmp/pa-watch.log" | awk '
        /^--- watch.sh log/ { inw = 1; print; next }
        !inw { print; next }
        {
            n++; st[$3]++
            split($1, t, ":"); now = t[1] * 3600 + t[2] * 60 + t[3]
            if (n == 1 || $2 != last) { if (n > 1 && now - lc > gap) { gap = now - lc; gs = lt } last = $2; lc = now; lt = $1 }
        }
        END {
            printf "samples=%d", n
            for (k in st) printf " %s=%d", k, st[k]
            printf " longest-gap-without-new-acked=%.1fs (from %s)\n", gap, gs
        }'
    echo '--- quorum log of dv-1 (runs of equal values: first - last  value  samples)'
    qsum "$1"
    vm dv-1 "umount /mnt/pa || echo 'umount /mnt/pa failed'"
    vm dv-1 "touch $vol/stop-$1"
}

kmsg() {
    local name="$1" n; shift
    for n in "$@"; do
        echo "--- dmesg -T | grep 'drbd $name' | tail -n 80 [$n]"
        vm "$n" "dmesg -T | grep 'drbd $name' | tail -n 80" || true
    done
}

# wait_utd <node> <name> <limit>: poll every 5 s until the local volume of <node> is UpToDate;
# print the elapsed time from UP_T0 (epoch seconds, set by the caller) and the state lines.
wait_utd() {
    local node="$1" name="$2" limit="$3" out now
    while :; do
        out="$(tb "$node" "drbdsetup status $name --verbose --statistics" 2>&1 || true)"
        now=$(date +%s)
        echo "t+$((now - UP_T0)) s [$node]"
        grep -E 'volume:0|replication:|out-of-sync' <<<"$out" | sed 's/^ */  /' || true
        if grep -qE '^  volume:0 minor:[0-9]+ disk:UpToDate' <<<"$out"; then
            echo "UpToDate on $node: t+$((now - UP_T0)) s after the mark (poll step 5 s plus the call latency)"
            return 0
        fi
        if (( now - UP_T0 > limit )); then
            echo "NOT UpToDate on $node after $limit s"
            return 1
        fi
        sleep 5
    done
}

# ---------------------------------------------------------------- P11

# p11_prepare: dv-1 and dv-2 with a copy, dv-3 diskless, writer on dv-1.
p11_prepare() {
    build p11 1011 7011 1G dv-1:0:disk dv-2:1:disk dv-3:2:none
    writer_start p11 1011
}

# p11_with_disk <target-nodes>: p11.res with a disk on dv-3.
p11_with_disk() {
    write_res p11 1011 7011 "$1" dv-1:0:disk dv-2:1:disk dv-3:2:disk
}

p11() {
    say "P11 run 1: dv-3 changes first, then adjust on dv-1 and dv-2"
    p11_prepare
    local ok1=0 ok2=0
    say "P11 run 1, step 1: create-md on dv-3 while the resource runs without a disk there"
    backing dv-3 p11
    p11_with_disk dv-3
    mark "create-md on the running diskless resource"
    run_rc dv-3 "drbdadm create-md --force --max-peers=7 p11"
    run_rc dv-3 "drbdsetup status p11 --verbose"
    say "P11 run 1, step 2: down on dv-3, status of dv-1 and dv-2"
    mark "down on dv-3"
    run_rc dv-3 "drbdadm down p11"
    sleep 3
    run_rc dv-1 "drbdsetup status p11 --verbose"
    run_rc dv-2 "drbdsetup status p11 --verbose"
    say "P11 run 1, step 3: create-md and up on dv-3"
    mark "create-md on dv-3 after down"
    run_rc dv-3 "drbdadm create-md --force --max-peers=7 p11"
    run_rc dv-3 "drbdadm up p11"
    UP_T0=$(date +%s)
    mark "up on dv-3 (UP_T0)"
    say "P11 run 1, step 4: p11.res with a disk on dv-3 on dv-1 and dv-2, adjust"
    p11_with_disk dv-1,dv-2
    mark "adjust on dv-1"
    run_rc dv-1 "drbdadm adjust p11"
    run_rc dv-2 "drbdadm adjust p11"
    sleep 3
    status_all p11 dv-1 dv-2 dv-3
    say "P11 run 1, step 5: wait for UpToDate on dv-3"
    wait_utd dv-3 p11 300 && ok1=1
    sleep 3
    status_all p11 dv-1 dv-2 dv-3
    say "P11 run 1, step 6: stop the writer, quorum log, kernel logs"
    writer_report p11
    kmsg p11 dv-1 dv-2 dv-3
    echo "P11 run 1 result: dv-3 UpToDate=$ok1"

    say "P11 run 2: adjust on dv-1 and dv-2 first, then dv-3 changes"
    cleanup
    p11_prepare
    say "P11 run 2, step 1: p11.res with a disk on dv-3 on dv-1 and dv-2, adjust while dv-3 runs without a disk"
    backing dv-3 p11
    p11_with_disk dv-1,dv-2,dv-3
    mark "adjust on dv-1"
    run_rc dv-1 "drbdadm adjust p11"
    run_rc dv-2 "drbdadm adjust p11"
    sleep 5
    status_all p11 dv-1 dv-2 dv-3
    mark "state after adjust"
    say "P11 run 2, step 2: down on dv-3, status of dv-1 and dv-2"
    mark "down on dv-3"
    run_rc dv-3 "drbdadm down p11"
    sleep 3
    run_rc dv-1 "drbdsetup status p11 --verbose"
    run_rc dv-2 "drbdsetup status p11 --verbose"
    say "P11 run 2, step 3: create-md and up on dv-3"
    mark "create-md on dv-3 after down"
    run_rc dv-3 "drbdadm create-md --force --max-peers=7 p11"
    run_rc dv-3 "drbdadm up p11"
    UP_T0=$(date +%s)
    mark "up on dv-3 (UP_T0)"
    sleep 3
    status_all p11 dv-1 dv-2 dv-3
    say "P11 run 2, step 5: wait for UpToDate on dv-3"
    wait_utd dv-3 p11 300 && ok2=1
    sleep 3
    status_all p11 dv-1 dv-2 dv-3
    say "P11 run 2, step 6: stop the writer, quorum log, kernel logs"
    writer_report p11
    kmsg p11 dv-1 dv-2 dv-3
    echo "P11 run 2 result: dv-3 UpToDate=$ok2"
}

# ---------------------------------------------------------------- P12

p12() {
    say "P12 steps 1-5: three copies, dv-3 leaves"
    build p12 1012 7012 1G dv-1:0:disk dv-2:1:disk dv-3:2:disk
    writer_start p12 1012
    mark "down on dv-3"
    run_rc dv-3 "drbdadm down p12"
    sleep 3
    run_rc dv-1 "drbdsetup status p12 --verbose"
    run_rc dv-2 "drbdsetup status p12 --verbose"
    sleep 30
    mark "30 s after down; dv-1 quorum now"
    run_rc dv-1 "drbdsetup status p12 --verbose | grep -o 'quorum:[a-z]*'"
    say "P12 step 2: p12.res without dv-3 on dv-1 and dv-2, adjust"
    write_res p12 1012 7012 dv-1,dv-2 dv-1:0:disk dv-2:1:disk
    mark "adjust on dv-1"
    run_rc dv-1 "drbdadm adjust p12"
    run_rc dv-2 "drbdadm adjust p12"
    sleep 5
    status_all p12 dv-1 dv-2
    say "P12 step 3: forget-peer for node-id 2 on dv-1, twice"
    mark "forget-peer 1"
    run_rc dv-1 "drbdsetup forget-peer p12 2"
    mark "forget-peer 2"
    run_rc dv-1 "drbdsetup forget-peer p12 2"
    run_rc dv-1 "drbdsetup status p12 --verbose"
    say "P12 step 4: forget-peer for node-id 5 (not in the metadata) on dv-2"
    run_rc dv-2 "drbdsetup forget-peer p12 5"
    sleep 5
    say "P12 step 5: stop the writer"
    writer_report p12
    kmsg p12 dv-1 dv-2

    say "P12 step 6: a diskless node leaves, adjust on the others first"
    cleanup
    build p12 1012 7012 1G dv-1:0:disk dv-2:1:disk dv-4:3:none
    writer_start p12 1012
    say "P12 step 6a: p12.res without dv-4 on dv-1 and dv-2, adjust while dv-4 is up and connected"
    write_res p12 1012 7012 dv-1,dv-2 dv-1:0:disk dv-2:1:disk
    mark "adjust on dv-1"
    run_rc dv-1 "drbdadm adjust p12"
    run_rc dv-2 "drbdadm adjust p12"
    sleep 10
    status_all p12 dv-1 dv-2 dv-4
    mark "state after adjust"
    say "P12 step 6b: down on dv-4"
    mark "down on dv-4"
    run_rc dv-4 "drbdadm down p12"
    sleep 10
    status_all p12 dv-1 dv-2
    run_rc dv-4 "drbdsetup status p12 --verbose"
    writer_report p12
    kmsg p12 dv-1 dv-2 dv-4
}

# ---------------------------------------------------------------- P13

# hash64 <node>: sha256 of the first 64 MiB of /dev/drbd1013.
hash64() {
    run_rc "$1" "dd if=/dev/drbd1013 bs=1M count=64 iflag=direct status=none | sha256sum"
}

# p13_once <with-forget: 0|1>
p13_once() {
    local forget="$1"
    MAXPEERS=1 build p13 1013 7013 1G dv-1:0:disk dv-2:1:disk
    must dv-1 "dd if=/dev/urandom of=/dev/drbd1013 bs=1M count=64 oflag=direct status=none"
    run_rc dv-1 "drbdsetup status p13 --verbose --statistics"
    say "P13 step 1: dv-2 leaves (down on dv-2, p13.res without dv-2 on dv-1, adjust on dv-1)"
    mark "down on dv-2"
    run_rc dv-2 "drbdadm down p13"
    write_res p13 1013 7013 dv-1 dv-1:0:disk
    run_rc dv-1 "drbdadm adjust p13"
    sleep 3
    run_rc dv-1 "drbdsetup status p13 --verbose"
    if [[ "$forget" == 1 ]]; then
        mark "forget-peer 1 on dv-1"
        run_rc dv-1 "drbdsetup forget-peer p13 1"
        run_rc dv-1 "drbdsetup status p13 --verbose"
    fi
    say "P13 step 2: dv-3 (node-id 2) joins with a copy"
    backing dv-3 p13
    write_res p13 1013 7013 dv-3 dv-1:0:disk dv-3:2:disk
    run_rc dv-3 "drbdadm create-md --force --max-peers=1 p13"
    run_rc dv-3 "drbdadm up p13"
    write_res p13 1013 7013 dv-1 dv-1:0:disk dv-3:2:disk
    mark "adjust on dv-1"
    run_rc dv-1 "drbdadm adjust p13"
    sleep 30
    status_all p13 dv-1 dv-3
    kmsg p13 dv-1 dv-3
    say "P13 step 4: UpToDate on dv-3 (up to 120 s more), then the hashes"
    UP_T0=$(date +%s)
    local ok=0
    wait_utd dv-3 p13 120 && ok=1
    if [[ "$ok" == 1 ]]; then
        hash64 dv-1
        run_rc dv-3 "drbdadm primary p13"
        hash64 dv-3
        run_rc dv-3 "drbdadm secondary p13"
        status_all p13 dv-1 dv-3
    else
        echo "P13: dv-3 did not reach UpToDate, no hash comparison"
        status_all p13 dv-1 dv-3
    fi
}

p13() {
    say "P13 run 1: no forget-peer"
    p13_once 0
    say "P13 run 2: forget-peer p13 1 on dv-1 after the node left"
    cleanup
    p13_once 1
}

# ---------------------------------------------------------------- P14

# p14_once <with-witness: 1|0>
p14_once() {
    local witness="$1" members=() copies=3 nodes_after
    members=(dv-1:0:disk dv-2:1:disk)
    nodes_after=(dv-1 dv-2 dv-3)
    if [[ "$witness" == 1 ]]; then
        members+=(dv-4:3:none)
        nodes_after+=(dv-4)
    fi
    build p14 1014 7014 4G "${members[@]}"
    must dv-1 "dd if=/dev/urandom of=/dev/drbd1014 bs=1M count=2048 oflag=direct status=none"
    writer_start p14 1014
    df_free="$(vm dv-1 "df -h / | tail -n1")"
    echo "free space on dv-1 after the 2 GiB write: $df_free"
    say "P14: dv-3 joins with a copy"
    local all=("${members[@]}" dv-3:2:disk)
    write_res p14 1014 7014 dv-3 "${all[@]}"
    backing dv-3 p14 4G
    must dv-3 "drbdadm create-md --force --max-peers=7 p14"
    must dv-3 "drbdadm up p14"
    UP_T0=$(date +%s)
    mark "up on dv-3 (UP_T0)"
    write_res p14 1014 7014 dv-1,dv-2$([[ "$witness" == 1 ]] && echo ,dv-4) "${all[@]}"
    run_rc dv-1 "drbdadm adjust p14"
    run_rc dv-2 "drbdadm adjust p14"
    if [[ "$witness" == 1 ]]; then run_rc dv-4 "drbdadm adjust p14"; fi
    say "P14: wait for dv-3 Inconsistent, then down on dv-2"
    local i seen=0 out
    for i in $(seq 60); do
        out="$(tb dv-1 "drbdsetup status p14 --verbose --statistics" 2>&1 || true)"
        if grep -q 'peer-disk:Inconsistent' <<<"$out"; then seen=1; break; fi
        sleep 1
    done
    echo "$out"
    if [[ "$seen" != 1 ]]; then echo "P14: a peer with peer-disk:Inconsistent was not seen in 60 polls"; exit 1; fi
    mark "dv-3 seen Inconsistent (t+$(( $(date +%s) - UP_T0 )) s after up); down on dv-2"
    run_rc dv-2 "drbdadm down p14"
    mark "down on dv-2 returned"
    run_rc dv-1 "drbdsetup status p14 --verbose --statistics"
    sleep 20
    mark "20 s after down on dv-2"
    status_all p14 dv-1 dv-3
    say "P14: dv-2 returns"
    run_rc dv-2 "drbdadm up p14"
    mark "up on dv-2"
    wait_status dv-2 p14 "dv-2 UpToDate" "drbdsetup status p14 --verbose | grep -qE '^  volume:0 minor:[0-9]+ disk:UpToDate'" 1200
    wait_status dv-3 p14 "dv-3 UpToDate" "drbdsetup status p14 --verbose | grep -qE '^  volume:0 minor:[0-9]+ disk:UpToDate'" 1200
    sleep 3
    mark "all copies UpToDate"
    status_all p14 "${nodes_after[@]}"
    writer_report p14
    kmsg p14 dv-1
}

p14() {
    # P14_VARIANTS: which variants to run, "1" with a witness, "0" without; default both.
    local r v
    for v in ${P14_VARIANTS:-1 0}; do
        for r in 1 2; do
            if [[ "$v" == 1 ]]; then say "P14 variant with a witness, run $r"; else say "P14 variant without a witness, run $r"; fi
            p14_once "$v"
            cleanup
        done
    done
}

case "${1:-}" in
    p11|p12|p13|p14)
        trap 'exit 130' INT
        trap 'exit 143' TERM
        trap cleanup EXIT
        cleanup
        "$1"
        ;;
    clean) cleanup ;;
    toolbox-off) kc -n ambar-spike delete ds/toolbox --ignore-not-found --wait=true ;;
    *) echo "usage: $0 <p11|p12|p13|p14|clean|toolbox-off>" >&2; exit 2 ;;
esac
