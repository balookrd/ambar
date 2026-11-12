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
| `diskless-peer-down.json` | `spike2` | `lima-dv-3` | diskless node Primary writing, diskful peer `lima-dv-2` stopped gracefully (`limactl stop`) (`Connecting`), quorum kept |
| `peer-disconnected-diskful.json` | `spike2` | `lima-dv-1` | diskful Secondary, peer `lima-dv-2` stopped gracefully, diskless peer `lima-dv-3` Primary |
| `resync-target.json` | `spike2` | `lima-dv-2` | rejoined after a graceful stop, writer still running on `lima-dv-3`: `Inconsistent`, `SyncTarget` from `lima-dv-1` with `rs-*` sync details |
| `standalone-primary.json` | `spike` | `lima-dv-1` | experiment E1: `on-no-quorum io-error`, network restored while still Primary with the volume mounted; both connections `StandAlone`, `quorum: false` |
| `diskless-peer-killed.json` | `spike2` | `lima-dv-3` | experiment E2: diskless Primary writing, diskful peer `lima-dv-2` force-stopped (`limactl stop --force`), peer `Connecting`, quorum kept |
| `diskless-alone-no-quorum.json` | `spike2` | `lima-dv-3` | experiment C1 variant A: both diskful peers force-stopped one after the other, diskless node alone and Secondary: `quorum: false`, both peers `Connecting` |
| `stale-diskful-no-quorum.json` | `spike2` | `lima-dv-2` | experiment C1 variant A: stale copy started while the newest copy `lima-dv-1` is down, connected only to the diskless node; reports its own disk as `UpToDate` although it is stale, `quorum: false` |
| `diskless-stale-peer.json` | `spike2` | `lima-dv-3` | experiment C1 variant A, same moment, diskless node view: peer `lima-dv-2` is `Consistent` (downgraded because its data is older), `quorum: false` |
| `diskless-rebooted-stale-peer.json` | `spike2` | `lima-dv-3` | experiment C1 variant B: diskless node rebooted (no in-kernel memory of the newer data), connected only to the stale copy `lima-dv-2`: peer shown as `UpToDate`, `quorum: false` |
| `split-brain-victim.json` | `spike` | `lima-dv-1` | experiment C3, resource with `quorum off`: real split-brain after both sides wrote while isolated; both connections `StandAlone`, Secondary, `quorum: true` (always true with `quorum off`). No field marks the split-brain; compare with `standalone-primary.json` only by role, `quorum`, `open` and counters |
| `split-brain-survivor.json` | `spike` | `lima-dv-2` | experiment C3, same moment, the side whose data was kept: connection to `lima-dv-1` `StandAlone`, connection to `lima-dv-3` `Connected` |
