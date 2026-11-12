# ambar

[Русская версия](README.ru.md)

`ambar` is a Kubernetes CSI driver for replicated persistent volumes on small
k3s clusters. Replication is done by the DRBD 9 kernel module; the driver
itself is planned as a single Rust binary running as a DaemonSet.

## Status

The driver is not written yet. The repository contains the design and the
groundwork it rests on:

| Part | State |
|---|---|
| Design spec | written, five open questions listed at its end |
| Image with a prebuilt DRBD 9.3.4 module and drbd-utils 9.35.0 | builds, has tests |
| Test stand: three Lima VMs with k3s | scripts in `stand/` |
| DRBD 9 failure experiments on the stand | recorded with raw outputs |
| CSI driver (Rust agent) | not started |
| Measurements on the target hardware (NanoPi R5C) | not done |

Nothing here has been run on a production cluster.

## What it is designed to do

- A volume has `replicas: N` copies on N nodes (N is at least 2). The other
  nodes attach to the volume without a disk, so a pod can start on any node.
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
| `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md` | record of the DRBD 9 experiments (Russian) |
| `docs/superpowers/plans/` | executed plan of the groundwork phase (Russian, historical) |
| `image/` | Dockerfile, module loader, image tests |
| `stand/` | test stand scripts; see `stand/README.md` (Russian) |
| `stand/spike/` | experiment scripts and `drbdsetup status --json` samples |

## Requirements for the image and the stand

| What | Value |
|---|---|
| Host | macOS on Apple silicon |
| Lima | 2.3.0, `vmType: vz` |
| Docker | `docker buildx`, `linux/arm64` |
| Kernel the module is built for | `6.12.116+deb13-arm64` (Debian 13 stock kernel) |

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

Bringing up the stand is described in `stand/README.md`.

## License

No license has been chosen yet.
