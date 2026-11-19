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
echo "OK: Preflight checks passed."
