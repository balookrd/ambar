//! Brings one volume on one node to the state its description asks for.
//!
//! `plan` is a pure function from the description and the observed state to
//! a list of actions; `Reconciler` observes the node and executes the plan.
//!
//! Rules of `plan`, first match wins within a block:
//!
//! | Condition | Actions |
//! |---|---|
//! | node with a copy, no backing file | `CreateFile`, `AttachLoop`, `LinkDevice`, `WriteRes`, `CreateMd`, `MarkReady`, `Up` |
//! | backing file with the ready marker, no loop device, resource not configured (after a reboot) | `AttachLoop`, `LinkDevice`, `Up` |
//! | backing file without the ready marker, resource not configured (interrupted initialisation) | missing steps, then `CreateMd`, `MarkReady`, `Up` |
//! | backing file without the ready marker, resource configured | error `UninitialisedFileWithLiveResource` |
//! | node without a disk, resource not configured | `WriteRes` if needed, `Up` |
//! | resource file differs, resource configured | `WriteRes`, `Adjust` |
//!
//! The next rules apply only when none of the above planned anything: the
//! resource is configured and its file matches.
//!
//! | Condition (node with a copy) | Actions |
//! |---|---|
//! | leader, no synced marker, status gate holds | `SkipInitialSync`, `MarkSynced` |
//! | no synced marker, local volume-0 disk `UpToDate` | `MarkSynced` |
//! | local volume-0 disk `Diskless`, backing file ready | `Adjust` |
//! | anything else | nothing |
//!
//! With the synced marker present the plan never contains `SkipInitialSync`.

use std::fs;
use std::io;
use std::path::Path;

use crate::backing::{self, BackingError};
use crate::drbd::adm::{Drbd, DrbdError};
use crate::drbd::resource;
use crate::drbd::status::{DiskState, ResourceStatus};
use crate::exec::Runner;
use crate::model::{SpecError, VolumeSpec};
use crate::paths::Paths;

/// One pass creates and configures; a second pass can skip the initial sync
/// once the peers are connected. More passes would repeat that command.
const MAX_ROUNDS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No backing file.
    Absent,
    /// Backing file without the "metadata created" marker.
    Pending,
    /// Backing file with DRBD metadata.
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
    /// The synced marker exists.
    pub synced: bool,
    /// `None`: the resource is not configured in the kernel.
    pub status: Option<ResourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    CreateFile { bytes: u64 },
    AttachLoop,
    LinkDevice,
    WriteRes,
    CreateMd,
    MarkReady,
    Up,
    Adjust,
    SkipInitialSync,
    MarkSynced,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("node {node:?} is not a member of volume {volume:?}")]
    NotAMember { node: String, volume: String },
    #[error("volume {volume:?}: the backing file has no metadata marker, but the resource is configured in the kernel")]
    UninitialisedFileWithLiveResource { volume: String },
}

/// Decides what has to be done on `node_name` for `spec`, given `obs`.
pub fn plan(spec: &VolumeSpec, node_name: &str, obs: &Observed) -> Result<Vec<Action>, PlanError> {
    let node = spec.node(node_name).ok_or_else(|| PlanError::NotAMember {
        node: node_name.to_string(),
        volume: spec.name.clone(),
    })?;
    let mut actions = Vec::new();

    if node.diskful {
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
        if obs.status.is_some() {
            return Err(PlanError::UninitialisedFileWithLiveResource {
                volume: spec.name.clone(),
            });
        }
        // Metadata is created only for a file this agent has not finished
        // initialising; a ready file is never touched.
        actions.push(Action::CreateMd);
        actions.push(Action::MarkReady);
    }

    match &obs.status {
        None => actions.push(Action::Up),
        Some(_) if !obs.res_matches => actions.push(Action::Adjust),
        Some(status) if node.diskful && actions.is_empty() => {
            let leader = spec.initial_sync_leader().is_some_and(|l| l.name == node_name);
            let disk = status.device().map(|d| d.disk_state);
            if !obs.synced && leader && status.ready_to_skip_initial_sync() {
                actions.push(Action::SkipInitialSync);
                actions.push(Action::MarkSynced);
            } else if !obs.synced && disk == Some(DiskState::UpToDate) {
                actions.push(Action::MarkSynced);
            } else if obs.file == FileState::Ready && disk == Some(DiskState::Diskless) {
                // A failed `drbdadm up` can leave the resource configured with
                // the disk detached; `adjust` attaches it again.
                actions.push(Action::Adjust);
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
}

pub struct Reconciler<'a> {
    pub paths: &'a Paths,
    pub runner: &'a dyn Runner,
}

impl Reconciler<'_> {
    /// Validates the description, observes the node, executes the plan and
    /// returns what was done.
    pub fn apply(&self, spec: &VolumeSpec, node_name: &str) -> Result<Vec<Action>, ReconcileError> {
        spec.validate()?;
        let mut done = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let obs = self.observe(spec, node_name)?;
            let actions = plan(spec, node_name, &obs)?;
            if actions.is_empty() {
                break;
            }
            let mut loop_device = obs.loop_device.clone();
            for action in &actions {
                self.execute(spec, action, &mut loop_device)?;
            }
            done.extend(actions);
        }
        Ok(done)
    }

    pub fn observe(&self, spec: &VolumeSpec, node_name: &str) -> Result<Observed, ReconcileError> {
        let volume = spec.name.as_str();
        let diskful = spec.node(node_name).is_some_and(|n| n.diskful);
        let image = self.paths.image(volume);

        let file = if !image.exists() {
            FileState::Absent
        } else if self.paths.ready_marker(volume).exists() {
            FileState::Ready
        } else {
            FileState::Pending
        };
        let loop_device = if diskful && file != FileState::Absent {
            backing::find_loop(self.runner, &image)?
        } else {
            None
        };
        let rendered = resource::render(spec, &self.paths.device_link(volume));
        let res_matches = fs::read_to_string(self.paths.res_file(volume)).is_ok_and(|on_disk| on_disk == rendered);
        let status = Drbd { runner: self.runner }.status(volume)?;

        Ok(Observed {
            file,
            loop_device,
            link_target: backing::link_target(&self.paths.device_link(volume)),
            res_matches,
            synced: self.paths.synced_marker(volume).exists(),
            status,
        })
    }

    fn execute(
        &self,
        spec: &VolumeSpec,
        action: &Action,
        loop_device: &mut Option<String>,
    ) -> Result<(), ReconcileError> {
        let volume = spec.name.as_str();
        let drbd = Drbd { runner: self.runner };
        match action {
            Action::CreateFile { bytes } => backing::create_sparse_file(&self.paths.image(volume), *bytes)?,
            Action::AttachLoop => *loop_device = Some(backing::attach_loop(self.runner, &self.paths.image(volume))?),
            Action::LinkDevice => {
                // AttachLoop always precedes LinkDevice when no device was observed.
                let device = loop_device.as_deref().unwrap_or_default();
                backing::link_device(&self.paths.device_link(volume), device)?;
            }
            Action::WriteRes => {
                let text = resource::render(spec, &self.paths.device_link(volume));
                write_atomically(&self.paths.res_file(volume), &text)?;
            }
            Action::CreateMd => drbd.create_md(volume, spec.max_peers)?,
            Action::MarkReady => write_atomically(&self.paths.ready_marker(volume), "")?,
            Action::Up => drbd.up(volume)?,
            Action::Adjust => {
                drbd.adjust(volume)?;
            }
            Action::SkipInitialSync => drbd.skip_initial_sync(volume)?,
            Action::MarkSynced => write_atomically(&self.paths.synced_marker(volume), "")?,
        }
        Ok(())
    }

    /// Takes the volume down on this node and deletes its local files.
    /// Returns the steps that were performed.
    pub fn remove(&self, volume: &str) -> Result<Vec<&'static str>, ReconcileError> {
        let mut steps = Vec::new();
        let drbd = Drbd { runner: self.runner };
        if drbd.status(volume)?.is_some() {
            drbd.down(volume)?;
            steps.push("down");
        }
        let image = self.paths.image(volume);
        if image.exists()
            && let Some(device) = backing::find_loop(self.runner, &image)?
        {
            backing::detach_loop(self.runner, &device)?;
            steps.push("detach-loop");
        }
        let files = [
            (self.paths.device_link(volume), "remove-link"),
            (self.paths.res_file(volume), "remove-res"),
            (self.paths.ready_marker(volume), "remove-marker"),
            (self.paths.synced_marker(volume), "remove-synced-marker"),
            (image, "remove-file"),
        ];
        for (path, step) in files {
            if remove_if_present(&path)? {
                steps.push(step);
            }
        }
        Ok(steps)
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<(), ReconcileError> {
    let io_err = |action, source| ReconcileError::Io {
        action,
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("create directory for", e))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| io_err("write", e))?;
    fs::rename(&tmp, path).map_err(|e| io_err("rename into", e))
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
