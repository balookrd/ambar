# `drbdsetup status <res> --json` samples

Raw output captured on the stand (DRBD 9.3.4, drbd-utils 9.35.0) during the
assumption checks in `docs/superpowers/specs/2026-11-12-drbd9-spike-results.md`.
Fixtures for the status parser tests. Node ids: `lima-dv-1` = 0, `lima-dv-2` = 1,
`lima-dv-3` = 2.

| File | Resource | Captured on | State |
|---|---|---|---|
| `three-inconsistent.json` | `spike` | `lima-dv-1` | three diskful nodes Connected, all disks `Inconsistent`, before `new-current-uuid --clear-bitmap` |
| `three-uptodate.json` | `spike` | `lima-dv-1` | three diskful nodes Connected and `UpToDate`, nobody Primary |
| `primary-mounted.json` | `spike` | `lima-dv-1` | this node Primary with ext4 mounted (`open: true`), peers Secondary |
| `peer-disconnected.json` | `spike` | `lima-dv-2` | peer `lima-dv-1` isolated (`Connecting`), quorum kept with `lima-dv-3` |
| `quorum-lost.json` | `spike` | `lima-dv-1` | isolated node, `on-no-quorum io-error`: Primary, `quorum: false`, both peers `Connecting` |
| `quorum-suspended.json` | `spike` | `lima-dv-1` | isolated node, `on-no-quorum suspend-io`: `suspended-quorum: true`, `quorum: false` |
| `peer-primary.json` | `spike` | `lima-dv-1` | after rejoin and resync: this node Secondary, peer `lima-dv-2` Primary, all `UpToDate` |
| `diskless-node.json` | `spike2` | `lima-dv-3` | diskless node (`client: true`, `Diskless`), both diskful peers `UpToDate` |
| `diskless-peer-down.json` | `spike2` | `lima-dv-3` | diskless node Primary writing, diskful peer `lima-dv-2` powered off (`Connecting`), quorum kept |
| `peer-disconnected-diskful.json` | `spike2` | `lima-dv-1` | diskful Secondary, peer `lima-dv-2` powered off, diskless peer `lima-dv-3` Primary |
| `resync-target.json` | `spike2` | `lima-dv-2` | rejoined after power-off: `Inconsistent`, `SyncTarget` from `lima-dv-1` with `rs-*` sync details |
