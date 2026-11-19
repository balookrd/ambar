#!/usr/bin/env bash
# Checks NanoPi R5C physical nodes prerequisites and records baseline k3s memory.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

echo "=== 1. Checking physical nodes availability and kernel ==="
for n in "${R5C_NODES[@]}"; do
    kver="$(r5c_ssh "$n" "uname -r")"
    echo "Node $n: kernel $kver"
    if [[ "$kver" != *"deb13-arm64"* ]]; then
        echo "FAIL: Node $n has unexpected kernel: $kver" >&2
        exit 1
    fi
    free_gb="$(r5c_ssh "$n" "df -BG /var/lib/rancher | awk 'NR==2 {print \$4}' | tr -d 'G'")"
    echo "Node $n: /var/lib/rancher has ${free_gb} GB free"
    if (( free_gb < 20 )); then
        echo "FAIL: Node $n has insufficient disk space: ${free_gb} GB" >&2
        exit 1
    fi
done

echo "=== 2. Checking network interconnect (lan0) ==="
for i in "${!R5C_NODES[@]}"; do
    n="${R5C_NODES[$i]}"
    ip="${R5C_IPS[$i]}"
    has_ip="$(r5c_ssh "$n" "ip addr show lan0 | grep -c '$ip' || true")"
    if (( has_ip < 1 )); then
        echo "FAIL: Node $n does not have $ip on lan0" >&2
        exit 1
    fi
    echo "Node $n: lan0 $ip OK"
done

echo "=== 3. Recording baseline k3s-server RSS ==="
results="{"
sep=""
for n in "${R5C_NODES[@]}"; do
    rss_kb="$(r5c_k3s_rss_kb "$n")"
    echo "Node $n k3s-server RSS: ${rss_kb} KiB ($(( rss_kb / 1024 )) MiB)"
    results="${results}${sep}\"${n}\": ${rss_kb}"
    sep=", "
done
results="${results}}"
echo "$results" > "$HERE/baseline-k3s-rss.json"
echo "Saved baseline to $HERE/baseline-k3s-rss.json"

echo "=== 4. Checking LVM topology (k3s-vg / lv_k3s) ==="
CHECK_LVM="${CHECK_LVM:-auto}"
for n in "${R5C_NODES[@]}"; do
    has_vg="$(r5c_ssh "$n" "command -v vgs >/dev/null 2>&1 && vgs k3s-vg --noheadings >/dev/null 2>&1 && echo 1 || echo 0")"
    if [[ "$has_vg" == "1" ]]; then
        r5c_ssh "$n" "vgs k3s-vg --noheadings -o vg_free --units g | tr -d '<gG ' | awk '{if (\$1 < 40) exit 1}'"
        r5c_ssh "$n" "lvs /dev/k3s-vg/lv_k3s >/dev/null"
        vg_free_gb="$(r5c_ssh "$n" "vgs k3s-vg --noheadings -o vg_free --units g | tr -d '<gG '")"
        echo "Node $n: k3s-vg free space ${vg_free_gb} GB (>= 40 GB) and lv_k3s OK"
    else
        if [[ "$CHECK_LVM" == "1" ]]; then
            echo "FAIL: Node $n has no k3s-vg volume group (CHECK_LVM=1 enforced)" >&2
            exit 1
        else
            echo "Node $n: k3s-vg not present yet (migration planned in Task 5; set CHECK_LVM=1 to enforce)"
        fi
    fi
done

echo "OK: Preflight checks passed."
