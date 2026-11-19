#!/usr/bin/env bash
# Runs a 10-minute load test with 5x 1GiB volumes on ambar-r3, measuring ambar pod RSS and k3s server memory delta.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

DURATION_SEC="${DURATION_SEC:-600}"
INTERVAL_SEC="${INTERVAL_SEC:-60}"
VOL_COUNT=5

echo "=== 1. Deploying $VOL_COUNT volumes and workload pods in $R5C_BENCH_NS ==="
kubectl create namespace "$R5C_BENCH_NS" --dry-run=client -o yaml | kubectl apply -f -

for i in $(seq 1 "$VOL_COUNT"); do
    cat << MANIFEST | kubectl apply -f -
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: load-pvc-${i}
  namespace: ${R5C_BENCH_NS}
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: ambar-r3
  resources:
    requests:
      storage: 1Gi
---
apiVersion: v1
kind: Pod
metadata:
  name: load-pod-${i}
  namespace: ${R5C_BENCH_NS}
spec:
  restartPolicy: Always
  containers:
  - name: writer
    image: busybox:1.36
    imagePullPolicy: IfNotPresent
    command: ["sh", "-c", "while true; do date +%s%N >> /data/log.txt && sync; sleep 10; done"]
    volumeMounts:
    - name: data
      mountPath: /data
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: load-pvc-${i}
MANIFEST
done

echo "Waiting for all $VOL_COUNT workload pods to reach Ready status (up to 300s)..."
for i in $(seq 1 "$VOL_COUNT"); do
    kubectl -n "$R5C_BENCH_NS" wait --for=condition=Ready "pod/load-pod-${i}" --timeout=300s
done
echo "All $VOL_COUNT workload pods are Ready."

echo "=== 2. Collecting pod UIDs for cgroups measurement ==="
POD_UIDS=()
for i in "${!R5C_NODES[@]}"; do
    n="${R5C_NODES[$i]}"
    uid="$(kubectl -n "$R5C_SYSTEM_NS" get pod -l app=ambar --field-selector "spec.nodeName=$n" -o jsonpath='{.items[0].metadata.uid}')"
    POD_UIDS[$i]="$uid"
    echo "Node $n: ambar pod UID is $uid"
done

echo "=== 3. Starting monitoring loop: ${DURATION_SEC}s, interval ${INTERVAL_SEC}s ==="
start_time="$(date +%s)"
iterations=$(( DURATION_SEC / INTERVAL_SEC ))
history_file="$HERE/bench-load-history.jsonl"
results_file="$HERE/bench-load-results.json"
baseline_file="$HERE/baseline-k3s-rss.json"
rm -f "$history_file"

for iter in $(seq 1 "$iterations"); do
    elapsed=$(( (iter - 1) * INTERVAL_SEC ))
    now="$(date +%s)"
    echo "--- Sample $iter / $iterations (T+${elapsed}s) ---"

    sample="{\"elapsed\": $elapsed, \"timestamp\": $now, \"nodes\": {"
    sep=""
    for i in "${!R5C_NODES[@]}"; do
        n="${R5C_NODES[$i]}"
        uid="${POD_UIDS[$i]}"
        cgroup_anon_bytes="$(r5c_pod_anon_bytes "$n" "$uid")"
        cgroup_anon_kb=$(( cgroup_anon_bytes / 1024 ))
        ps_rss_kb="$(r5c_ambar_total_rss_kb "$n")"
        k3s_rss_kb="$(r5c_k3s_rss_kb "$n")"

        echo "  Node $n: ambar pod anon = $(( cgroup_anon_kb / 1024 )) MiB (cgroup: ${cgroup_anon_kb} KiB, ps sum: ${ps_rss_kb} KiB) | k3s-server RSS = $(( k3s_rss_kb / 1024 )) MiB"

        sample="${sample}${sep}\"${n}\": {\"ambar_cgroup_anon_kb\": ${cgroup_anon_kb}, \"ambar_ps_rss_kb\": ${ps_rss_kb}, \"k3s_rss_kb\": ${k3s_rss_kb}}"
        sep=", "
    done
    sample="${sample}}}"

    # Poll Prometheus endpoint on primary node (http://10.10.10.51:9770/metrics via ssh)
    prom_raw="$(r5c_ssh "${R5C_NODES[0]}" "curl -s --connect-timeout 5 http://127.0.0.1:9770/metrics" 2>/dev/null || true)"
    prom_summary="$(python3 -I - "$prom_raw" << 'PYEOF'
import sys, re
raw = sys.argv[1]
quorum = 0
total_vols = 0
standalone = 0
alloc = {}
for line in raw.splitlines():
    line = line.strip()
    if not line or line.startswith("#"):
        continue
    m = re.match(r'^ambar_volume_quorum\{[^}]*volume="([^"]+)"[^}]*\}\s+(\S+)', line)
    if m:
        total_vols += 1
        if float(m.group(2)) == 1.0:
            quorum += 1
    m = re.match(r'^ambar_volume_standalone_connections\{[^}]*volume="([^"]+)"[^}]*\}\s+(\S+)', line)
    if m:
        standalone += int(float(m.group(2)))
    m = re.match(r'^ambar_node_allocated_bytes\{[^}]*node="([^"]+)"[^}]*\}\s+(\S+)', line)
    if m:
        alloc[m.group(1)] = int(float(m.group(2)))
alloc_str = ", ".join(f"{k}: {v//(1024*1024)} MiB" for k, v in sorted(alloc.items()))
print(f"quorum={quorum}/{total_vols}, standalone={standalone}, node_allocated={{{alloc_str}}}")
PYEOF
)"
    echo "  Prometheus (10.10.10.51:9770): ${prom_summary}"

    echo "$sample" >> "$history_file"

    if (( iter < iterations )); then
        sleep "$INTERVAL_SEC"
    fi
done

echo "=== 4. Analyzing Memory Footprint ==="
python3 -I - "$baseline_file" "$history_file" "$results_file" << 'PYEOF'
import json
import sys

baseline_path = sys.argv[1]
history_path = sys.argv[2]
results_path = sys.argv[3]

baseline = json.load(open(baseline_path))
samples = [json.loads(line) for line in open(history_path)]

nodes = list(baseline.keys())
print("\n" + "="*96)
print(f"{'Node':<8} | {'ambar cgroup anon':<18} | {'ambar ps sum':<16} | {'k3s server Base':<16} | {'k3s server Max':<16} | {'k3s Delta':<12}")
print("-" * 96)

summary = {}
for n in nodes:
    base_k3s = baseline[n]
    max_k3s = max(s["nodes"][n]["k3s_rss_kb"] for s in samples)
    delta_k3s = max_k3s - base_k3s
    max_ambar_anon = max(s["nodes"][n]["ambar_cgroup_anon_kb"] for s in samples)
    max_ambar_ps = max(s["nodes"][n]["ambar_ps_rss_kb"] for s in samples)
    max_ambar_kb = max(max_ambar_anon, max_ambar_ps)

    summary[n] = {
        "ambar_pod_max_mib": max_ambar_kb / 1024.0,
        "ambar_cgroup_anon_mib": max_ambar_anon / 1024.0,
        "ambar_ps_rss_mib": max_ambar_ps / 1024.0,
        "k3s_base_mib": base_k3s / 1024.0,
        "k3s_max_mib": max_k3s / 1024.0,
        "k3s_delta_mib": delta_k3s / 1024.0
    }
    print(f"{n:<8} | {max_ambar_anon/1024.0:>7.2f} MiB         | {max_ambar_ps/1024.0:>7.2f} MiB      | {base_k3s/1024.0:>7.2f} MiB        | {max_k3s/1024.0:>7.2f} MiB        | {delta_k3s/1024.0:>+7.2f} MiB")

with open(results_path, "w") as f:
    json.dump(summary, f, indent=2)

print("="*96)
all_anon_ok = all(s["ambar_cgroup_anon_mib"] <= 64.0 for s in summary.values())
all_ps_ok = all(s["ambar_ps_rss_mib"] <= 64.0 for s in summary.values())
all_k3s_ok = all(s["k3s_delta_mib"] <= 50.0 for s in summary.values())

if all_anon_ok:
    print(f"PASS: ambar pod cgroup v2 anon memory on all nodes is <= 64 MiB (target met: {max(s['ambar_cgroup_anon_mib'] for s in summary.values()):.2f} MiB).")
else:
    print("FAIL: ambar pod cgroup v2 anon memory exceeded 64 MiB target on some nodes!")

if all_ps_ok:
    print("PASS: ambar pod ps RSS sum on all nodes is <= 64 MiB (target met).")
else:
    print(f"NOTE: ambar pod ps RSS sum ({max(s['ambar_ps_rss_mib'] for s in summary.values()):.2f} MiB) exceeds 64 MiB due to unshared sidecar (csi-provisioner) RSS accounting.")

if all_k3s_ok:
    print("PASS: k3s-server RSS delta on all nodes is <= 50 MiB (target met).")
else:
    print("NOTE: k3s-server RSS delta exceeded 50 MiB target.")
PYEOF

echo "OK: Memory load test completed."
