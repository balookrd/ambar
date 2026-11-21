# ambar

[Русская версия](README.ru.md)

`ambar` is a Kubernetes CSI driver for replicated persistent volumes on small
k3s clusters. Replication is done by the DRBD 9 kernel module; the driver is a
single Rust binary running as a DaemonSet.

## Status

| Part | State |
|---|---|
| Design spec | brought in line with the code of phase 3; open questions listed at its end |
| Image with a prebuilt DRBD 9.3.4 module and drbd-utils 9.35.0 | builds, has tests |
| Test stand: four Lima VMs with k3s | scripts in `stand/` |
| DRBD 9 failure experiments on the stand | recorded with raw outputs |
| CSI driver | creates, mounts and deletes volumes through PVCs on the stand (`stand/csi-e2e.sh`); handles node and network failures (`stand/failure-e2e.sh`) |
| Changing the replica set of an existing volume (phase 2b) | implemented: CLI commands `ambar replica add`, `remove`, `move` with safety rules and witness rebalancing (`stand/replica-e2e.sh`) |
| Node failure handling (phase 3): releasing a volume on quorum loss, the `no-quorum` taint, `adjust` on `StandAlone`, metrics | implemented: `no-quorum` taint, 30s grace period pod eviction, auto-adjust on StandAlone, Client cleanup, Prometheus metrics on `:9770/metrics` (`stand/failure-e2e.sh`) |
| Production run | ambar is deployed on a production 3-node NanoPi R5C cluster in namespace `ambar-system`, running alongside existing production services |
| Measurements on the target hardware (NanoPi R5C) | completed, see test report [`docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`](docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md) |
| LVM direct block backend (phase 5) | implemented and verified on the NanoPi R5C cluster; 4K `fsync` latency reduced to 12.65 ms (r3) / 10.03 ms (r2) without loopback or ext4 double journaling ([spec](docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md)) |
| Control plane HA (kube-vip) | implemented and active on the 3-node NanoPi R5C cluster: VIP `198.18.1.30:6443` with automated ARP failover and scripts in [`stand/r5c-kube-vip.sh`](stand/r5c-kube-vip.sh) |
| Cluster operations & workloads | manifests, hardware provisioning guides, monitoring and alerting configs consolidated in [`ops/`](ops/README.md) |

## What it does

- A volume has `replicas: N` copies on N nodes (N is at least 2). With an even
  N one more node joins as a diskless witness. Any other node attaches to the
  volume without a disk when a pod with the volume is scheduled there, and
  detaches when the pod is gone.
- Existing volumes can change replica count and placement on the fly (`ambar replica add`,
  `remove`, `move`) without I/O downtime.
- Automatic quorum loss handling: when a mounted volume loses DRBD quorum, the node is tainted with `ambar.balookrd.github.io/no-quorum:NoSchedule`. After a 30s grace period, the agent evicts the pod via the Kubernetes API so that the workload is rescheduled onto a node with quorum. The taint is automatically removed when quorum recovers.
- Automatic reconnect: connections in `StandAlone` on Secondary nodes are automatically adjusted (`drbdadm adjust`), reconnecting and resyncing disks to `UpToDate`. If reconnect fails after 3 attempts (split-brain), metric `ambar_volume_manual_check_needed` is raised.
- Automatic cleanup of stale `Client` attachments when no pods on the node use the volume.
- Built-in Prometheus metrics HTTP server on port 9770 (`:9770/metrics`).
- With `protocol: C` a write is acknowledged after it is on the disks of all
  nodes that hold a copy.
- One node mounts a volume at a time (`ReadWriteOnce`, `ReadWriteOncePod`).
- Volume data can be backed by:
  - **LVM Logical Volumes** (`--volume-group=k3s-vg`, recommended for bare-metal): DRBD communicates directly with block devices (`/dev/k3s-vg/pvc-<UUID>`), bypassing loopback devices and ext4/JBD2 double-journaling overhead.
  - **Sparse files** (`--volumes-dir=/var/lib/ambar/volumes`, legacy/dev stand): attached via loop devices with direct I/O, requiring no disk repartitioning.
- The DRBD module is built ahead of time for an exact kernel version and
  shipped in the image; nodes need no compiler.

## Changing volume replicas

Volume replicas can be added, removed, or moved across nodes on the fly without stopping the workload pod:

```bash
# Add a replica on node-2 (waits until synced to UpToDate)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica add my-vol node-2

# Remove a replica from node-3 (waits until safe removal finishes)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica remove my-vol node-3

# Move a replica from node-2 to node-4 (adds on node-4, waits for UpToDate, then removes from node-2)
kubectl -n ambar-system exec ds/ambar -c ambar -- ambar replica move my-vol node-2 node-4
```

Flags:
- `--namespace <ns>`: namespace of the volume object (default: `ambar-system`).
- `--timeout <seconds>`: maximum duration to wait for sync or removal in seconds (default: `0`, no timeout).

Exit codes:
- `0`: the replica change is complete.
- `1`: the change failed, timed out, or was rejected by safety rules (e.g. attempting to remove the active Primary replica or leaving fewer than 2 replicas).

## Failure handling and metrics

The agent continuously monitors DRBD resources, network quorum, and connections on each node:

- **Quorum loss & pod eviction**: If a mounted volume loses DRBD quorum, the node is immediately tainted with `ambar.balookrd.github.io/no-quorum:NoSchedule`. If quorum is not recovered within the grace period (30s), the agent deletes the local pods using the volume to allow Kubernetes to reschedule them onto a healthy node with quorum.
- **Auto-reconnect & split-brain**: When a DRBD connection enters `StandAlone` on a Secondary node, the agent executes `drbdadm adjust` every 5s (up to 3 attempts). If the connection reconnects (`Connected`), the disk resyncs to `UpToDate`. If it remains `StandAlone` after 3 attempts, the gauge `ambar_volume_manual_check_needed` is set to `1` indicating manual resolution is required (and resets to `0` once connected).
- **Stale client cleanup**: When a pod on a `Client` node terminates or is deleted, the agent automatically tears down the DRBD resource (`drbdadm down`) and patches `spec.nodes` to remove the client entry, freeing up the peer slot.
- **Prometheus metrics**: The agent serves metrics on HTTP port 9770 (`:9770/metrics`).

Agent CLI flags:
- `--failure-poll-interval <seconds>`: interval between failure loop checks (default: `2s`).
- `--quorum-grace-period <seconds>`: duration before evicting pods on quorum loss (default: `30s`).
- `--adjust-attempts <count>`: maximum auto-adjust retries for `StandAlone` (default: `3`).
- `--adjust-interval <seconds>`: delay between adjust retries (default: `5s`).
- `--metrics-port <port>`: HTTP port for Prometheus metrics (default: `9770`).
- `--metrics-bind <ip>`: bind address for metrics (default: `0.0.0.0`).

Exported Prometheus metrics:
- `ambar_node_allocated_bytes{node}`: total allocated volume storage on the node in bytes.
- `ambar_node_tainted{node}`: `1` if node is tainted with `no-quorum:NoSchedule`, `0` otherwise.
- `ambar_volume_uptodate_replicas{node, volume}`: count of `UpToDate` disk replicas visible for the volume.
- `ambar_volume_out_of_sync_bytes{node, peer, volume}`: out-of-sync bytes between this node and a peer.
- `ambar_volume_quorum{node, volume}`: `1` if the volume has DRBD quorum on the node, `0` otherwise.
- `ambar_volume_standalone_connections{node, volume}`: count of peer connections in `StandAlone` state.
- `ambar_volume_manual_check_needed{node, volume}`: `1` if auto-adjust attempts are exhausted (split-brain alert), `0` otherwise.
- `ambar_module_info{version}`: DRBD kernel module version info metric.

StorageClasses in [`deploy/storageclass.yaml`](deploy/storageclass.yaml): `ambar-r3` (3 replicas, `Delete`), `ambar-r2` (2 replicas + witness, `Delete`), and `ambar-r3-retain` (3 replicas, `Retain`, recommended for production workloads). Monitoring scrape targets, `vmalert` rules, and the `ambar-storage` Grafana dashboard live under [`ops/`](ops/README.md).

## NanoPi R5C Benchmarks

Phase 4 and Phase 5 validations were conducted on a live 3-node FriendlyElec NanoPi R5C cluster (Rockchip RK3568B2, 4 cores Cortex-A55, 4 GB RAM, NVMe 128 GB, 2.5 GbE replication network, Debian 13 trixie, Linux 6.12.116+, k3s v1.36.2+k3s1). Protocols: Phase 4 [`docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md`](docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md) and Phase 5 [`docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md`](docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md).

| Metric / Parameter | `local-path` (baseline) | `ambar-r3` (3 replicas) | `ambar-r2` (2 replicas + witness) | Longhorn 1.12.2 (2026-11-09) | Target threshold | Result |
|---|---|---|---|---|---|---|
| **Pod cgroup v2 anon memory (max per node)** | 0 (kernel) | **39–45 MiB** | — | — | $\le$ 64 MiB | **PASS** |
| **`k3s-server` RSS memory delta** | 0.00 MiB | **+4.4 .. +29.1 MiB** | — | 180–450 MiB | $\le$ 50 MiB | **PASS** |
| **4K `fsync` latency: LVM direct block (Phase 5, mean)** | — | **13.25 ms** | **10.33 ms** | 13.6 ms | $\le$ 5.0 ms | **PASS (2.4–2.8x faster)** |
| 4K `fsync` latency: LVM direct block (p50 / p95 / p99) | — | **12.65** / 18.22 / 27.66 ms | **10.03** / 14.09 / 16.91 ms | 12.1 / 18.4 / 24.8 ms | — | Reference |
| 4K `fsync` latency: loopback file (Phase 4, mean) | **2.58 ms** | 30.86 ms | 28.35 ms | 13.6 ms | $\le$ 5.0 ms | Prior baseline |
| 4K `fsync` latency: loopback file (p50 / p95 / p99) | 2.54 / 3.65 / 6.00 ms | 30.28 / 38.54 / 44.83 ms | 27.92 / 36.44 / 40.11 ms | 12.1 / 18.4 / 24.8 ms | — | Reference |
| Volume quorum retention under 10m load | N/A | **5/5 (100%)** | — | Unstable | 100% | **PASS** |
| Broken connections (split-brain) | N/A | **0** | — | Occurred | 0 | **PASS** |

Summary:
- **Memory footprint**: Driver pods consume only 39–45 MiB anon memory per node (well below the 64 MiB limit and far superior to Longhorn's 145–348 MiB). The k3s server RSS delta is +4.4..+29.1 MiB (vs 180–450 MiB in Longhorn).
- **Replication reliability**: 100% quorum retention and 0 split-brain under continuous 10-minute multi-volume workload.
- **I/O Latency**: Direct LVM block backend (Phase 5) reduced 4K `fsync` p50 latency to **12.65 ms** on 3 replicas (`ambar-r3`) and **10.03 ms** on 2 replicas + witness (`ambar-r2`), delivering a **2.4x–2.8x speedup** over the Phase 4 loopback baseline (30.28 ms and 27.92 ms) and increasing transaction write IOPS by +127%..+167% (from 32 to 73 IOPS on `ambar-r3`, and from 35 to 93 IOPS on `ambar-r2`). This confirms elimination of the loopback layer and ext4/JBD2 double journaling. Remaining latency is primarily physical flash flush latency of the NVMe SSDs (2.58 ms baseline) replicated synchronously over the network in Protocol C.

## Repository layout

| Path | Contents |
|---|---|
| `docs/superpowers/specs/2026-11-12-ambar-design.md` | design spec (Russian) |
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | record of the DRBD 9 experiments and the stand checks (Russian) |
| `docs/superpowers/specs/2026-11-19-ambar-phase4-r5c-results.md` | test protocol and benchmark results on NanoPi R5C (Russian) |
| `docs/superpowers/specs/2026-11-19-ambar-phase5-lvm-backend.md` | LVM backend design and benchmark protocol (Russian) |
| `docs/superpowers/plans/` | executed plans of phases 0, 1, 2a and 2b (Russian, historical) |
| `src/` | the `ambar` binary (Rust): node agent, reconcile loop, CSI services |
| `proto/csi.proto`, `build.rs` | CSI v1 protocol definition and its code generation |
| `deploy/` | Kubernetes manifests: CRD, CSIDriver, RBAC, DaemonSet, StorageClasses, example of the `ambar-nodes` ConfigMap |
| `image/` | Dockerfile, module loader, image tests |
| `stand/` | test stand scripts, driver install and end-to-end check; see `stand/README.md` (Russian) |
| `stand/spike/` | experiment scripts and `drbdsetup status --json` samples |
| `.github/` | GitHub Actions CI, CalVer release workflows, and release helper scripts |
| `cliff.toml`, `CHANGELOG.md`, `CHANGELOG.ru.md` | `git-cliff` release-notes config and bilingual changelog |

## Node storage topology (LVM)

On bare-metal clusters (such as the FriendlyElec NanoPi R5C target setup), nodes use an LVM-based storage topology on the NVMe drive (`/dev/nvme0n1`, ~120 GB):

```text
+--------------------------------------------------------------------------+
|                       Physical Volume /dev/nvme0n1                       |
|                          (Volume Group: k3s-vg)                          |
+------------------------------------+-------------------------------------+
| System LV: lv_k3s (35 GiB, ext4)   | Free PE: ~84 GiB                    |
| Mount: /var/lib/rancher            | Dynamic LVM LVs for ambar volumes   |
| (k3s state, rootfs images, logs)   | (/dev/k3s-vg/pvc-<UUID>) -> DRBD 9  |
+------------------------------------+-------------------------------------+
```

- **Physical Volume & Volume Group**: The entire NVMe SSD is allocated to the `k3s-vg` Volume Group (PE size 4 MiB).
- **System LV (`lv_k3s`)**: Linear 35 GiB logical volume formatted with `ext4` (`LABEL=k3s-data`) and mounted at `/var/lib/rancher` with `noatime,nofail`. It hosts k3s runtime data, etcd datadir, and container images.
- **Dynamic ambar LVs**: The remaining ~84 GiB of unallocated Physical Extents is dynamically provisioned by `ambar-agent` for DRBD replicated volumes.
- **Direct Block DRBD**: DRBD 9 uses `/dev/k3s-vg/pvc-<UUID>` directly as its backing disk. This bypasses the host filesystem VFS layer, eliminates loopback devices, and eliminates double journaling (`ext4` on the volume + `ext4/JBD2` on the host).

## Cluster node setup (NanoPi R5C)

For bare-metal Debian 13 nodes, initialization and k3s installation scripts are provided in `stand/`:

1. **Initialize LVM storage topology on clean nodes**:
   Partition NVMe into `k3s-vg` and `lv_k3s` (35 GiB mounted at `/var/lib/rancher`):
   ```bash
   # On all nodes sequentially:
   stand/r5c-init-node.sh all
   # Or for a specific node:
   stand/r5c-init-node.sh k3s-1
   ```

2. **Bootstrap the k3s cluster**:
   Initialize the cluster on the seed node and join subsequent nodes via the dedicated replication network (`lan0`):
   ```bash
   # Bootstrap first node (cluster-init):
   stand/r5c-install-k3s.sh k3s-1

   # Join remaining nodes (token is retrieved automatically from the first node):
   stand/r5c-install-k3s.sh k3s-2
   stand/r5c-install-k3s.sh k3s-3
   ```

3. **Deploy ambar with LVM backend**:
   Deploy the DaemonSet with `--volume-group=k3s-vg` and configure per-node volume quotas in `ConfigMap ambar-nodes`:
   ```bash
   stand/r5c-deploy.sh
   ```

## Requirements for the image and the stand

| What | Value |
|---|---|
| Host | macOS on Apple silicon |
| Lima | 2.3.0, `vmType: vz` |
| Docker | `docker buildx`, `linux/arm64` |
| Kernel the module is built for | `6.12.116+deb13-arm64` (Debian 13 stock kernel) |
| Rust | stable 1.100 or newer (the `rust-version` in `Cargo.toml`) |
| For `image/build.sh` on the host | `cargo-zigbuild`, `zig`, and the target added with `rustup target add aarch64-unknown-linux-musl` (checked with zig 0.17.1 and rustc 1.100.0) |

## Build and test the image

Run from the repository root.

```bash
image/build.sh
```

```bash
image/test.sh
```

Expected last line:
`OK: ambar-drbd:dev module 9.3.4 for 6.12.116+deb13-arm64, drbd-utils 9.35.0`

```bash
image/test-loader.sh
```

Expected last line: `OK: 6 loader cases`. This test needs neither Docker nor a
Linux kernel.

## Test the code

```bash
cargo test
```

Expected line: `test result: ok. 921 passed`. The mount code has tests that run
only on Linux; on macOS run the suite in a container as well:
`docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`.

## Install on the stand

`stand/README.md` describes how to bring up the stand, install the driver from
`deploy/` with `stand/install.sh` and run the end-to-end check
`stand/csi-e2e.sh`. The manifests have not been applied outside the stand; on
another cluster the image and the `ambar-nodes` ConfigMap must be set for that
cluster (see the comments in `deploy/daemonset.yaml` and
`deploy/nodes-configmap.example.yaml`).

## Cluster operations and workloads

All production operational assets for the 3-node NanoPi R5C cluster (`k3s-1..3`) and supporting infrastructure are maintained in the repository:

- [`ops/`](ops/README.md): Production cluster deployment manifests, Traefik ingress, MetalLB L2 load balancer, and migrated apps (`smarthome`, `ingress`, `registry`, etc.).
- [`ops/nanopi-r5c-k3s/`](ops/nanopi-r5c-k3s/README.md): Detailed hardware setup guide, network layout (`wan0` 198.18.1.0/24, `lan0` 10.10.10.0/24), LVM NVMe partitioning, and HA Control Plane with kube-vip.
- [`stand/`](stand/README.md#физический-кластер-nanopi-r5c-скрипты-r5c-sh): Automation scripts for physical cluster maintenance (`stand/r5c-*.sh`), including LVM migration (`stand/r5c-migrate-lvm.sh`), kube-vip management (`stand/r5c-kube-vip.sh`), and fio performance benchmarks.
- [`ops/alerting/`](ops/alerting/README.md) & [`ops/grafana/`](ops/grafana/README.md): VictoriaMetrics alert rules and Grafana dashboards for cluster and storage observability.

## Releases

`ambar` uses CalVer tags (`vYY.M.PATCH` without a leading zero in the month,
e.g. `v26.11.0`) published as GitHub Releases (`Tag Release`). Each release
publishes static Linux `musl` binaries (`ambar-<version>-aarch64-unknown-linux-musl.tar.gz`
and `ambar-<version>-x86_64-unknown-linux-musl.tar.gz`), `SHA256SUMS.txt`, and
automatically generated release notes. The container image `ambar-drbd`, which
bundles a DRBD kernel module compiled for a specific host `KVER`, continues to
be built locally with `image/build.sh`. The Kubernetes CRD API version
(`v1alpha1`) remains independent of release tags.

The manual [*Release*](.github/workflows/release.yml) workflow accepts an
optional `YY.M` series (defaulting to the current UTC month), computes the next
patch number from existing `vYY.M.*` tags, updates `Cargo.toml` and `Cargo.lock`
via [`.github/scripts/version.py`](.github/scripts/version.py), pushes a
`release: cut v<version>` commit to `main`, and prints the `git tag -s v<version>`
command to run locally. Pushing that signed tag triggers
[*Tag Release*](.github/workflows/tag-release.yml), which verifies manifest
versions, builds both Linux `musl` targets, generates release notes with
`git-cliff` ([`.github/scripts/gen-release-notes.sh`](.github/scripts/gen-release-notes.sh)),
and publishes the GitHub Release as `latest`. See [CHANGELOG.md](CHANGELOG.md).

## License

Apache License 2.0, see [LICENSE](LICENSE).
