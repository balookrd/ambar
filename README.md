# ambar

[Русская версия](README.ru.md)

`ambar` is a Kubernetes CSI driver for replicated persistent volumes on small
k3s clusters. Replication is done by the DRBD 9 kernel module; the driver is a
single Rust binary running as a DaemonSet.

## Status

| Part | State |
|---|---|
| Design spec | brought in line with the code of phase 2a; open questions listed at its end |
| Image with a prebuilt DRBD 9.3.4 module and drbd-utils 9.35.0 | builds, has tests |
| Test stand: four Lima VMs with k3s | scripts in `stand/` |
| DRBD 9 failure experiments on the stand | recorded with raw outputs |
| CSI driver | creates, mounts and deletes volumes through PVCs on the stand (`stand/csi-e2e.sh`); node failures are not handled |
| Node failure handling (phase 3): releasing a volume on quorum loss, the `no-quorum` taint, `adjust` on `StandAlone`, metrics | not implemented |
| Changing the replica set of an existing volume (phase 2b) | not implemented; the CRD rejects the change |
| Production run | not done: nothing here has run on a production cluster |
| Measurements on the target hardware (NanoPi R5C) | not done |

## What it does

- A volume has `replicas: N` copies on N nodes (N is at least 2). With an even
  N one more node joins as a diskless witness. Any other node attaches to the
  volume without a disk when a pod with the volume is scheduled there, and
  detaches when the pod is gone.
- With `protocol: C` a write is acknowledged after it is on the disks of all
  nodes that hold a copy.
- One node mounts a volume at a time (`ReadWriteOnce`, `ReadWriteOncePod`).
- Volume data is a sparse file on the node's local disk, attached as a loop
  device with direct I/O. No repartitioning is required.
- The DRBD module is built ahead of time for an exact kernel version and
  shipped in the image; nodes need no compiler.

## Repository layout

| Path | Contents |
|---|---|
| `docs/superpowers/specs/2026-11-12-ambar-design.md` | design spec (Russian) |
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | record of the DRBD 9 experiments and the stand checks (Russian) |
| `docs/superpowers/plans/` | executed plans of phases 0, 1 and 2a (Russian, historical) |
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

Expected line: `test result: ok. 744 passed`. The mount code has tests that run
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
