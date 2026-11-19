#!/usr/bin/env bash
# Runs 4KiB fsync benchmark on the baseline default storage class (local-path).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

echo "=== 1. Preparing benchmark image on physical nodes ==="
if ! docker image inspect "$R5C_BENCH_IMAGE" >/dev/null 2>&1; then
    docker build -t "$R5C_BENCH_IMAGE" -f "$HERE/Dockerfile.fio" "$HERE"
fi

for n in "${R5C_NODES[@]}"; do
    echo "Importing $R5C_BENCH_IMAGE into containerd on $n..."
    docker save "$R5C_BENCH_IMAGE" | r5c_ssh "$n" "k3s ctr images import -"
done

echo "=== 2. Creating namespace $R5C_BENCH_NS ==="
kubectl create namespace "$R5C_BENCH_NS" --dry-run=client -o yaml | kubectl apply -f -

cleanup() {
    echo "Cleaning up local-path benchmark resources..."
    kubectl -n "$R5C_BENCH_NS" delete pod fio-local --ignore-not-found --wait=true || true
    kubectl -n "$R5C_BENCH_NS" delete pvc fio-local-pvc --ignore-not-found --wait=true || true
}
trap cleanup EXIT

echo "=== 3. Deploying fio pod on local-path ==="
cat << 'MANIFEST' | kubectl apply -f -
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: fio-local-pvc
  namespace: ambar-bench
spec:
  accessModes: [ReadWriteOnce]
  storageClassName: local-path
  resources:
    requests:
      storage: 1Gi
---
apiVersion: v1
kind: Pod
metadata:
  name: fio-local
  namespace: ambar-bench
spec:
  restartPolicy: Never
  containers:
  - name: fio
    image: ambar-fio:local
    imagePullPolicy: Never
    command: ["fio"]
    args:
    - "--name=fsync-local"
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
      claimName: fio-local-pvc
MANIFEST

echo "Waiting for fio-local to complete benchmark (up to 120s)..."
kubectl -n "$R5C_BENCH_NS" wait --for=jsonpath='{.status.phase}'=Succeeded pod/fio-local --timeout=120s

echo "Extracting benchmark results..."
kubectl -n "$R5C_BENCH_NS" logs fio-local > "$HERE/baseline-local-path.json"

python3 -I -c '
import json, sys
data = json.load(open("stand/baseline-local-path.json"))
job = data["jobs"][0]
sync = job.get("sync", {})
lat_ns = sync.get("lat_ns", {})
clat_ns = job.get("write", {}).get("clat_ns", {})
mean_ms = lat_ns.get("mean", 0) / 1_000_000.0
p50_ms = lat_ns.get("percentile", {}).get("50.000000", 0) / 1_000_000.0
p95_ms = lat_ns.get("percentile", {}).get("95.000000", 0) / 1_000_000.0
p99_ms = lat_ns.get("percentile", {}).get("99.000000", 0) / 1_000_000.0
iops = job.get("write", {}).get("iops", 0)
print("=== Baseline local-path Results ===")
print(f"IOPS: {iops:.1f}")
print(f"fsync latency mean: {mean_ms:.3f} ms")
print(f"fsync latency p50:  {p50_ms:.3f} ms")
print(f"fsync latency p95:  {p95_ms:.3f} ms")
print(f"fsync latency p99:  {p99_ms:.3f} ms")
'
echo "OK: Baseline local-path benchmark completed."
