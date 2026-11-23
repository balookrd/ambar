# Changelog

Starting with `v26.11.0`, `ambar` uses CalVer tags in the `vYY.M.PATCH` format
without a leading zero in the month (for example `v26.11.0`, `v26.11.1`,
`v26.12.0`). Prior to the first release, `Cargo.toml` carried the pre-release
placeholder version `0.1.0`. The Kubernetes CRD schema version (`v1alpha1` in
`ReplicatedVolume`) evolves independently from driver release tags.

Release notes for each GitHub Release are generated automatically from
Conventional Commits across the repository using [`cliff.toml`](cliff.toml) and
[`.github/scripts/gen-release-notes.sh`](.github/scripts/gen-release-notes.sh)
(`feat`, `fix`, `perf`, and `BREAKING CHANGE` entries; `ops`, `docs`, `chore`,
`refactor`, `test`, `ci`, `build`, `bench`, and `style` commits are omitted).

## Pre-CalVer Baseline (Phases 0–5)

Before adopting CalVer releases, the project completed Phases 0–5 and hardening:

- **Kubernetes CSI driver over DRBD 9** (`ambar` agent, controller, node, and
  identity services, `ReplicatedVolume` CRD `v1alpha1`, and unit/e2e test
  coverage).
- **Online replica management:** `ambar replica add`, `remove`, and `move` CLI
  commands with safety checks and witness rebalancing.
- **Failure handling & observability:** automatic `no-quorum` node tainting,
  30-second grace-period pod eviction, automatic `drbdadm adjust` recovery on
  `StandAlone` secondary connections, split-brain detection, stale `Client`
  cleanup, and Prometheus metrics on `:9770/metrics`.
- **Direct LVM block backend (`--volume-group`):** bypasses loopback files and
  host ext4/JBD2 double journaling, cutting 4K `fsync` p50 latency on NanoPi R5C
  clusters by 2.4x–2.8x.
- **Security & concurrency hardening:** volume-level file locks across CSI and
  failure paths, atomic Kubernetes API JSON patches with `resourceVersion` tests,
  PVC-to-PV validation for pod eviction, path-traversal checks, and bounded
  metrics server concurrency.

*Русская версия: [CHANGELOG.ru.md](CHANGELOG.ru.md)*
