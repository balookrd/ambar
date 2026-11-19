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
| Production run | not done: nothing here has run on a production cluster |
| Measurements on the target hardware (NanoPi R5C) | not done |

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
- Volume data is a sparse file on the node's local disk, attached as a loop
  device with direct I/O. No repartitioning is required.
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
- `ambar_node_allocated_bytes{node}`: total allocated sparse volume file bytes on the node.
- `ambar_node_tainted{node}`: `1` if node is tainted with `no-quorum:NoSchedule`, `0` otherwise.
- `ambar_volume_quorum{node, volume}`: `1` if the volume has DRBD quorum on the node, `0` otherwise.
- `ambar_volume_standalone_connections{node, volume}`: count of peer connections in `StandAlone` state.
- `ambar_volume_manual_check_needed{node, volume}`: `1` if auto-adjust attempts are exhausted (split-brain alert), `0` otherwise.
- `ambar_module_info{version}`: DRBD kernel module version info metric.

## Repository layout

| Path | Contents |
|---|---|
| `docs/superpowers/specs/2026-11-12-ambar-design.md` | design spec (Russian) |
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | record of the DRBD 9 experiments and the stand checks (Russian) |
| `docs/superpowers/plans/` | executed plans of phases 0, 1, 2a and 2b (Russian, historical) |
| `src/` | the `ambar` binary (Rust): node agent, reconcile loop, CSI services |
| `proto/csi.proto`, `build.rs` | CSI v1 protocol definition and its code generation |
| `deploy/` | Kubernetes manifests: CRD, CSIDriver, RBAC, DaemonSet, StorageClasses, example of the `ambar-nodes` ConfigMap |
| `image/` | Dockerfile, module loader, image tests |
| `stand/` | test stand scripts, driver install and end-to-end check; see `stand/README.md` (Russian) |
| `stand/spike/` | experiment scripts and `drbdsetup status --json` samples |

## Requirements for the image and the stand

| What | Value |
|---|---|
| Host | macOS on Apple silicon |
| Lima | 2.3.0, `vmType: vz` |
| Docker | `docker buildx`, `linux/arm64` |
| Kernel the module is built for | `6.12.116+deb13-arm64` (Debian 13 stock kernel) |
| Rust | stable 1.95 or newer (the `rust-version` in `Cargo.toml`) |
| For `image/build.sh` on the host | `cargo-zigbuild`, `zig`, and the target added with `rustup target add aarch64-unknown-linux-musl` (checked with zig 0.17.1 and rustc 1.99.1) |

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

Expected line: `test result: ok. 902 passed`. The mount code has tests that run
only on Linux; on macOS run the suite in a container as well:
`docker run --rm -v "$PWD":/src -w /src rust:1-slim cargo test --locked`.

## Install on the stand

`stand/README.md` describes how to bring up the stand, install the driver from
`deploy/` with `stand/install.sh` and run the end-to-end check
`stand/csi-e2e.sh`. The manifests have not been applied outside the stand; on
another cluster the image and the `ambar-nodes` ConfigMap must be set for that
cluster (see the comments in `deploy/daemonset.yaml` and
`deploy/nodes-configmap.example.yaml`).

## License

Apache License 2.0, see [LICENSE](LICENSE).
