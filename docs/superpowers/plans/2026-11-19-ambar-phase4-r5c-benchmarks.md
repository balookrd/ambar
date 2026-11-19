# План реализации Фазы 4: Замеры на целевом железе (NanoPi R5C) и бенчмарк

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Провести комплексные замеры производительности и потребления ресурсов CSI-драйвера `ambar` на физическом боевом кластере k3s из трёх одноплатников NanoPi R5C (Debian 13 trixie, ядро `6.12.116+deb13-arm64`, NVMe 128 ГБ, 2.5 GbE интерконнект) по методике испытаний Longhorn от 2026-11-09 без нарушения работы существующих боевых сервисов.

**Architecture:** Набор модульных bash-скриптов в каталоге `stand/` (`r5c-lib.sh`, `r5c-preflight.sh`, `r5c-deploy.sh`, `r5c-bench-fsync.sh`, `r5c-bench-load.sh`, `bench-r5c.sh`), использующих SSH-доступ к физическим узлам и локальный `kubectl`. Образы поставляются напрямую в containerd узлов. Бенчмарк выполняется в изолированном namespace `ambar-bench`. Потребление памяти измеряется через cgroup v2 (`memory.stat anon`) и `ps -o rss=`, задержки `fsync` 4K измеряются через `fio` с JSON-выводом.

**Tech Stack:** Bash, Linux cgroup v2, DRBD 9.3.4, fio 3.33+, jq, Python 3, k3s v1.36.2+k3s1, containerd.

## Global Constraints

- Все диалоги, комментарии, отчеты и планы — строго на русском языке.
- Терминология: «носитель» / «carrier» (НИКОГДА «карьер»), «гейт» / «gate» (НИКОГДА «ворота»).
- Двуязычная документация: `README.md` и `README.ru.md` обновляются строго синхронно.
- Коммиты Git: только с явным перечислением файлов `git commit -m "..." -- <files>`, без ключей `-a` и `-A`.
- Безопасность боевого кластера: `local-path` остаётся дефолтным StorageClass, существующие namespaces (`home`, `smarthome`, `vpn` и др.) не затрагиваются.
- Изоляция сети репликации: DRBD репликация строго привязывается к интерфейсу `lan0` (`10.10.10.51..53`).
- Целевые пороги метрик:
  1. RSS подов `ambar` на узел: <= 64 МиБ.
  2. Прирост памяти `k3s server`: <= 50 МиБ.
  3. Задержка `fsync` 4 КиБ (`protocol: C`): <= 5.0 мс.

---

### Task 1: R5C Cluster Library and Preflight/Baseline Scripts

**Files:**
- Create: `stand/r5c-lib.sh`
- Create: `stand/Dockerfile.fio`
- Create: `stand/r5c-preflight.sh`
- Create: `stand/r5c-bench-local.sh`

**Interfaces:**
- Consumes: SSH `mmv@k3s-1..3`, локальный `kubectl`, NVMe `/var/lib/rancher`.
- Produces: функции `r5c_ssh`, `r5c_k3s_rss`, `r5c_cgroup_rss`, файлы `stand/baseline-k3s-rss.json` и `stand/baseline-local-path.json`.

- [x] **Step 1: Создать библиотеку функций физического кластера `stand/r5c-lib.sh`**

```bash
cat << 'EOF' > stand/r5c-lib.sh
#!/usr/bin/env bash
# Library of common functions and variables for NanoPi R5C cluster operations.
set -euo pipefail

R5C_NODES=(k3s-1 k3s-2 k3s-3)
R5C_IPS=("10.10.10.51" "10.10.10.52" "10.10.10.53")
R5C_USER="${R5C_USER:-mmv}"
R5C_SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new)
R5C_IMAGE="ambar-drbd:dev"
R5C_BENCH_IMAGE="ambar-fio:local"
R5C_BENCH_NS="ambar-bench"
R5C_SYSTEM_NS="ambar-system"

r5c_ssh() {
    local node="$1"
    shift
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${node}" "sudo bash -c $(printf '%q' "$*")"
}

r5c_ssh_user() {
    local node="$1"
    shift
    ssh "${R5C_SSH_OPTS[@]}" "${R5C_USER}@${node}" "$@"
}

r5c_k3s_rss_kb() {
    local node="$1"
    r5c_ssh "$node" "ps -o rss= -C k3s-server | awk '{s+=\$1} END {print s+0}'"
}

r5c_pod_anon_bytes() {
    local node="$1"
    local pod_uid="$2"
    local uid_underscore="${pod_uid//-/_}"
    r5c_ssh "$node" "
        path=\$(find /sys/fs/cgroup/kubepods.slice -name \"*${uid_underscore}*\" 2>/dev/null | head -n 1)
        if [[ -n \"\$path\" && -f \"\$path/memory.stat\" ]]; then
            awk '\$1 == \"anon\" {print \$2}' \"\$path/memory.stat\"
        else
            echo 0
        fi
    "
}

r5c_ambar_total_rss_kb() {
    local node="$1"
    r5c_ssh "$node" "ps -o rss= -C ambar,csi-provisioner,node-driver-reg | awk '{s+=\$1} END {print s+0}'"
}
EOF
chmod +x stand/r5c-lib.sh
```

- [x] **Step 2: Создать `stand/Dockerfile.fio` для автономного бенчмарк-контейнера**

```bash
cat << 'EOF' > stand/Dockerfile.fio
FROM alpine:3.20
RUN apk add --no-cache fio jq bash coreutils
WORKDIR /data
ENTRYPOINT ["fio"]
EOF
```

- [x] **Step 3: Создать скрипт preflight-проверки и baseline памяти `stand/r5c-preflight.sh`**

```bash
cat << 'EOF' > stand/r5c-preflight.sh
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
EOF
chmod +x stand/r5c-preflight.sh
```

- [x] **Step 4: Создать скрипт замера эталонного `fsync` на `local-path` (`stand/r5c-bench-local.sh`)**

```bash
cat << 'EOF' > stand/r5c-bench-local.sh
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
    - "--output=/data/fio-result.json"
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
kubectl -n "$R5C_BENCH_NS" exec fio-local -- cat /data/fio-result.json > "$HERE/baseline-local-path.json"

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
EOF
chmod +x stand/r5c-bench-local.sh
```

- [x] **Step 5: Запустить preflight и проверить выходные данные**

Run: `stand/r5c-preflight.sh`
Expected: Все 3 узла выдают статус ядра, свободное место на NVMe, lan0 IP и сохраняют `stand/baseline-k3s-rss.json`.

- [x] **Step 6: Запустить эталонный замер local-path**

Run: `stand/r5c-bench-local.sh`
Expected: Образ собран, загружен в containerd на 3 узла, fio pod завершается со статусом Succeeded, результаты выводятся и сохраняются в `stand/baseline-local-path.json`.

- [x] **Step 7: Commit**

```bash
git add stand/r5c-lib.sh stand/Dockerfile.fio stand/r5c-preflight.sh stand/r5c-bench-local.sh
git commit -m "feat(stand): add r5c preflight and baseline benchmark scripts" -- stand/r5c-lib.sh stand/Dockerfile.fio stand/r5c-preflight.sh stand/r5c-bench-local.sh
```

---

### Task 2: Deployment Script for ambar on R5C Physical Cluster

**Files:**
- Create: `stand/r5c-deploy.sh`

**Interfaces:**
- Consumes: `deploy/*`, `image/build.sh`, `stand/r5c-lib.sh`.
- Produces: работающий DaemonSet `ambar` в namespace `ambar-system`, StorageClasses `ambar-r3` и `ambar-r2`, регистрация в CSINode.

- [x] **Step 1: Создать скрипт развертывания `stand/r5c-deploy.sh`**

```bash
cat << 'EOF' > stand/r5c-deploy.sh
#!/usr/bin/env bash
# Deploys ambar CSI driver to the physical 3-node NanoPi R5C cluster.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HERE/.."
DEPLOY="$ROOT/deploy"
source "$HERE/r5c-lib.sh"

echo "=== 1. Ensuring host directories on physical nodes ==="
for n in "${R5C_NODES[@]}"; do
    echo "Creating /var/lib/rancher/ambar/{volumes,res} on $n..."
    r5c_ssh "$n" "install -d -m 0700 /var/lib/rancher/ambar/volumes && install -d -m 0755 /var/lib/rancher/ambar/res"
done

echo "=== 2. Building and distributing ambar image ==="
(cd "$ROOT/image" && ./build.sh)

for n in "${R5C_NODES[@]}"; do
    echo "Importing $R5C_IMAGE into containerd on $n..."
    docker save "$R5C_IMAGE" | r5c_ssh "$n" "k3s ctr images import -"
done

echo "=== 3. Applying Kubernetes core manifests ==="
kubectl apply -f "$DEPLOY/namespace.yaml"
kubectl apply -f "$DEPLOY/crd.json"
kubectl wait --for=condition=Established crd/replicatedvolumes.ambar.balookrd.github.io --timeout=60s
kubectl apply -f "$DEPLOY/csidriver.yaml"
kubectl apply -f "$DEPLOY/rbac.yaml"

echo "=== 4. Applying ambar-nodes ConfigMap ==="
cat << 'CM' | kubectl apply -f -
apiVersion: v1
kind: ConfigMap
metadata:
  name: ambar-nodes
  namespace: ambar-system
data:
  nodes: |
    [
      {"name": "k3s-1", "address": "10.10.10.51", "nodeId": 0, "capacityBytes": 107374182400},
      {"name": "k3s-2", "address": "10.10.10.52", "nodeId": 1, "capacityBytes": 107374182400},
      {"name": "k3s-3", "address": "10.10.10.53", "nodeId": 2, "capacityBytes": 107374182400}
    ]
CM

echo "=== 5. Applying DaemonSet and StorageClasses ==="
image_id="$(docker image inspect --format '{{.Id}}' "$R5C_IMAGE")"
sed -e "s|^\( *imagePullPolicy:\) AMBAR_IMAGE_PULL_POLICY\$|\1 Never|" \
    -e "s|^\( *image:\) AMBAR_IMAGE\$|\1 $R5C_IMAGE|" \
    "$DEPLOY/daemonset.yaml" \
    | awk -v id="$image_id" '
        { print }
        /^    metadata:$/ { print "      annotations:"; print "        ambar.balookrd.github.io/image-id: \"" id "\"" }' \
    | kubectl apply -f -

kubectl apply -f "$DEPLOY/storageclass.yaml"

echo "Waiting for ambar DaemonSet rollout (up to 300s)..."
kubectl -n "$R5C_SYSTEM_NS" rollout status ds/ambar --timeout=300s

echo "=== 6. Verifying CSINode driver registration ==="
for n in "${R5C_NODES[@]}"; do
    registered=0
    for _ in $(seq 30); do
        drivers="$(kubectl get csinode "$n" -o jsonpath='{.spec.drivers[*].name}' 2>/dev/null || true)"
        if [[ " $drivers " == *" ambar.balookrd.github.io "* ]]; then
            registered=1
            break
        fi
        sleep 2
    done
    if (( registered == 0 )); then
        echo "FAIL: Node $n has no ambar CSI driver registered (drivers: '$drivers')" >&2
        exit 1
    fi
    echo "Node $n registered ambar.balookrd.github.io OK"
done

echo "OK: ambar deployed successfully on all 3 R5C nodes."
EOF
chmod +x stand/r5c-deploy.sh
```

- [x] **Step 2: Запустить развертывание ambar на физическом кластере**

Run: `stand/r5c-deploy.sh`
Expected: Образ собран, загружен в containerd на k3s-1..3, применены манифесты, rollout ds/ambar завершен, driver зарегистрирован в CSINode на всех трех узлах.

- [x] **Step 3: Проверить статус подов и загрузку DRBD 9 в ядро**

Run: `kubectl -n ambar-system get pods -o wide`
Expected: 3 пода `ambar-...` в статусе Running 3/3, по одному на каждом физическом узле.

- [x] **Step 4: Commit**

```bash
git add stand/r5c-deploy.sh
git commit -m "feat(stand): add r5c ambar deployment script" -- stand/r5c-deploy.sh
```

---

### Task 3: 4KiB fsync Latency Benchmark Script (`stand/r5c-bench-fsync.sh`)

**Files:**
- Create: `stand/r5c-bench-fsync.sh`

**Interfaces:**
- Consumes: StorageClasses `ambar-r3`, `ambar-r2`, namespace `ambar-bench`, образ `ambar-fio:local`.
- Produces: JSON-отчеты `stand/bench-fsync-r3.json` и `stand/bench-fsync-r2.json`, задержки fdatasync.

- [x] **Step 1: Создать скрипт замера задержек fsync `stand/r5c-bench-fsync.sh`**

```bash
cat << 'EOF' > stand/r5c-bench-fsync.sh
#!/usr/bin/env bash
# Measures 4KiB fdatasync latency on ambar-r3 (3 replicas) and ambar-r2 (2 replicas + witness).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

kubectl create namespace "$R5C_BENCH_NS" --dry-run=client -o yaml | kubectl apply -f -

run_fio_bench() {
    local sc="$1"
    local output_file="$2"
    local pod_name="fio-${sc}"
    local pvc_name="pvc-${sc}"

    echo "--- Testing StorageClass $sc ---"
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
    - "--output=/data/fio-result.json"
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
    kubectl -n "$R5C_BENCH_NS" exec "$pod_name" -- cat /data/fio-result.json > "$output_file"

    echo "Cleaning up $pod_name and $pvc_name..."
    kubectl -n "$R5C_BENCH_NS" delete "pod/$pod_name" --wait=true
    kubectl -n "$R5C_BENCH_NS" delete "pvc/$pvc_name" --wait=true

    # Wait for RV object to be completely removed
    for _ in $(seq 30); do
        rv_count="$(kubectl get rv -A --no-headers 2>/dev/null | grep -c "$pvc_name" || true)"
        (( rv_count == 0 )) && break
        sleep 1
    done
}

echo "=== 1. Running ambar-r3 benchmark (3 replicas) ==="
run_fio_bench "ambar-r3" "$HERE/bench-fsync-r3.json"

echo "=== 2. Running ambar-r2 benchmark (2 replicas + witness) ==="
run_fio_bench "ambar-r2" "$HERE/bench-fsync-r2.json"

echo "=== 3. Summary Latency Comparison ==="
python3 -I -c '
import json

def parse_fio(path):
    d = json.load(open(path))
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

local = parse_fio("stand/baseline-local-path.json")
r3 = parse_fio("stand/bench-fsync-r3.json")
r2 = parse_fio("stand/bench-fsync-r2.json")

print(f"{'StorageClass':<12} | {'IOPS':<8} | {'mean (ms)':<10} | {'p50 (ms)':<10} | {'p95 (ms)':<10} | {'p99 (ms)':<10}")
print("-" * 72)
print(f"{'local-path':<12} | {local['iops']:<8.1f} | {local['mean_ms']:<10.3f} | {local['p50_ms']:<10.3f} | {local['p95_ms']:<10.3f} | {local['p99_ms']:<10.3f}")
print(f"{'ambar-r3':<12} | {r3['iops']:<8.1f} | {r3['mean_ms']:<10.3f} | {r3['p50_ms']:<10.3f} | {r3['p95_ms']:<10.3f} | {r3['p99_ms']:<10.3f}")
print(f"{'ambar-r2':<12} | {r2['iops']:<8.1f} | {r2['mean_ms']:<10.3f} | {r2['p50_ms']:<10.3f} | {r2['p95_ms']:<10.3f} | {r2['p99_ms']:<10.3f}")

if r3["mean_ms"] > 5.0:
    print(f"\nWARNING: ambar-r3 mean latency {r3['mean_ms']:.3f} ms exceeds 5.0 ms threshold!")
else:
    print(f"\nPASS: ambar-r3 mean latency {r3['mean_ms']:.3f} ms is within <= 5.0 ms target!")
'
echo "OK: fsync benchmarks completed."
EOF
chmod +x stand/r5c-bench-fsync.sh
```

- [x] **Step 2: Запустить замер задержек fsync на `ambar-r3` и `ambar-r2`**

Run: `stand/r5c-bench-fsync.sh`
Expected: Замеры для обоих StorageClass выполняются, формируется сравнительная таблица с `local-path`, средняя задержка `ambar-r3` выводится в миллисекундах.

- [x] **Step 3: Commit**

```bash
git add stand/r5c-bench-fsync.sh
git commit -m "feat(stand): add r5c fsync latency benchmark script" -- stand/r5c-bench-fsync.sh
```

---

### Task 4: 10-Minute Memory Load Test Script (`stand/r5c-bench-load.sh`)

**Files:**
- Create: `stand/r5c-bench-load.sh`

**Interfaces:**
- Consumes: StorageClass `ambar-r3`, namespace `ambar-bench`, `stand/r5c-lib.sh`, `stand/baseline-k3s-rss.json`.
- Produces: `stand/bench-load-results.json`, временные ряды RSS подов ambar и k3s server на всех узлах.

- [x] **Step 1: Создать скрипт 10-минутного нагрузочного теста `stand/r5c-bench-load.sh`**

```bash
cat << 'EOF' > stand/r5c-bench-load.sh
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

echo "Waiting for all $VOL_COUNT workload pods to reach Running status (up to 300s)..."
for i in $(seq 1 "$VOL_COUNT"); do
    kubectl -n "$R5C_BENCH_NS" wait --for=condition=Ready "pod/load-pod-${i}" --timeout=300s
done
echo "All $VOL_COUNT workload pods are Running."

echo "=== 2. Collecting pod UIDs for cgroups measurement ==="
declare -A POD_UIDS
for n in "${R5C_NODES[@]}"; do
    uid="$(kubectl -n "$R5C_SYSTEM_NS" get pod -l app=ambar --field-selector "spec.nodeName=$n" -o jsonpath='{.items[0].metadata.uid}')"
    POD_UIDS["$n"]="$uid"
    echo "Node $n: ambar pod UID is $uid"
done

echo "=== 3. Starting monitoring loop: ${DURATION_SEC}s, interval ${INTERVAL_SEC}s ==="
start_time="$(date +%s)"
iterations=$(( DURATION_SEC / INTERVAL_SEC ))
history_file="$HERE/bench-load-history.jsonl"
rm -f "$history_file"

for iter in $(seq 1 "$iterations"); do
    elapsed=$(( (iter - 1) * INTERVAL_SEC ))
    now="$(date +%s)"
    echo "--- Sample $iter / $iterations (T+${elapsed}s) ---"

    sample="{\"elapsed\": $elapsed, \"timestamp\": $now, \"nodes\": {"
    sep=""
    for n in "${R5C_NODES[@]}"; do
        uid="${POD_UIDS[$n]}"
        cgroup_anon_bytes="$(r5c_pod_anon_bytes "$n" "$uid")"
        cgroup_anon_kb=$(( cgroup_anon_bytes / 1024 ))
        ps_rss_kb="$(r5c_ambar_total_rss_kb "$n")"
        k3s_rss_kb="$(r5c_k3s_rss_kb "$n")"

        echo "  Node $n: ambar pod RSS = $(( cgroup_anon_kb / 1024 )) MiB (cgroup: ${cgroup_anon_kb} KiB, ps: ${ps_rss_kb} KiB) | k3s-server RSS = $(( k3s_rss_kb / 1024 )) MiB"

        sample="${sample}${sep}\"${n}\": {\"ambar_cgroup_anon_kb\": ${cgroup_anon_kb}, \"ambar_ps_rss_kb\": ${ps_rss_kb}, \"k3s_rss_kb\": ${k3s_rss_kb}}"
        sep=", "
    done
    sample="${sample}}}"
    echo "$sample" >> "$history_file"

    if (( iter < iterations )); then
        sleep "$INTERVAL_SEC"
    fi
done

echo "=== 4. Analyzing Memory Footprint ==="
python3 -I -c '
import json

baseline = json.load(open("stand/baseline-k3s-rss.json"))
samples = [json.loads(line) for line in open("stand/bench-load-history.jsonl")]

nodes = list(baseline.keys())
print("\n" + "="*80)
print(f"{'Node':<8} | {'ambar Pod RSS (Max)':<22} | {'k3s server Base':<16} | {'k3s server Max':<16} | {'k3s Delta':<12}")
print("-" * 80)

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
        "k3s_base_mib": base_k3s / 1024.0,
        "k3s_max_mib": max_k3s / 1024.0,
        "k3s_delta_mib": delta_k3s / 1024.0
    }
    print(f"{n:<8} | {max_ambar_kb/1024.0:>7.2f} MiB             | {base_k3s/1024.0:>7.2f} MiB        | {max_k3s/1024.0:>7.2f} MiB        | {delta_k3s/1024.0:>+7.2f} MiB")

with open("stand/bench-load-results.json", "w") as f:
    json.dump(summary, f, indent=2)

print("="*80)
all_ambar_ok = all(s["ambar_pod_max_mib"] <= 64.0 for s in summary.values())
all_k3s_ok = all(s["k3s_delta_mib"] <= 50.0 for s in summary.values())

if all_ambar_ok:
    print("PASS: ambar pod RSS on all nodes is <= 64 MiB (target met).")
else:
    print("FAIL: ambar pod RSS exceeded 64 MiB target on some nodes!")

if all_k3s_ok:
    print("PASS: k3s-server RSS delta on all nodes is <= 50 MiB (target met).")
else:
    print("NOTE: k3s-server RSS delta exceeded 50 MiB target.")
'
echo "OK: Memory load test completed."
EOF
chmod +x stand/r5c-bench-load.sh
```

- [x] **Step 2: Запустить 10-минутный нагрузочный тест памяти**

Run: `stand/r5c-bench-load.sh`
Expected: 5 томов создаются, поды запускаются, 10 минут собираются сэмплы памяти, результаты выводятся и сохраняются в `stand/bench-load-results.json`.

- [x] **Step 3: Commit**

```bash
git add stand/r5c-bench-load.sh
git commit -m "feat(stand): add r5c 10-minute memory load benchmark script" -- stand/r5c-bench-load.sh
```

---

### Task 5: Master Orchestrator and Safe Cleanup Script (`stand/bench-r5c.sh`)

**Files:**
- Create: `stand/bench-r5c.sh`

**Interfaces:**
- Consumes: `stand/r5c-preflight.sh`, `stand/r5c-bench-local.sh`, `stand/r5c-deploy.sh`, `stand/r5c-bench-fsync.sh`, `stand/r5c-bench-load.sh`.
- Produces: единая CLI-точка входа для запуска бенчмарка и очистки ресурсов.

- [ ] **Step 1: Создать скрипт-оркестратор `stand/bench-r5c.sh`**

```bash
cat << 'EOF' > stand/bench-r5c.sh
#!/usr/bin/env bash
# Master orchestrator for ambar benchmarks on NanoPi R5C physical cluster.
# Usage:
#   stand/bench-r5c.sh preflight   - check cluster prerequisites and record baseline
#   stand/bench-r5c.sh deploy      - deploy ambar CSI driver to cluster
#   stand/bench-r5c.sh fsync       - run 4KiB fsync latency benchmark
#   stand/bench-r5c.sh load        - run 10-minute 5-volume memory load test
#   stand/bench-r5c.sh clean       - clean up ambar-bench namespace and test volumes
#   stand/bench-r5c.sh report      - print summary markdown report
#   stand/bench-r5c.sh all         - run complete benchmark pipeline end-to-end
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/r5c-lib.sh"

clean_bench() {
    echo "=== Cleaning up ambar-bench namespace ==="
    kubectl delete namespace "$R5C_BENCH_NS" --ignore-not-found --wait=true || true
    echo "Waiting for ReplicatedVolumes to be deleted..."
    for _ in $(seq 30); do
        rvs="$(kubectl get rv -A --no-headers 2>/dev/null || true)"
        [[ -z "$rvs" ]] && break
        sleep 2
    done
    echo "OK: ambar-bench namespace cleaned up."
}

generate_report() {
    python3 -I -c '
import json, os

def get_json(path):
    return json.load(open(path)) if os.path.exists(path) else None

local = get_json("stand/baseline-local-path.json")
r3 = get_json("stand/bench-fsync-r3.json")
r2 = get_json("stand/bench-fsync-r2.json")
load = get_json("stand/bench-load-results.json")

def parse_fio(d):
    if not d: return {"iops": 0, "mean": 0, "p50": 0, "p95": 0, "p99": 0}
    job = d["jobs"][0]
    lat = job.get("sync", {}).get("lat_ns", {})
    pct = lat.get("percentile", {})
    return {
        "iops": job.get("write", {}).get("iops", 0),
        "mean": lat.get("mean", 0) / 1_000_000.0,
        "p50": pct.get("50.000000", 0) / 1_000_000.0,
        "p95": pct.get("95.000000", 0) / 1_000_000.0,
        "p99": pct.get("99.000000", 0) / 1_000_000.0,
    }

f_loc = parse_fio(local)
f_r3 = parse_fio(r3)
f_r2 = parse_fio(r2)

print("## Сводная таблица результатов замеров на NanoPi R5C\n")
print("| Метрика / Тест | `local-path` (baseline) | `ambar-r3` (3 replica) | `ambar-r2` (2 rep + witness) | Longhorn 1.12.2 (2026-11-09) | Цель ambar |")
print("|---|---|---|---|---|---|")
print(f"| **Задержка fsync 4K (средняя)** | {f_loc['mean']:.2f} мс | **{f_r3['mean']:.2f} мс** | {f_r2['mean']:.2f} мс | 13.6 мс | **<= 5.0 мс** |")
print(f"| **Задержка fsync 4K (p50)** | {f_loc['p50']:.2f} мс | {f_r3['p50']:.2f} мс | {f_r2['p50']:.2f} мс | 12.1 мс | - |")
print(f"| **Задержка fsync 4K (p95)** | {f_loc['p95']:.2f} мс | {f_r3['p95']:.2f} мс | {f_r2['p95']:.2f} мс | 18.4 мс | - |")
print(f"| **Задержка fsync 4K (p99)** | {f_loc['p99']:.2f} мс | {f_r3['p99']:.2f} мс | {f_r2['p99']:.2f} мс | 24.8 мс | - |")
print(f"| **IOPS fsync 4K** | {f_loc['iops']:.0f} | {f_r3['iops']:.0f} | {f_r2['iops']:.0f} | 73 | - |")

if load:
    max_pod_rss = max(v["ambar_pod_max_mib"] for v in load.values())
    max_k3s_delta = max(v["k3s_delta_mib"] for v in load.values())
    print(f"| **RSS подов драйвера на узел** | 0 (в ядре) | **{max_pod_rss:.1f} МиБ** | - | 145–348 МиБ | **<= 64 МиБ** |")
    print(f"| **Прирост памяти k3s-server** | 0 | **{max_k3s_delta:.1f} МиБ** | - | 180–450 МиБ | **<= 50 МиБ** |")
'
}

case "${1:-all}" in
    preflight)
        "$HERE/r5c-preflight.sh"
        "$HERE/r5c-bench-local.sh"
        ;;
    deploy)
        "$HERE/r5c-deploy.sh"
        ;;
    fsync)
        "$HERE/r5c-bench-fsync.sh"
        ;;
    load)
        "$HERE/r5c-bench-load.sh"
        ;;
    clean)
        clean_bench
        ;;
    report)
        generate_report
        ;;
    all)
        "$HERE/r5c-preflight.sh"
        "$HERE/r5c-bench-local.sh"
        "$HERE/r5c-deploy.sh"
        "$HERE/r5c-bench-fsync.sh"
        "$HERE/r5c-bench-load.sh"
        clean_bench
        generate_report
        ;;
    *)
        echo "Usage: $0 [preflight|deploy|fsync|load|clean|report|all]" >&2
        exit 1
        ;;
esac
EOF
chmod +x stand/bench-r5c.sh
```

- [ ] **Step 2: Проверить синтаксис скрипта orchestrator**

Run: `bash -n stand/bench-r5c.sh`
Expected: Успешный выход без синтаксических ошибок.

- [ ] **Step 3: Commit**

```bash
git add stand/bench-r5c.sh
git commit -m "feat(stand): add r5c benchmark master orchestrator script" -- stand/bench-r5c.sh
```

---

### Task 6: Execution of R5C Benchmark and Results Protocol Creation

**Files:**
- Create: `docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`

**Interfaces:**
- Consumes: результаты работы `stand/bench-r5c.sh all`.
- Produces: утвержденный протокол замеров `2026-11-19-ambar-phase4-r5c-results.md`.

- [ ] **Step 1: Выполнить полный цикл замеров**

Run: `stand/bench-r5c.sh all`
Expected: Все этапы (preflight, baseline local-path, deploy ambar, fsync ambar-r3 & r2, load 10m, clean, report) проходят успешно.

- [ ] **Step 2: Зафиксировать протокол результатов в `docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`**

Создать документ с описанием стенда, методики, сырых данных и сравнительной таблицы с Longhorn и local-path.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md stand/baseline-*.json stand/bench-*.json*
git commit -m "docs(spec): add phase 4 r5c benchmark results protocol" -- docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md stand/baseline-*.json stand/bench-*.json*
```

---

### Task 7: Documentation Synchronization and Memory Update

**Files:**
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `docs/superpowers/specs/2026-11-12-ambar-design.md`
- Modify: `~/.claude/projects/-Users-mvmalykh-IdeaProjects-ambar/memory/MEMORY.md`

**Interfaces:**
- Consumes: фактические данные из протокола замеров `2026-11-19-ambar-phase4-r5c-results.md`.
- Produces: обновленная синхронная двуязычная документация.

- [ ] **Step 1: Обновить `README.md` и `README.ru.md` с фактическими цифрами замеров**

Заполнить итоговую таблицу производительности и потребления памяти фактическими данными, полученными на кластере NanoPi R5C.

- [ ] **Step 2: Обновить архитектурную спецификацию `docs/superpowers/specs/2026-11-12-ambar-design.md`**

Отметить успешное прохождение всех критериев Фазы 4.

- [ ] **Step 3: Обновить память проекта Claude `~/.claude/projects/-Users-mvmalykh-IdeaProjects-ambar/memory/MEMORY.md`**

Зафиксировать статус Фазы 4, конфигурацию R5C и результаты бенчмарков.

- [ ] **Step 4: Прогнать тесты и локальный гейт качества**

Run: `cargo test --locked && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings`
Expected: Все юнит- и интеграционные тесты проходят, гейт качества зелёный.

- [ ] **Step 5: Commit**

```bash
git add README.md README.ru.md docs/superpowers/specs/2026-11-12-ambar-design.md
git commit -m "docs: record phase 4 physical hardware benchmark results" -- README.md README.ru.md docs/superpowers/specs/2026-11-12-ambar-design.md
```
