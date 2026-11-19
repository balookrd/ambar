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
    local ok=false
    for _ in $(seq 30); do
        rvs="$(kubectl get rv -A --no-headers 2>/dev/null || true)"
        if [[ -z "$rvs" ]]; then
            ok=true
            break
        fi
        sleep 2
    done
    if [[ "$ok" == "true" ]]; then
        echo "OK: ambar-bench namespace and all ReplicatedVolumes cleaned up."
    else
        echo "WARN: some ReplicatedVolumes still present after timeout:" >&2
        kubectl get rv -A || true
    fi
}

generate_report() {
    python3 -I - \
        "$HERE/baseline-local-path.json" \
        "$HERE/bench-fsync-r3.json" \
        "$HERE/bench-fsync-r2.json" \
        "$HERE/bench-load-results.json" << 'PYEOF'
import sys, json, os

def get_json(path):
    return json.load(open(path)) if os.path.exists(path) else None

local_path = sys.argv[1] if len(sys.argv) > 1 else "stand/baseline-local-path.json"
r3_path = sys.argv[2] if len(sys.argv) > 2 else "stand/bench-fsync-r3.json"
r2_path = sys.argv[3] if len(sys.argv) > 3 else "stand/bench-fsync-r2.json"
load_path = sys.argv[4] if len(sys.argv) > 4 else "stand/bench-load-results.json"

local = get_json(local_path)
r3 = get_json(r3_path)
r2 = get_json(r2_path)
load = get_json(load_path)

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
    max_cgroup_anon = max(v.get("ambar_cgroup_anon_mib", 0) for v in load.values())
    max_ps_rss = max(v.get("ambar_ps_rss_mib", v.get("ambar_pod_max_mib", 0)) for v in load.values())
    max_k3s_delta = max(v.get("k3s_delta_mib", 0) for v in load.values())
    print(f"| **cgroup anon подов ambar на узел** | 0 (в ядре) | **{max_cgroup_anon:.1f} МиБ** | - | - | **<= 64 МиБ** |")
    print(f"| **RSS процессов ambar (ps) на узел** | 0 (в ядре) | **{max_ps_rss:.1f} МиБ** | - | 145–348 МиБ | **<= 64 МиБ** |")
    print(f"| **Прирост памяти k3s-server** | 0 | **{max_k3s_delta:.1f} МиБ** | - | 180–450 МиБ | **<= 50 МиБ** |")
PYEOF
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
