#!/usr/bin/env bash
# End-to-end check of the agent CLI on the stand: one volume description,
# three nodes (two copies and a node without a disk).
# usage: agent-smoke.sh [--reboot]
#   --reboot  also force-stops and restarts dv-2 and lets the agent restore it
# After a failed run clean up with:
#   bash -c 'source stand/lib.sh; for n in "${NODES[@]}"; do tb $n "ambar remove --name smoke"; done'
set -euo pipefail
source "$(dirname "$0")/lib.sh"

name=smoke
minor=1010
vol_dir=/var/lib/rancher/ambar/volumes
res_dir=/var/lib/rancher/ambar/res
spec=$vol_dir/$name.json
reboot="${1:-}"
[[ -z "$reboot" || "$reboot" == --reboot ]] || { echo "usage: $0 [--reboot]" >&2; exit 2; }

fail() { echo "FAIL: $1" >&2; exit 1; }

json="$(cat <<EOF
{
  "name": "$name",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": $minor,
  "port": 7010,
  "maxPeers": 7,
  "nodes": [
    {"name": "$(vm_host dv-1)", "address": "$(vm_ip dv-1)", "nodeId": 0, "diskful": true},
    {"name": "$(vm_host dv-2)", "address": "$(vm_ip dv-2)", "nodeId": 1, "diskful": true},
    {"name": "$(vm_host dv-3)", "address": "$(vm_ip dv-3)", "nodeId": 2, "diskful": false}
  ]
}
EOF
)"

apply() { tb "$1" "ambar apply --file $spec"; }
state() { tb "$1" "ambar status --name $name"; }

# wait_for <node> <pattern> <seconds>: poll `ambar status` until the pattern
# is found.
wait_for() {
    local node="$1" pattern="$2" deadline=$(( $(date +%s) + $3 )) out
    while :; do
        out="$(state "$node")"
        if grep -q -E "$pattern" <<<"$out"; then return 0; fi
        if (( $(date +%s) > deadline )); then
            echo "$out" >&2
            fail "$node: no '$pattern' within $3 s"
        fi
        sleep 2
    done
}

# expect_apply <node> <expected output>: one apply, exact output.
expect_apply() {
    local out
    out="$(apply "$1")" || fail "$1: apply failed: $out"
    [[ "$out" == "$name: $2" ]] || fail "$1: apply printed '$out', expected '$name: $2'"
}

# expect_apply_re <node> <regex>: one apply, output matches the regex.
expect_apply_re() {
    local out
    out="$(apply "$1")" || fail "$1: apply failed: $out"
    [[ "$out" =~ ^$name:\ $2$ ]] || fail "$1: apply printed '$out', expected to match '$2'"
}

# loops_of_volume <node>: loop devices whose backing file is this volume's.
loops_of_volume() {
    vm "$1" "losetup -a | grep '$vol_dir/$name.img' || true"
}

# wait_no_loops <node>: `losetup -d` is not instantaneous.
wait_no_loops() {
    local i
    for i in $(seq 1 20); do
        [[ -z "$(loops_of_volume "$1")" ]] && return 0
        sleep 0.5
    done
    fail "$1: loop device of $name is still listed"
}

echo "== a. the binary is in every toolbox pod; the module is loaded"
for n in "${NODES[@]}"; do
    out="$(tb "$n" "ambar --version")" || fail "$n: ambar --version failed"
    [[ "$out" == "ambar "* ]] || fail "$n: ambar --version printed '$out'"
    out="$(tb "$n" "ambar load-module")" || fail "$n: ambar load-module failed"
    [[ "$out" == "drbd 9.3.4 is already loaded" ]] || fail "$n: load-module printed '$out'"
done

echo "== leftovers of an earlier run"
for n in "${NODES[@]}"; do
    tb "$n" "ambar remove --name $name" >/dev/null 2>&1 || true
    vm "$n" "if mountpoint -q /mnt/$name; then umount /mnt/$name; fi
        rmdir /mnt/$name 2>/dev/null || true; rm -f $spec"
done

echo "== b. apply on three nodes"
for n in "${NODES[@]}"; do
    printf '%s\n' "$json" | vm_put "$n" "$spec"
done
for n in dv-1 dv-2; do
    expect_apply_re "$n" '\[CreateFile \{ bytes: [0-9]+ \}, AttachLoop, LinkDevice, WriteRes, CreateMd, MarkReady, Up, MarkApplied\]'
done
expect_apply dv-3 "[WriteRes, Up, MarkApplied]"

echo "== c. the leader skips the initial sync once everyone is connected"
start=$(date +%s)
skipped=no
while (( $(date +%s) - start < 60 )); do
    out="$(apply dv-1)" || fail "dv-1: apply failed: $out"
    if [[ "$out" == "$name: [SkipInitialSync]" ]]; then skipped=yes; break; fi
    [[ "$out" == "$name: []" ]] || fail "dv-1: apply printed '$out' while waiting for the gate"
    sleep 2
done
[[ "$skipped" == yes ]] || fail "dv-1 never printed [SkipInitialSync] within 60 s"
echo "leader took $(( $(date +%s) - start )) s to print [SkipInitialSync]"
wait_for dv-1 "disk=UpToDate" 60
wait_for dv-2 "disk=UpToDate" 60
wait_for dv-3 "disk=Diskless quorum=true" 60
[[ "$(state dv-1 | grep -c ': Connected ')" == 2 ]] || fail "dv-1 does not see two connected peers"

echo "== d. settling"
for n in dv-1 dv-2; do
    out="$(apply "$n")" || fail "$n: apply failed: $out"
    [[ "$out" == "$name: [MarkSynced]" || "$out" == "$name: []" ]] \
        || fail "$n: apply printed '$out', expected [MarkSynced] or []"
done
for _ in 1 2; do
    for n in "${NODES[@]}"; do
        expect_apply "$n" "[]"
    done
done

echo "== e. files of the volume"
for n in dv-1 dv-2; do
    mode="$(vm "$n" "stat -c %a $vol_dir/$name.img")"
    [[ "$mode" == 600 ]] || fail "$n: $name.img has mode $mode"
    vm "$n" "test -f $vol_dir/$name.img.ready && test -f $vol_dir/$name.img.synced && test -f $vol_dir/$name.lock \
        && test -f $res_dir/$name.res && test -f $res_dir/$name.res.applied" \
        || fail "$n: a per-volume file is missing"
done
vm dv-3 "test -f $res_dir/$name.res && test -f $res_dir/$name.res.applied" \
    || fail "dv-3: a resource-dir file is missing"
vm dv-3 "test ! -e $vol_dir/$name.img && test ! -e $vol_dir/$name.img.ready && test ! -e $vol_dir/$name.img.synced" \
    || fail "dv-3: it has files of a copy"

echo "== f. write through the node without a disk, read on a node with a copy"
tb dv-1 "mkfs.ext4 -q /dev/drbd$minor"
vm dv-3 "mkdir -p /mnt/$name && mount /dev/drbd$minor /mnt/$name && echo ambar > /mnt/$name/f && sync /mnt/$name/f && umount /mnt/$name"
got="$(vm dv-1 "mkdir -p /mnt/$name && mount /dev/drbd$minor /mnt/$name && cat /mnt/$name/f && umount /mnt/$name")"
[[ "$got" == ambar ]] || fail "read back '$got'"

echo "== g. remove refuses bad names"
for bad in all ../x; do
    rc=0
    out="$(tb dv-1 "ambar remove --name $bad" 2>&1)" || rc=$?
    [[ "$rc" != 0 ]] || fail "ambar remove --name $bad exited 0: $out"
    [[ "$out" == *"ambar: "* ]] || fail "ambar remove --name $bad: no agent error message: $out"
    state dv-1 | grep -q "^$name: role=" || fail "after refusing '$bad' the volume is not configured on dv-1"
    vm dv-1 "test -f $vol_dir/$name.img" || fail "after refusing '$bad' the volume file is gone"
done

echo "== h. a copy that lost its loop device and kernel state is restored"
tb dv-2 "drbdadm down $name"
vm dv-2 "losetup -d \$(losetup -j $vol_dir/$name.img | cut -d: -f1); rm -f /dev/ambar/$name"
wait_no_loops dv-2
expect_apply dv-2 "[AttachLoop, LinkDevice, Up, MarkApplied]"
wait_for dv-2 "disk=UpToDate" 60
expect_apply dv-2 "[]"

echo "== i. a detached disk is attached again"
tb dv-2 "drbdadm detach $name"
wait_for dv-2 "disk=Diskless" 30
expect_apply dv-2 "[Adjust, MarkApplied]"
wait_for dv-2 "disk=UpToDate" 60

echo "== j. the lock file exists while the volume exists"
for n in dv-1 dv-2; do
    vm "$n" "test -f $vol_dir/$name.lock" || fail "$n: no lock file"
done

if [[ "$reboot" == --reboot ]]; then
    echo "== k. dv-2 is force-stopped and started again"
    limactl stop --force dv-2
    "$(dirname "$0")/up.sh"
    kc wait --for=condition=Ready node --all --timeout=300s
    kc -n ambar-spike rollout status ds/toolbox --timeout=180s
    deadline=$(( $(date +%s) + 120 ))
    until tb dv-2 "ambar --version" >/dev/null 2>&1; do
        (( $(date +%s) < deadline )) || fail "dv-2: toolbox pod does not run commands within 120 s"
        sleep 2
    done
    out="$(tb dv-2 "ambar load-module")" || fail "dv-2: load-module failed: $out"
    [[ "$out" == "loaded drbd 9.3.4 "* ]] || fail "dv-2: load-module printed '$out'"
    expect_apply dv-2 "[AttachLoop, LinkDevice, Up, MarkApplied]"
    wait_for dv-2 "disk=UpToDate" 120
fi

echo "== l. remove everywhere"
full='["down", "detach-loop", "remove-file", "remove-marker", "remove-synced-marker", "remove-link", "remove-res", "remove-applied"]'
nodisk='["down", "remove-res", "remove-applied"]'
for n in "${NODES[@]}"; do
    want="$full"
    [[ "$n" != dv-3 ]] || want="$nodisk"
    out="$(tb "$n" "ambar remove --name $name")" || fail "$n: remove failed: $out"
    [[ "$out" == "$name: $want" ]] || fail "$n: remove printed '$out', expected '$name: $want'"
    vm "$n" "rm -f $spec; rmdir /mnt/$name 2>/dev/null || true"
    wait_no_loops "$n"
    left="$(vm "$n" "ls -A $vol_dir $res_dir | grep -F '$name' || true")"
    [[ -z "$left" ]] || fail "$n: files of the volume are left: $left"
    vm "$n" "test ! -e /dev/ambar/$name" || fail "$n: /dev/ambar/$name is left"
    [[ "$(state "$n")" == "$name: not configured" ]] || fail "$n: resource is still configured"
done

echo "OK: agent smoke"
