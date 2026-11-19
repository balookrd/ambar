#!/usr/bin/env bash
# Measures 4KiB fdatasync latency on ambar-r3 (3 replicas) and ambar-r2 (2 replicas + witness).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

echo "=== Ensuring namespace $R5C_BENCH_NS ==="
kubectl create namespace "$R5C_BENCH_NS" --dry-run=client -o yaml | kubectl apply -f -

run_fio_bench() {
    local sc="$1"
    local output_file="$2"
    local pod_name="fio-${sc}"
    local pvc_name="pvc-${sc}"

    echo "--- Testing StorageClass $sc ---"
    kubectl -n "$R5C_BENCH_NS" delete pod "$pod_name" --ignore-not-found --wait=true >/dev/null 2>&1 || true
    kubectl -n "$R5C_BENCH_NS" delete pvc "$pvc_name" --ignore-not-found --wait=true >/dev/null 2>&1 || true

    cat << MANIFEST | kubectl apply -f -
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: ${pvc_name}
  namespace: ${R5C_BENCH_NS}
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: ${sc}
  resources:
    requests:
      storage: 1Gi
---
apiVersion: v1
kind: Pod
metadata:
  name: ${pod_name}
  namespace: ${R5C_BENCH_NS}
spec:
  restartPolicy: Never
  containers:
  - name: fio
    image: ${R5C_BENCH_IMAGE}
    imagePullPolicy: Never
    command: ["fio"]
    args:
    - "--name=fsync-${sc}"
    - "--filename=/data/test.dat"
    - "--size=64M"
    - "--bs=4k"
    - "--ioengine=sync"
    - "--fdatasync=1"
    - "--rw=write"
    - "--direct=0"
    - "--numjobs=1"
    - "--runtime=30"
    - "--time_based"
    - "--output-format=json"
    volumeMounts:
    - name: data
      mountPath: /data
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: ${pvc_name}
MANIFEST

    echo "Waiting for $pod_name to complete benchmark (up to 180s)..."
    kubectl -n "$R5C_BENCH_NS" wait --for=jsonpath='{.status.phase}'=Succeeded "pod/$pod_name" --timeout=180s

    echo "Extracting benchmark results..."
    kubectl -n "$R5C_BENCH_NS" logs "$pod_name" > "$output_file"

    local pv_name=""
    pv_name="$(kubectl -n "$R5C_BENCH_NS" get pvc "$pvc_name" -o jsonpath='{.spec.volumeName}' 2>/dev/null || true)"

    echo "Cleaning up $pod_name and $pvc_name..."
    kubectl -n "$R5C_BENCH_NS" delete "pod/$pod_name" --ignore-not-found --wait=true
    kubectl -n "$R5C_BENCH_NS" delete "pvc/$pvc_name" --ignore-not-found --wait=true

    if [[ -n "$pv_name" ]]; then
        echo "Waiting for ReplicatedVolume $pv_name to be completely removed..."
        for _ in $(seq 60); do
            if ! kubectl get rv "$pv_name" >/dev/null 2>&1; then
                break
            fi
            sleep 1
        done
    fi
}

echo "=== 1. Running ambar-r3 benchmark (3 replicas) ==="
run_fio_bench "ambar-r3" "$HERE/bench-fsync-r3.json"

echo "=== 2. Running ambar-r2 benchmark (2 replicas + witness) ==="
run_fio_bench "ambar-r2" "$HERE/bench-fsync-r2.json"

echo "=== 3. Summary Latency Comparison ==="
python3 -I - "$HERE/baseline-local-path.json" "$HERE/bench-fsync-r3.json" "$HERE/bench-fsync-r2.json" << 'PYEOF'
import json
import sys

def parse_fio(path):
    with open(path) as f:
        d = json.load(f)
    job = d["jobs"][0]
    sync = job.get("sync", {})
    lat = sync.get("lat_ns", {})
    pct = lat.get("percentile", {})
    iops = job.get("write", {}).get("iops", 0)
    return {
        "iops": iops,
        "mean_ms": lat.get("mean", 0) / 1_000_000.0,
        "p50_ms": pct.get("50.000000", 0) / 1_000_000.0,
        "p95_ms": pct.get("95.000000", 0) / 1_000_000.0,
        "p99_ms": pct.get("99.000000", 0) / 1_000_000.0,
    }

local = parse_fio(sys.argv[1])
r3 = parse_fio(sys.argv[2])
r2 = parse_fio(sys.argv[3])

print(f"{'StorageClass':<12} | {'IOPS':<8} | {'mean (ms)':<10} | {'p50 (ms)':<10} | {'p95 (ms)':<10} | {'p99 (ms)':<10}")
print("-" * 72)
print(f"{'local-path':<12} | {local['iops']:<8.1f} | {local['mean_ms']:<10.3f} | {local['p50_ms']:<10.3f} | {local['p95_ms']:<10.3f} | {local['p99_ms']:<10.3f}")
print(f"{'ambar-r3':<12} | {r3['iops']:<8.1f} | {r3['mean_ms']:<10.3f} | {r3['p50_ms']:<10.3f} | {r3['p95_ms']:<10.3f} | {r3['p99_ms']:<10.3f}")
print(f"{'ambar-r2':<12} | {r2['iops']:<8.1f} | {r2['mean_ms']:<10.3f} | {r2['p50_ms']:<10.3f} | {r2['p95_ms']:<10.3f} | {r2['p99_ms']:<10.3f}")

if r3["mean_ms"] > 5.0:
    print(f"\nWARNING: ambar-r3 mean latency {r3['mean_ms']:.3f} ms exceeds 5.0 ms threshold!")
else:
    print(f"\nPASS: ambar-r3 mean latency {r3['mean_ms']:.3f} ms is within <= 5.0 ms target!")
PYEOF

echo "OK: fsync benchmarks completed."
