//! Brings one volume on one node to the state its description asks for.
//!
//! `plan` is a pure function from the description and the observed state to
//! a list of actions; `Reconciler` observes the node and executes the plan.
//!
//! Data-safety rules this module enforces:
//!
//! 1. `drbdadm create-md --force` destroys existing metadata: it runs only on
//!    a backing file without the ready marker `<volume>.img.ready` whose
//!    metadata is absent or in the just-created state. The decision comes from
//!    the metadata itself, read by `drbdadm dump-md` right before `create-md`
//!    in `execute` (`Drbd::metadata`; `plan` does not depend on it):
//!    `Metadata::Absent` (no metadata) and `Metadata::Fresh` (metadata that
//!    never held data: `current-uuid` `0x4`, zero bitmap and history UUIDs,
//!    experiment P10) run `create-md`; `Metadata::Used` (anything else,
//!    including a dump that cannot be read) stops the pass with
//!    `MetadataPresent`, and a failed check stops it with
//!    `DrbdError::MetadataCheck`. `Fresh` is what an interrupted pass leaves
//!    (`create-md` done, ready marker not written), so the next `apply`
//!    repeats `create-md` and finishes. No marker file takes part in the
//!    decision. A leftover `<volume>.img.creating` of an earlier build is
//!    never read; `remove` and `ClearStaleMarkers` delete it.
//! 2. `drbdadm new-current-uuid --clear-bitmap <volume>/0` declares all copies
//!    identical without copying data: only the leader (lowest `nodeId` among
//!    nodes with a copy) runs it, only when the description says
//!    `initialSync: Skip` (the default is `Done`), only while the synced
//!    marker is absent and the status gate holds.
//! 3. `drbdadm primary --force` is never run.
//! 4. The outcome of a state-changing `drbdadm` command is established by
//!    re-reading `drbdsetup status --json`, not by its return code (for
//!    `adjust` a non-zero code is a failure, a zero code is not proof).
//! 5. The volumes directory is never created by `apply` or `remove`: when it
//!    is missing they fail with `Io` ("open" of the lock file). The node
//!    setup creates it.
//!
//! Decision table of `plan`. "Copy" means the node holds a copy (`diskful`).
//! `matches` is `status_matches_spec`; `applied` is `Observed::res_applied`.
//!
//! Errors, checked first; nothing is planned when one applies:
//!
//! | Input | Result |
//! |---|---|
//! | node not in the description | `NotAMember` |
//! | copy, backing file without the ready marker, synced marker present | `ReadyMarkerLost` |
//! | copy, backing file absent or without the ready marker, resource configured | `UninitialisedFileWithLiveResource` |
//!
//! Otherwise the actions below are appended in this order:
//!
//! | Input | Actions |
//! |---|---|
//! | copy, ready or synced marker present while the image is absent | `ClearStaleMarkers` |
//! | copy, no backing file | `CreateFile` |
//! | copy, no loop device | `AttachLoop` |
//! | copy, no loop device or the link points elsewhere | `LinkDevice` |
//! | resource file differs from the rendered one | `WriteRes` |
//! | copy, backing file without the ready marker | `CreateMd`, `MarkReady` |
//! | resource not configured | `Up`, `MarkApplied` |
//! | resource configured, and the file differs, or not `applied`, or not `matches` | `Adjust`, `MarkApplied` |
//!
//! Only when the resource is configured, the file matches, it was applied,
//! the status matches the description, the node holds a copy and nothing
//! else was planned:
//!
//! | Input | Actions |
//! |---|---|
//! | leader, `initialSync: Skip`, no synced marker, `ready_to_skip_initial_sync` | `SkipInitialSync` |
//! | no synced marker, local volume-0 disk `UpToDate`, or a peer's volume-0 disk `UpToDate`, `Consistent` or `Outdated` | `MarkSynced` |
//! | anything else | nothing |
//!
//! The executor re-reads the status after `Up` (the resource must be
//! configured), after `Adjust` (the exit code must be zero, the resource must
//! be configured and match the description) and after `drbdadm down` in
//! `remove` (it must be gone); it stops with `NotApplied` otherwise. The exit
//! code of `adjust` is checked because the status cannot show every field of
//! the resource file. The pass that runs `SkipInitialSync` is the last pass
//! of an `apply`: the synced marker is written by a later `apply` once the
//! status shows the local disk `UpToDate`, or a peer's disk in a state that
//! holds data. When the skip gate holds, every peer disk is `Inconsistent` or
//! the peer has no disk, so the two rules never both apply.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::backing::{self, BackingError};
use crate::drbd::adm::{Drbd, DrbdError, Metadata};
use crate::drbd::resource;
use crate::drbd::status::{DiskState, ResourceStatus};
use crate::exec::{Runner, command_line};
use crate::model::{InitialSync, SpecError, VolumeSpec, valid_volume_name};
use crate::paths::Paths;

/// Pass 1 creates, configures or repairs; pass 2 observes the result and,
/// once the resource is configured as described and the peers are connected,
/// lets the leader skip the initial sync. A pass that skips is the last one:
/// another pass could still see the copies `Inconsistent` and repeat the
/// command instead of waiting for its effect.
const MAX_ROUNDS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No backing file.
    Absent,
    /// Backing file without the ready marker.
    Pending,
    /// Backing file with the ready marker: DRBD metadata was created on it.
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub file: FileState,
    /// Loop device the backing file is attached to.
    pub loop_device: Option<String>,
    /// Target of the stable device link.
    pub link_target: Option<String>,
    /// The resource file on disk equals the rendered one.
    pub res_matches: bool,
    /// The copy of the last resource file `drbdadm` applied with a confirmed
    /// effect equals the rendered one.
    pub res_applied: bool,
    /// The synced marker exists.
    pub synced: bool,
    /// The ready or synced marker exists while the image does not.
    pub stale_markers: bool,
    /// `None`: the resource is not configured in the kernel.
    pub status: Option<ResourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ClearStaleMarkers,
    CreateFile { bytes: u64 },
    AttachLoop,
    LinkDevice,
    WriteRes,
    CreateMd,
    MarkReady,
    Up,
    Adjust,
    MarkApplied,
    SkipInitialSync,
    MarkSynced,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("node {node:?} is not a member of volume {volume:?}")]
    NotAMember { node: String, volume: String },
    #[error(
        "volume {volume:?}: the backing file is missing or has no ready marker, but the resource is configured in the \
         kernel; metadata is never created under a configured resource. Check `drbdsetup status {volume}` and take the \
         resource down with `drbdadm down {volume}` before the next apply"
    )]
    UninitialisedFileWithLiveResource { volume: String },
    #[error(
        "volume {volume:?}: the backing file has no ready marker, but the synced marker exists, so the file held DRBD \
         metadata and data; it is not initialised again. Check whether the file still holds DRBD metadata \
         (`drbdadm dump-md {volume}` with the resource down): if it does, recreate the ready marker; if the copy is \
         to be discarded, run `ambar remove --name {volume}` first"
    )]
    ReadyMarkerLost { volume: String },
}

/// `true` when the configured resource is the one the description asks for
/// on `node_name`: its node id, the minor of volume 0, exactly one
/// connection per other node (same name and peer node id), a local disk on
/// a node with a copy and a client device on a node without one.
pub fn status_matches_spec(spec: &VolumeSpec, node_name: &str, status: &ResourceStatus) -> bool {
    let Some(node) = spec.node(node_name) else {
        return false;
    };
    let Some(device) = status.device() else {
        return false;
    };
    let mut expected: Vec<(&str, u32)> = spec
        .nodes
        .iter()
        .filter(|n| n.name != node_name)
        .map(|n| (n.name.as_str(), n.node_id))
        .collect();
    let mut actual: Vec<(&str, u32)> = status
        .connections
        .iter()
        .map(|c| (c.name.as_str(), c.peer_node_id))
        .collect();
    expected.sort_unstable();
    actual.sort_unstable();
    let disk_as_described = if node.diskful {
        device.disk_state != DiskState::Diskless
    } else {
        device.client
    };
    status.node_id == node.node_id && device.minor == spec.minor && actual == expected && disk_as_described
}

/// Decides what has to be done on `node_name` for `spec`, given `obs`.
pub fn plan(spec: &VolumeSpec, node_name: &str, obs: &Observed) -> Result<Vec<Action>, PlanError> {
    let node = spec.node(node_name).ok_or_else(|| PlanError::NotAMember {
        node: node_name.to_string(),
        volume: spec.name.clone(),
    })?;
    if node.diskful && obs.file == FileState::Pending && obs.synced {
        // The synced marker is written only after metadata was created and
        // the copy was in sync: the file holds data, whatever happened to
        // the ready marker.
        return Err(PlanError::ReadyMarkerLost {
            volume: spec.name.clone(),
        });
    }
    if node.diskful && obs.file != FileState::Ready && obs.status.is_some() {
        return Err(PlanError::UninitialisedFileWithLiveResource {
            volume: spec.name.clone(),
        });
    }

    let mut actions = Vec::new();
    if node.diskful {
        if obs.stale_markers {
            actions.push(Action::ClearStaleMarkers);
        }
        if obs.file == FileState::Absent {
            let bytes = backing::backing_file_bytes(spec.size_bytes, spec.max_peers);
            actions.push(Action::CreateFile { bytes });
        }
        if obs.loop_device.is_none() {
            actions.push(Action::AttachLoop);
        }
        if obs.loop_device.is_none() || obs.link_target != obs.loop_device {
            actions.push(Action::LinkDevice);
        }
    }
    if !obs.res_matches {
        actions.push(Action::WriteRes);
    }
    if node.diskful && obs.file != FileState::Ready {
        // Metadata is created only for a file this agent has not finished
        // initialising; a ready file is never touched.
        actions.push(Action::CreateMd);
        actions.push(Action::MarkReady);
    }

    match &obs.status {
        None => {
            actions.push(Action::Up);
            actions.push(Action::MarkApplied);
        }
        Some(status) if !obs.res_matches || !obs.res_applied || !status_matches_spec(spec, node_name, status) => {
            actions.push(Action::Adjust);
            actions.push(Action::MarkApplied);
        }
        Some(status) if node.diskful && actions.is_empty() => {
            let leader = spec.initial_sync_leader().is_some_and(|l| l.name == node_name);
            let disk = status.device().map(|d| d.disk_state);
            let peer_has_data = status.connections.iter().any(|c| {
                c.peer_devices.iter().find(|p| p.volume == 0).is_some_and(|p| {
                    matches!(
                        p.peer_disk_state,
                        DiskState::UpToDate | DiskState::Consistent | DiskState::Outdated
                    )
                })
            });
            if !obs.synced && leader && spec.initial_sync == InitialSync::Skip && status.ready_to_skip_initial_sync() {
                actions.push(Action::SkipInitialSync);
            } else if !obs.synced && (disk == Some(DiskState::UpToDate) || peer_has_data) {
                // The volume has a data source: this copy is the one that
                // holds data or receives it. The marker stops a later skip
                // from declaring a partial copy identical to an empty one.
                actions.push(Action::MarkSynced);
            }
        }
        Some(_) => {}
    }
    Ok(actions)
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Backing(#[from] BackingError),
    #[error(transparent)]
    Drbd(#[from] DrbdError),
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: io::Error,
    },
    #[error(
        "volume {volume:?}: `drbdadm {command} {volume}` did not take effect: drbdsetup status does not show the \
         expected state"
    )]
    NotApplied { command: &'static str, volume: String },
    #[error(
        "volume {volume:?}: the backing file already holds DRBD metadata but has no ready marker; the metadata was \
         not touched. Check the file by hand before restoring the marker"
    )]
    MetadataPresent { volume: String },
    #[error("volume {volume:?} is locked by another ambar process; try again later")]
    Busy { volume: String },
    #[error("volume {volume:?}: no loop device is known to point the device link at")]
    NoLoopDevice { volume: String },
}

pub struct Reconciler<'a> {
    pub paths: &'a Paths,
    pub runner: &'a dyn Runner,
}

impl Reconciler<'_> {
    /// Validates the description, locks the volume, observes the node,
    /// executes the plan and returns what was done.
    pub fn apply(&self, spec: &VolumeSpec, node_name: &str) -> Result<Vec<Action>, ReconcileError> {
        spec.validate()?;
        let _lock = self.lock_volume(&spec.name)?;
        let mut done = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let obs = self.observe(spec, node_name)?;
            let actions = plan(spec, node_name, &obs)?;
            if actions.is_empty() {
                break;
            }
            let mut loop_device = obs.loop_device.clone();
            for action in &actions {
                self.execute(spec, node_name, action, &mut loop_device)?;
            }
            let skipped = actions.contains(&Action::SkipInitialSync);
            done.extend(actions);
            if skipped {
                break;
            }
        }
        Ok(done)
    }

    pub fn observe(&self, spec: &VolumeSpec, node_name: &str) -> Result<Observed, ReconcileError> {
        let volume = spec.name.as_str();
        let diskful = spec.node(node_name).is_some_and(|n| n.diskful);
        let image = self.paths.image(volume);
        let image_exists = exists(&image)?;
        let ready = exists(&self.paths.ready_marker(volume))?;
        let synced = exists(&self.paths.synced_marker(volume))?;

        let file = match (image_exists, ready) {
            (false, _) => FileState::Absent,
            (true, true) => FileState::Ready,
            (true, false) => FileState::Pending,
        };
        let loop_device = if diskful && file != FileState::Absent {
            backing::find_loop(self.runner, &image)?
        } else {
            None
        };
        let rendered = self.rendered(spec);
        let same_text = |path: PathBuf| fs::read_to_string(path).is_ok_and(|on_disk| on_disk == rendered);
        let status = Drbd { runner: self.runner }.status(volume)?;

        Ok(Observed {
            file,
            loop_device,
            link_target: backing::link_target(&self.paths.device_link(volume)),
            res_matches: same_text(self.paths.res_file(volume)),
            res_applied: same_text(self.paths.applied_res(volume)),
            synced,
            stale_markers: !image_exists && (ready || synced),
            status,
        })
    }

    fn execute(
        &self,
        spec: &VolumeSpec,
        node_name: &str,
        action: &Action,
        loop_device: &mut Option<String>,
    ) -> Result<(), ReconcileError> {
        let volume = spec.name.as_str();
        let drbd = Drbd { runner: self.runner };
        let not_applied = |command| ReconcileError::NotApplied {
            command,
            volume: volume.to_string(),
        };
        match action {
            Action::ClearStaleMarkers => {
                remove_if_present(&self.paths.ready_marker(volume))?;
                remove_if_present(&self.paths.synced_marker(volume))?;
                remove_if_present(&self.paths.leftover_creating_file(volume))?;
                sync_dir(&self.paths.volumes_dir)?;
            }
            Action::CreateFile { bytes } => {
                // Made durable before the ready marker can exist, so the
                // marker never outlives a file that a power loss shortened.
                backing::create_sparse_file(&self.paths.image(volume), *bytes)?;
                sync_file(&self.paths.image(volume))?;
                sync_dir(&self.paths.volumes_dir)?;
            }
            Action::AttachLoop => *loop_device = Some(backing::attach_loop(self.runner, &self.paths.image(volume))?),
            Action::LinkDevice => {
                let device = loop_device.as_deref().ok_or_else(|| ReconcileError::NoLoopDevice {
                    volume: volume.to_string(),
                })?;
                backing::link_device(&self.paths.device_link(volume), device)?;
            }
            Action::WriteRes => {
                // The resource directory is not under the volumes-directory
                // rule: in the agent container it is `/etc/drbd.d`.
                fs::create_dir_all(&self.paths.res_dir).map_err(|source| ReconcileError::Io {
                    action: "create directory",
                    path: self.paths.res_dir.display().to_string(),
                    source,
                })?;
                write_atomically(&self.paths.res_file(volume), &self.rendered(spec))?
            }
            Action::CreateMd => {
                // The file has no ready marker, but it may still hold
                // metadata of a copy (a marker lost, a file restored, metadata
                // written by something else): `create-md --force` would
                // destroy it. It runs only over no metadata or over metadata
                // that never held data, which is also what an earlier pass of
                // this agent leaves when it stops before `MarkReady`.
                match drbd.metadata(volume)? {
                    Metadata::Absent | Metadata::Fresh => {}
                    Metadata::Used => {
                        return Err(ReconcileError::MetadataPresent {
                            volume: volume.to_string(),
                        });
                    }
                }
                drbd.create_md(volume, spec.max_peers, spec.size_bytes)?;
                // `create-md` writes through the loop device; flush the
                // backing file itself before `MarkReady` records the result.
                sync_file(&self.paths.image(volume))?;
            }
            Action::MarkReady => write_atomically(&self.paths.ready_marker(volume), "")?,
            Action::Up => {
                drbd.up(volume)?;
                if drbd.status(volume)?.is_none() {
                    return Err(not_applied("up"));
                }
            }
            Action::Adjust => {
                // A zero code is necessary, not sufficient: the kernel can
                // reject a change the status does not show (address, port,
                // protocol, quorum options) and leave the connections as they
                // were. The status check covers what the status can show.
                let rejected = !drbd.adjust(volume)?.success();
                let matches = drbd
                    .status(volume)?
                    .is_some_and(|status| status_matches_spec(spec, node_name, &status));
                if rejected || !matches {
                    return Err(not_applied("adjust"));
                }
            }
            Action::MarkApplied => write_atomically(&self.paths.applied_res(volume), &self.rendered(spec))?,
            Action::SkipInitialSync => drbd.skip_initial_sync(volume)?,
            Action::MarkSynced => write_atomically(&self.paths.synced_marker(volume), "")?,
        }
        Ok(())
    }

    fn rendered(&self, spec: &VolumeSpec) -> String {
        resource::render(spec, &self.paths.device_link(&spec.name))
    }

    /// Takes the volume down on this node and deletes its local files.
    /// Returns the steps that were performed.
    pub fn remove(&self, volume: &str) -> Result<Vec<&'static str>, ReconcileError> {
        if !valid_volume_name(volume) {
            return Err(SpecError::Name(volume.to_string()).into());
        }
        let _lock = self.lock_volume(volume)?;
        let mut steps = Vec::new();
        let drbd = Drbd { runner: self.runner };
        if drbd.status(volume)?.is_some() {
            drbd.down(volume)?;
            if drbd.status(volume)?.is_some() {
                return Err(ReconcileError::NotApplied {
                    command: "down",
                    volume: volume.to_string(),
                });
            }
            steps.push("down");
        }
        let image = self.paths.image(volume);
        let devices = if exists(&image)? {
            backing::find_loops(self.runner, &image)?
        } else {
            self.loop_of_missing_image(volume)?.into_iter().collect()
        };
        for device in devices {
            backing::detach_loop(self.runner, &device)?;
            steps.push("detach-loop");
        }
        // The image goes first and its removal is made durable before the
        // markers go: a crash in between leaves markers without an image
        // (cleared by the next apply), never an image without markers that
        // the next apply would initialise again.
        if remove_if_present(&image)? {
            steps.push("remove-file");
        }
        sync_dir(&self.paths.volumes_dir)?;
        let markers = [
            (self.paths.ready_marker(volume), "remove-marker"),
            (self.paths.synced_marker(volume), "remove-synced-marker"),
            (
                self.paths.leftover_creating_file(volume),
                "remove-leftover-creating-file",
            ),
        ];
        for (path, step) in markers {
            if remove_if_present(&path)? {
                steps.push(step);
            }
        }
        sync_dir(&self.paths.volumes_dir)?;
        let rest = [
            (self.paths.device_link(volume), "remove-link"),
            (self.paths.res_file(volume), "remove-res"),
            (self.paths.applied_res(volume), "remove-applied"),
        ];
        for (path, step) in rest {
            if remove_if_present(&path)? {
                steps.push(step);
            }
        }
        if exists(&self.paths.res_dir)? {
            sync_dir(&self.paths.res_dir)?;
        }
        // Last step, still under the lock.
        remove_if_present(&self.paths.lock_file(volume))?;
        Ok(steps)
    }

    /// The loop device the device link points at, when the image is gone and
    /// `losetup` reports that device's backing file as the (deleted) image.
    /// After an interrupted `remove` the link can point at a device that was
    /// detached and since reused for another file; that one is left alone.
    fn loop_of_missing_image(&self, volume: &str) -> Result<Option<String>, ReconcileError> {
        let Some(device) = backing::link_target(&self.paths.device_link(volume)) else {
            return Ok(None);
        };
        let args = ["--list", "--noheadings", "--output", "BACK-FILE", device.as_str()];
        let out = self.runner.run("losetup", &args).map_err(|e| BackingError::Spawn {
            command: command_line("losetup", &args),
            message: e.to_string(),
        })?;
        if !out.success() {
            // Not a loop device, or not attached: nothing to detach.
            return Ok(None);
        }
        let image = self.paths.image(volume).display().to_string();
        let back_file = out.stdout.trim();
        let ours = back_file == image || back_file.strip_suffix(" (deleted)") == Some(image.as_str());
        Ok(ours.then_some(device))
    }

    /// Takes the exclusive lock of `volume` without waiting; it is held until
    /// the returned file is dropped.
    fn lock_volume(&self, volume: &str) -> Result<File, ReconcileError> {
        let path = self.paths.lock_file(volume);
        let io_err = |action, source| ReconcileError::Io {
            action,
            path: path.display().to_string(),
            source,
        };
        // The directory is never created here: a missing one means the
        // volume storage is not mounted (or the node is set up wrong), and a
        // fresh empty directory on the root disk would hide that and start
        // the volumes from scratch.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound && !self.paths.volumes_dir.exists() {
                    return io_err(
                        "open",
                        io::Error::new(
                            e.kind(),
                            format!(
                                "the volumes directory {} does not exist; it must exist on the node (mode 0700) \
                                 before the agent runs, the agent never creates it",
                                self.paths.volumes_dir.display()
                            ),
                        ),
                    );
                }
                io_err("open", e)
            })?;
        let busy = || ReconcileError::Busy {
            volume: volume.to_string(),
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(busy()),
            Err(TryLockError::Error(e)) => return Err(io_err("lock", e)),
        }
        // `remove` deletes the lock file while holding the lock. If that
        // happened between our open and our lock, the lock is on a file no
        // longer reachable by its name and excludes nobody.
        if still_linked(&file, &path).map_err(|e| io_err("inspect", e))? {
            Ok(file)
        } else {
            Err(busy())
        }
    }
}

/// Whether `path` exists. A failed `stat` (permission denied, a loop of
/// symbolic links, a component that is not a directory) is an error, not
/// "absent": a marker that cannot be read must stop the pass, because
/// treating it as missing leads to `drbdadm create-md --force` over data.
fn exists(path: &Path) -> Result<bool, ReconcileError> {
    path.try_exists().map_err(|source| ReconcileError::Io {
        action: "inspect",
        path: path.display().to_string(),
        source,
    })
}

/// `true` when `path` still names the open `file`: same device and inode.
/// `false` when the path was removed or replaced by another file.
fn still_linked(file: &File, path: &Path) -> io::Result<bool> {
    let held = file.metadata()?;
    match fs::metadata(path) {
        Ok(current) => Ok(current.dev() == held.dev() && current.ino() == held.ino()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Writes `text` under a temporary name next to `path`, syncs it, renames it
/// over `path` and syncs the directory: after a crash `path` holds either
/// the old or the new text, and once this returns the new text survives a
/// power loss. The directory of `path` must exist; it is never created here
/// (`Action::WriteRes` creates the resource directory itself, the volumes
/// directory is never created).
fn write_atomically(path: &Path, text: &str) -> Result<(), ReconcileError> {
    let io_err = |action, source| ReconcileError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = temp_path(path);
    let mut file = File::create(&tmp).map_err(|e| io_err("create", e))?;
    file.write_all(text.as_bytes()).map_err(|e| io_err("write", e))?;
    file.sync_all().map_err(|e| io_err("sync", e))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| io_err("rename into", e))?;
    sync_dir(parent)
}

/// `.<file name>.tmp` next to `path`, so `x.img.ready` and `x.img.synced`
/// never share a temporary file.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tmp"))
}

/// Flushes the data and metadata of the file at `path` to storage.
fn sync_file(path: &Path) -> Result<(), ReconcileError> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|source| ReconcileError::Io {
            action: "sync",
            path: path.display().to_string(),
            source,
        })
}

/// Makes the creation, renaming and removal of entries in `dir` durable.
fn sync_dir(dir: &Path) -> Result<(), ReconcileError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|source| ReconcileError::Io {
            action: "sync directory",
            path: dir.display().to_string(),
            source,
        })
}

fn remove_if_present(path: &Path) -> Result<bool, ReconcileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ReconcileError::Io {
            action: "remove",
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
#[path = "tests/reconcile.rs"]
mod tests;
