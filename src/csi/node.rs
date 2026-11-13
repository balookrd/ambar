//! The CSI Node service. `NodeStageVolume` makes this node a member of the
//! volume (a diskless `Client`) when it is none, waits until the DRBD device
//! is usable, formats it only when it is provably a brand-new volume and
//! mounts it. `NodePublishVolume` bind-mounts the staged volume into the pod.
//! `NodeUnpublishVolume` and `NodeUnstageVolume` undo this, and unstage removes
//! a `Client` membership.
//!
//! Every handler may be cut at any await point (the client gives up, the
//! process exits) and kubelet repeats every call, so each RPC is safe to repeat
//! from any intermediate state: the mount table is read first, the membership
//! edits are idempotent, and `mkfs` cannot run twice (below). The parts that
//! change the node run in a spawned task that owns the volume lock, so that a
//! handler dropped by the client does not release the lock while a `mkfs`,
//! `mount` or `drbdadm down` it started is still running.
//!
//! `NodeStageVolume`, in order (the first failing step answers):
//!
//! | Step | Condition | Code |
//! |---|---|---|
//! | 1 | the agent has not finished its first pass (`ready`) | `UNAVAILABLE` |
//! | 2 | empty or relative path, a name that is not a volume name, a capability that is not `mount` or whose `fs_type` is not empty or `ext4`, a mount flag outside `[A-Za-z0-9_=.-]+` | `INVALID_ARGUMENT` |
//! | 3 | no object, or the object is being deleted | `NOT_FOUND` |
//! | 4 | the staging path already has `/dev/drbd<minor>` mounted: done. Another source there | `FAILED_PRECONDITION` |
//! | 5 | this node is not in `spec.nodes`: add it as a `Client` (`join_as_client`, a JSON patch with uid and node-list tests). Not in `ambar-nodes` | `FAILED_PRECONDITION` |
//! | 5 | no room (`maxPeers`), `nodeId` or address taken by another member | `RESOURCE_EXHAUSTED` |
//! | 6 | `Agent::reconcile_one` refuses the volume | `FAILED_PRECONDITION` |
//! | 6, 7 | the API server or the pass fails; the resource is not usable within `stage_wait` (`reconcile_one` is repeated between the polls while the last pass reported progress) | `UNAVAILABLE` |
//! | 8 | (under the volume lock) the object is gone | `NOT_FOUND` |
//! | 8 | this node has no entry in `status.nodes` of a fresh read | `UNAVAILABLE` |
//! | 8 | this node is not a member of the fresh object | `FAILED_PRECONDITION` |
//! | 9 | signature, `mkfs`, annotation: see below | |
//! | 10 | `mount` fails, the staging path is busy with another source | `FAILED_PRECONDITION` |
//!
//! When `mkfs.ext4` runs. It destroys what is on the device, so it runs only
//! if all three hold:
//!
//! 1. `Mounter::signature` returned `Signature::Empty`. Every error of the
//!    signature check (the device cannot be opened, the checks disagree, it is
//!    not a block device), `Ext4` and `Other(fs)` mean no `mkfs`.
//! 2. The object carries no annotation `ambar.balookrd.github.io/formatted`.
//!    An `Empty` device with the annotation is `FAILED_PRECONDITION` ("the
//!    volume was formatted before but no filesystem is found"); nothing is run.
//! 3. The device was opened and read by `signature` in this same call, under
//!    the volume lock, on the object read in that critical section. Nothing is
//!    remembered between calls.
//!
//! After a successful `mkfs` the annotation is set (a JSON patch with a uid
//! test) and read back; when that fails the call fails and kubelet retries: the
//! retry reads `Ext4`, sets the annotation and mounts. `Ext4` without the
//! annotation (a crash between `mkfs` and the annotation) sets it too. The
//! agent never removes it.
//!
//! Error mapping of the mounter (`mount_status`): `Open { in_use: true }` is
//! `FAILED_PRECONDITION` (open on another node); other `Open`,
//! `AmbiguousSignature`, `NotABlockDevice` and `Io` are `UNAVAILABLE`;
//! `WrongFilesystem`, `Busy`, `StillMounted` and `Command` are
//! `FAILED_PRECONDITION` (the last with stderr); `ModeMismatch` is
//! `ALREADY_EXISTS`.
//!
//! `NodePublishVolume`: empty paths or a bad capability are `INVALID_ARGUMENT`;
//! no mount at the staging path is `FAILED_PRECONDITION`; the target directory
//! is created and the staging path is bind-mounted into it, read-only when the
//! request says so; an existing bind of the other mode is `ALREADY_EXISTS`.
//! `NodeUnpublishVolume` unmounts the target and removes the directory when it
//! is empty; nothing mounted is `OK`.
//!
//! `NodeUnstageVolume`, under the volume lock, in order:
//!
//! 1. unmount the staging path; a failure is `FAILED_PRECONDITION` and stops
//!    here (the object is not touched);
//! 2. read the object; no object, or this node is not a `Client` in it
//!    (`Replica`, `Witness`, not a member): done, nothing but the unmount;
//! 3. a `Client`: take the resource down and remove the local files
//!    (`Reconciler::remove_checked`, guard: no backing file); `drbdadm down`
//!    failing (the device is still open) is `FAILED_PRECONDITION` and the
//!    membership is kept;
//! 4. remove this node's entry from `status.nodes` (`Agent::clear_entry`, the
//!    removal with the verification the reconcile loop uses);
//! 5. remove the `Client` from `spec.nodes` (`leave_as_client`).
//!
//! The resource goes down first and the peers' configuration changes last: the
//! order experiment P8 measured. Known limitation: when the process dies
//! between 3 and 5, the reconcile loop sees a `Client` in `spec.nodes` and
//! brings the resource up again; that membership without a pod stays until a
//! later unstage or an operator removes it.
//!
//! Lock order. `Agent::locks` (in-process, per volume) is taken first, for
//! steps 8 to 10 of the stage and for the whole unstage; inside it
//! `Reconciler::remove_checked` takes the flock file of the volume. The wait
//! of the stage (steps 6, 7) runs without the in-process lock, because
//! `Agent::reconcile_one` takes it itself: it must never be called while this
//! code holds it.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::time::{Instant, sleep};
use tonic::{Code, Request, Response, Status};

use super::controller::TOPOLOGY_KEY;
use super::proto::node_server::Node;
use super::proto::node_service_capability::{self, rpc};
use super::proto::volume_capability::{AccessType, MountVolume};
use super::proto::{
    NodeExpandVolumeRequest, NodeExpandVolumeResponse, NodeGetCapabilitiesRequest, NodeGetCapabilitiesResponse,
    NodeGetInfoRequest, NodeGetInfoResponse, NodeGetStorageHealthRequest, NodeGetStorageHealthResponse,
    NodeGetVolumeHealthRequest, NodeGetVolumeHealthResponse, NodeGetVolumeStatsRequest, NodeGetVolumeStatsResponse,
    NodePublishVolumeRequest, NodePublishVolumeResponse, NodeServiceCapability, NodeStageVolumeRequest,
    NodeStageVolumeResponse, NodeUnpublishVolumeRequest, NodeUnpublishVolumeResponse, NodeUnstageVolumeRequest,
    NodeUnstageVolumeResponse, Topology, VolumeCapability,
};
use crate::agent::kube::{ApiError, EDIT_ATTEMPTS, FORMATTED_ANNOTATION};
use crate::agent::local::local;
use crate::agent::nodes::NodeMap;
use crate::agent::run::{Agent, AgentError, Outcome, has_entry, uid_of};
use crate::agent::status::usable;
use crate::crd::{NodeKind, ReplicatedVolume};
use crate::exec::command_line;
use crate::membership::{Join, Leave, MembershipError, join_as_client, leave_as_client};
use crate::model::valid_volume_name;
use crate::mount::{MountEntry, MountError, Mounter, Signature, mounted_at, parse_mountinfo};
use crate::reconcile::{ReconcileError, Reconciler};

#[derive(Clone)]
pub struct NodeService {
    pub agent: Arc<Agent>,
    pub nodes: Arc<dyn NodeMap>,
    /// `/proc/self/mountinfo`.
    pub mountinfo: PathBuf,
    /// The directory of the DRBD devices, `/dev`: the device of a volume is
    /// `<dev_dir>/drbd<minor>`, never a symlink.
    pub dev_dir: PathBuf,
    /// Accept a regular file as the device. `false` in production; tests only.
    pub allow_regular_files: bool,
    /// How long `NodeStageVolume` waits for the device to become usable.
    pub stage_wait: Duration,
    /// Interval between the status reads of that wait.
    pub poll: Duration,
}

impl NodeService {
    /// The production settings.
    pub fn new(agent: Arc<Agent>, nodes: Arc<dyn NodeMap>) -> Self {
        Self {
            agent,
            nodes,
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
            dev_dir: PathBuf::from("/dev"),
            allow_regular_files: false,
            stage_wait: Duration::from_secs(30),
            poll: Duration::from_millis(500),
        }
    }
}

// ---- status mapping ----

/// What a failed call to the API server means to the caller: a refused patch
/// is a bug of this build, anything else is worth a retry.
fn api_status(context: &str, error: ApiError) -> Status {
    match error {
        ApiError::Rejected(why) => Status::internal(format!("{context}: the API server rejected the request: {why}")),
        other => Status::unavailable(format!("{context}: {other}")),
    }
}

fn agent_status(context: &str, error: AgentError) -> Status {
    Status::unavailable(format!("{context}: {error}"))
}

/// Error mapping of the mounter (module doc).
pub fn mount_status(error: MountError) -> Status {
    let text = error.to_string();
    match error {
        MountError::Open { in_use: true, .. } => {
            Status::failed_precondition(format!("the volume is open on another node: {text}"))
        }
        MountError::Open { .. }
        | MountError::AmbiguousSignature { .. }
        | MountError::NotABlockDevice { .. }
        | MountError::Io { .. } => Status::unavailable(text),
        MountError::WrongFilesystem { .. }
        | MountError::Busy { .. }
        | MountError::StillMounted { .. }
        | MountError::Command { .. } => Status::failed_precondition(text),
        MountError::ModeMismatch { .. } => Status::already_exists(text),
    }
}

fn membership_status(error: MembershipError) -> Status {
    match error {
        MembershipError::UnknownNode(_) => Status::failed_precondition(error.to_string()),
        MembershipError::NoRoom { .. } | MembershipError::NodeIdTaken { .. } | MembershipError::AddressTaken { .. } => {
            Status::resource_exhausted(error.to_string())
        }
    }
}

// ---- the decision about `mkfs` ----

/// What the stage does about the filesystem of the device.
#[derive(Debug, PartialEq, Eq)]
pub enum Format {
    /// A filesystem and the annotation: mount.
    Mount,
    /// A filesystem without the annotation (a crash after `mkfs`): set it, mount.
    MarkThenMount,
    /// Provably a new volume: `mkfs`, set the annotation, mount.
    FormatThenMark,
}

/// The only place that lets `mkfs` run: `Format::FormatThenMark` comes from an
/// `Ok(Signature::Empty)` of a signature check made in this call, on an object
/// without the annotation, and from nothing else (module doc).
pub fn plan_format(signature: Result<Signature, MountError>, annotated: bool) -> Result<Format, Status> {
    match signature {
        Ok(Signature::Ext4) => Ok(if annotated {
            Format::Mount
        } else {
            Format::MarkThenMount
        }),
        Ok(Signature::Empty) if annotated => Err(Status::failed_precondition(
            "the volume was formatted before but no filesystem is found; refusing to format again",
        )),
        Ok(Signature::Empty) => Ok(Format::FormatThenMark),
        Ok(Signature::Other(found)) => Err(Status::failed_precondition(format!(
            "the volume holds {found}, not ext4; refusing to format it"
        ))),
        Err(error) => Err(mount_status(error)),
    }
}

// ---- requests ----

fn volume_name(id: &str) -> Result<String, Status> {
    if id.is_empty() {
        return Err(Status::invalid_argument("volume_id is empty"));
    }
    if !valid_volume_name(id) {
        return Err(Status::invalid_argument(format!("{id:?} is not a valid volume name")));
    }
    Ok(id.to_string())
}

fn absolute(field: &str, value: &str) -> Result<PathBuf, Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!("{field} is empty")));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(Status::invalid_argument(format!("{field} must be an absolute path")));
    }
    Ok(path)
}

/// The `mount` access type with an empty or `ext4` file system.
fn mount_capability(capability: Option<&VolumeCapability>) -> Result<&MountVolume, Status> {
    let Some(capability) = capability else {
        return Err(Status::invalid_argument("volume_capability is required"));
    };
    match &capability.access_type {
        Some(AccessType::Mount(mount)) => {
            if !mount.fs_type.is_empty() && mount.fs_type != "ext4" {
                return Err(Status::invalid_argument(format!(
                    "fs_type {:?} is not supported, only ext4",
                    mount.fs_type
                )));
            }
            Ok(mount)
        }
        Some(AccessType::Block(_)) => Err(Status::invalid_argument("block volumes are not supported, only mount")),
        None => Err(Status::invalid_argument("the volume capability has no access type")),
    }
}

/// `-o` argument for `mount(8)` out of `mount_flags`; `None` for no flags.
/// No shell is involved, but the flags reach `mount(8)`: every flag must match
/// `[A-Za-z0-9_=.-]+`. The flags may be sensitive, so a message names the
/// position, not the text.
pub fn mount_options(flags: &[String]) -> Result<Option<String>, Status> {
    for (index, flag) in flags.iter().enumerate() {
        let valid = !flag.is_empty()
            && flag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'=' | b'.' | b'-'));
        if !valid {
            return Err(Status::invalid_argument(format!(
                "mount_flags[{index}] is empty or has characters outside [A-Za-z0-9_=.-]"
            )));
        }
    }
    Ok((!flags.is_empty()).then(|| flags.join(",")))
}

// ---- the mount table ----

/// The path as the mount table spells it (`fs::canonicalize`; a path that does
/// not exist is taken as given) and the entry mounted there.
fn entry_at(mountinfo: &Path, path: &Path) -> Result<(PathBuf, Option<MountEntry>), MountError> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(e) if e.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(source) => {
            return Err(MountError::Io {
                action: "resolve",
                path: path.display().to_string(),
                source,
            });
        }
    };
    let text = std::fs::read_to_string(mountinfo).map_err(|source| MountError::Io {
        action: "read",
        path: mountinfo.display().to_string(),
        source,
    })?;
    let entry = mounted_at(&parse_mountinfo(&text), &resolved).cloned();
    Ok((resolved, entry))
}

/// `Mounter::mount_device` with mount options, which it has no parameter for:
/// the same table check first, then `mount -t ext4 -o <options> <device> <target>`.
fn mount_with_options(mounter: &Mounter<'_>, device: &Path, target: &Path, options: &str) -> Result<(), MountError> {
    let device_name = device.to_string_lossy().into_owned();
    let (target, existing) = entry_at(mounter.mountinfo, target)?;
    if let Some(entry) = existing {
        if entry.source != device_name {
            return Err(MountError::Busy {
                target: target.display().to_string(),
                source_mounted: entry.source,
            });
        }
        if entry.fstype != "ext4" {
            return Err(MountError::WrongFilesystem {
                device: device_name,
                found: entry.fstype,
            });
        }
        return Ok(());
    }
    let target_name = target.to_string_lossy().into_owned();
    let args = ["-t", "ext4", "-o", options, device_name.as_str(), target_name.as_str()];
    let command = command_line("mount", &args);
    let output = mounter.runner.run("mount", &args).map_err(|e| MountError::Command {
        command: command.clone(),
        code: None,
        stderr: e.to_string(),
    })?;
    if output.success() {
        Ok(())
    } else {
        Err(MountError::Command {
            command,
            code: Some(output.code),
            stderr: output.stderr.trim().to_string(),
        })
    }
}

fn is_annotated(object: &ReplicatedVolume) -> bool {
    object
        .metadata
        .annotations
        .as_ref()
        .is_some_and(|a| a.contains_key(FORMATTED_ANNOTATION))
}

/// Runs `work` in a task of its own and waits for it: a handler that is
/// dropped half way (the client gave up) does not stop the work or release the
/// lock the work holds.
async fn detached<T: Send + 'static>(
    work: impl Future<Output = Result<T, Status>> + Send + 'static,
) -> Result<T, Status> {
    tokio::spawn(work)
        .await
        .map_err(|e| Status::internal(format!("the handler task failed: {e}")))?
}

// ---- the service ----

impl NodeService {
    fn device(&self, object: &ReplicatedVolume) -> PathBuf {
        self.dev_dir.join(format!("drbd{}", object.spec.minor))
    }

    /// Runs `work` with a mounter on the blocking pool.
    async fn mount_raw<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Mounter<'_>) -> Result<T, MountError> + Send + 'static,
    ) -> Result<T, MountError> {
        let runner = self.agent.runner.clone();
        let mountinfo = self.mountinfo.clone();
        let allow_regular_files = self.allow_regular_files;
        let joined = tokio::task::spawn_blocking(move || {
            work(&Mounter {
                runner: &*runner,
                mountinfo: &mountinfo,
                allow_regular_files,
            })
        })
        .await;
        joined.unwrap_or_else(|e| {
            Err(MountError::Io {
                action: "run",
                path: "the mount task".to_string(),
                source: io::Error::other(e.to_string()),
            })
        })
    }

    /// The object, or `NOT_FOUND` when it does not exist or is being deleted.
    async fn live_object(&self, volume: &str) -> Result<ReplicatedVolume, Status> {
        let object = self
            .agent
            .volumes
            .get(volume)
            .await
            .map_err(|e| api_status("cannot read the volume", e))?
            .ok_or_else(|| Status::not_found(format!("volume {volume:?} does not exist")))?;
        if object.metadata.deletion_timestamp.is_some() {
            return Err(Status::not_found(format!("volume {volume:?} is being deleted")));
        }
        Ok(object)
    }

    /// Whether `staging` has `device` mounted. Another source there is an error.
    async fn staged(&self, staging: &Path, device: &Path) -> Result<bool, Status> {
        let path = staging.to_path_buf();
        let (_, entry) = self
            .mount_raw(move |m| entry_at(m.mountinfo, &path))
            .await
            .map_err(mount_status)?;
        match entry {
            None => Ok(false),
            Some(entry) if entry.source == device.to_string_lossy() => Ok(true),
            Some(entry) => Err(Status::failed_precondition(format!(
                "{} is already a mount point of {}",
                staging.display(),
                entry.source
            ))),
        }
    }

    /// Step 5: this node becomes a `Client` of the volume unless it is a member.
    async fn join(&self, volume: &str) -> Result<(), Status> {
        let node = &self.agent.node;
        for _ in 0..EDIT_ATTEMPTS {
            let object = self.live_object(volume).await?;
            if object.spec.nodes.iter().any(|n| n.name == *node) {
                return Ok(());
            }
            let cluster = self
                .nodes
                .load()
                .await
                .map_err(|e| api_status("cannot read the node map", e))?;
            let mut spec = object.spec.clone();
            match join_as_client(&mut spec, node, &cluster) {
                Ok(Join::AlreadyMember(_)) => return Ok(()),
                Ok(Join::Added) => {}
                Err(e) => return Err(membership_status(e)),
            }
            let uid = uid_of(&object).map_err(|e| agent_status("cannot join the volume", e))?;
            let edited = self
                .agent
                .volumes
                .replace_members(volume, &uid, &object.spec.nodes, &spec.nodes)
                .await
                .map_err(|e| api_status("cannot add this node to the volume", e))?;
            if edited {
                return Ok(());
            }
        }
        Err(Status::unavailable(format!(
            "volume {volume:?} kept changing while this node was being added; retry"
        )))
    }

    /// One `reconcile_one` pass.
    async fn pass(&self, volume: &str) -> Result<Outcome, Status> {
        match self.agent.reconcile_one(volume).await {
            Ok(Outcome::Refused(refusal)) => Err(Status::failed_precondition(format!(
                "the agent refuses volume {volume:?}: {refusal}"
            ))),
            Ok(Outcome::Gone) => Err(Status::not_found(format!("volume {volume:?} does not exist"))),
            Ok(outcome) => Ok(outcome),
            Err(e) => Err(agent_status(
                &format!("volume {volume:?} cannot be set up on this node"),
                e,
            )),
        }
    }

    /// Steps 6 and 7: a pass, then the status every `poll` until it is usable;
    /// the pass is repeated between the polls while the last one reported
    /// progress.
    async fn wait_usable(&self, volume: &str) -> Result<(), Status> {
        let deadline = Instant::now() + self.stage_wait;
        let mut outcome = self.pass(volume).await?;
        loop {
            let last = match self.agent.read_status(volume).await {
                Ok(status) if usable(status.as_ref()) => return Ok(()),
                Ok(_) => "the resource has no quorum or no UpToDate disk in reach".to_string(),
                Err(e) => format!("cannot read the DRBD status: {e}"),
            };
            let now = Instant::now();
            if now >= deadline {
                return Err(Status::unavailable(format!(
                    "volume {volume:?} is not usable on this node after {:?}: {last}",
                    self.stage_wait
                )));
            }
            sleep(self.poll.min(deadline - now)).await;
            if outcome == Outcome::Progress {
                outcome = match self.pass(volume).await {
                    Ok(outcome) => outcome,
                    // An error of the API or of the pass may pass: try again until the deadline.
                    Err(status) if status.code() == Code::Unavailable => Outcome::Progress,
                    Err(status) => return Err(status),
                };
            }
        }
    }

    /// Sets the annotation `FORMATTED_ANNOTATION` and reads it back.
    async fn mark_formatted(&self, object: &ReplicatedVolume, volume: &str) -> Result<(), Status> {
        let uid = uid_of(object).map_err(|e| agent_status("cannot mark the volume as formatted", e))?;
        self.agent
            .volumes
            .mark_formatted(volume, &uid)
            .await
            .map_err(|e| api_status("cannot mark the volume as formatted", e))?;
        let fresh = self
            .agent
            .volumes
            .get(volume)
            .await
            .map_err(|e| api_status("cannot read the volume back after marking it as formatted", e))?;
        if fresh
            .as_ref()
            .is_some_and(|o| o.metadata.uid == object.metadata.uid && is_annotated(o))
        {
            Ok(())
        } else {
            Err(Status::unavailable(format!(
                "the annotation {FORMATTED_ANNOTATION} is not on volume {volume:?} after it was set; retry"
            )))
        }
    }

    /// Steps 8 to 10, under the volume lock (module doc).
    async fn stage_locked(&self, volume: String, staging: PathBuf, options: Option<String>) -> Result<(), Status> {
        let _guard = self.agent.locks.lock(&volume).await;
        let object = self.live_object(&volume).await?;
        // Peers decide from the presence of this entry whether this node may
        // still use the volume: it must be on the object before the device is
        // opened. `reconcile_one` writes it.
        if !has_entry(&object, &self.agent.node) {
            return Err(Status::unavailable(format!(
                "this node has not reported on volume {volume:?} yet (no entry in status.nodes); retry"
            )));
        }
        if !object.spec.nodes.iter().any(|n| n.name == self.agent.node) {
            return Err(Status::failed_precondition(format!(
                "this node is not a member of volume {volume:?} any more"
            )));
        }
        let device = self.device(&object);
        if self.staged(&staging, &device).await? {
            return Ok(());
        }
        let probe = device.clone();
        let signature = self.mount_raw(move |m| m.signature(&probe)).await;
        match plan_format(signature, is_annotated(&object))? {
            Format::Mount => {}
            Format::MarkThenMount => self.mark_formatted(&object, &volume).await?,
            Format::FormatThenMark => {
                let target = device.clone();
                self.mount_raw(move |m| m.mkfs_ext4(&target))
                    .await
                    .map_err(mount_status)?;
                self.mark_formatted(&object, &volume).await?;
            }
        }
        std::fs::create_dir_all(&staging)
            .map_err(|e| Status::unavailable(format!("cannot create {}: {e}", staging.display())))?;
        self.mount_raw(move |m| match &options {
            None => m.mount_device(&device, &staging),
            Some(options) => mount_with_options(m, &device, &staging, options),
        })
        .await
        .map_err(mount_status)
    }

    /// Takes the resource down and removes the local files of a `Client`
    /// (module doc, step 3 of the unstage). The guard: no backing file.
    async fn clean_local(&self, volume: &str) -> Result<(), Status> {
        let name = volume.to_string();
        let result = self
            .agent
            .blocking(move |paths, runner| {
                let guard = || match local(paths, &name) {
                    Ok(state) if state.image => Err("a backing file exists on this node".to_string()),
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("cannot inspect the backing file: {e}")),
                };
                Reconciler { paths, runner }.remove_checked(&name, &guard)
            })
            .await
            .map_err(|e| agent_status("cannot clean up the volume on this node", e))?;
        match result {
            Ok(_) => Ok(()),
            Err(error @ ReconcileError::Busy { .. }) => Err(Status::unavailable(error.to_string())),
            Err(error) => Err(Status::failed_precondition(format!(
                "the resource of volume {volume:?} could not be taken down on this node (is the device still open?): \
                 {error}"
            ))),
        }
    }

    /// Step 5 of the unstage: removes this node's `Client` entry from `spec.nodes`.
    async fn leave(&self, volume: &str) -> Result<(), Status> {
        let node = &self.agent.node;
        for _ in 0..EDIT_ATTEMPTS {
            let Some(object) = self
                .agent
                .volumes
                .get(volume)
                .await
                .map_err(|e| api_status("cannot read the volume", e))?
            else {
                return Ok(());
            };
            let mut spec = object.spec.clone();
            if leave_as_client(&mut spec, node) != Leave::Removed {
                return Ok(());
            }
            let uid = uid_of(&object).map_err(|e| agent_status("cannot leave the volume", e))?;
            let edited = self
                .agent
                .volumes
                .replace_members(volume, &uid, &object.spec.nodes, &spec.nodes)
                .await
                .map_err(|e| api_status("cannot remove this node from the volume", e))?;
            if edited {
                return Ok(());
            }
        }
        Err(Status::unavailable(format!(
            "volume {volume:?} kept changing while this node was being removed; retry"
        )))
    }

    /// The whole unstage, under the volume lock (module doc).
    async fn unstage_locked(&self, volume: String, staging: PathBuf) -> Result<(), Status> {
        let _guard = self.agent.locks.lock(&volume).await;
        self.mount_raw(move |m| m.unmount(&staging))
            .await
            .map_err(mount_status)?;
        let Some(object) = self
            .agent
            .volumes
            .get(&volume)
            .await
            .map_err(|e| api_status("cannot read the volume", e))?
        else {
            return Ok(());
        };
        let is_client = object
            .spec
            .nodes
            .iter()
            .any(|n| n.name == self.agent.node && n.kind == NodeKind::Client);
        if !is_client {
            return Ok(());
        }
        self.clean_local(&volume).await?;
        let cleared = self
            .agent
            .clear_entry(&volume, &object)
            .await
            .map_err(|e| agent_status("cannot remove this node's entry from status.nodes", e))?;
        if !cleared {
            return Err(Status::unavailable(format!(
                "the entry of this node in status.nodes of volume {volume:?} could not be removed; the membership is \
                 kept, retry"
            )));
        }
        self.leave(&volume).await
    }
}

#[tonic::async_trait]
impl Node for NodeService {
    async fn node_stage_volume(
        &self,
        request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        let request = request.into_inner();
        if !self.agent.ready.load(Ordering::Acquire) {
            return Err(Status::unavailable("the agent has not finished its first pass yet"));
        }
        let volume = volume_name(&request.volume_id)?;
        let staging = absolute("staging_target_path", &request.staging_target_path)?;
        let capability = mount_capability(request.volume_capability.as_ref())?;
        let options = mount_options(&capability.mount_flags)?;

        let object = self.live_object(&volume).await?;
        if self.staged(&staging, &self.device(&object)).await? {
            return Ok(Response::new(NodeStageVolumeResponse {}));
        }
        self.join(&volume).await?;
        self.wait_usable(&volume).await?;
        let this = self.clone();
        detached(async move { this.stage_locked(volume, staging, options).await }).await?;
        Ok(Response::new(NodeStageVolumeResponse {}))
    }

    async fn node_unstage_volume(
        &self,
        request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        let request = request.into_inner();
        let volume = volume_name(&request.volume_id)?;
        let staging = absolute("staging_target_path", &request.staging_target_path)?;
        let this = self.clone();
        detached(async move { this.unstage_locked(volume, staging).await }).await?;
        Ok(Response::new(NodeUnstageVolumeResponse {}))
    }

    async fn node_publish_volume(
        &self,
        request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        let request = request.into_inner();
        volume_name(&request.volume_id)?;
        let staging = absolute("staging_target_path", &request.staging_target_path)?;
        let target = absolute("target_path", &request.target_path)?;
        mount_capability(request.volume_capability.as_ref())?;
        let read_only = request.readonly;

        let path = staging.clone();
        let (_, entry) = self
            .mount_raw(move |m| entry_at(m.mountinfo, &path))
            .await
            .map_err(mount_status)?;
        if entry.is_none() {
            return Err(Status::failed_precondition(format!(
                "{} has no mount: the volume is not staged",
                staging.display()
            )));
        }
        std::fs::create_dir_all(&target)
            .map_err(|e| Status::unavailable(format!("cannot create {}: {e}", target.display())))?;
        self.mount_raw(move |m| m.bind(&staging, &target, read_only))
            .await
            .map_err(mount_status)?;
        Ok(Response::new(NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        let request = request.into_inner();
        volume_name(&request.volume_id)?;
        let target = absolute("target_path", &request.target_path)?;

        let path = target.clone();
        self.mount_raw(move |m| m.unmount(&path)).await.map_err(mount_status)?;
        // Only an empty directory goes: `remove_dir` refuses anything else.
        match std::fs::remove_dir(&target) {
            Ok(()) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty) => {}
            Err(e) => return Err(Status::unavailable(format!("cannot remove {}: {e}", target.display()))),
        }
        Ok(Response::new(NodeUnpublishVolumeResponse {}))
    }

    async fn node_get_volume_stats(
        &self,
        _request: Request<NodeGetVolumeStatsRequest>,
    ) -> Result<Response<NodeGetVolumeStatsResponse>, Status> {
        Err(Status::unimplemented("node_get_volume_stats is not implemented yet"))
    }

    async fn node_get_volume_health(
        &self,
        _request: Request<NodeGetVolumeHealthRequest>,
    ) -> Result<Response<NodeGetVolumeHealthResponse>, Status> {
        Err(Status::unimplemented("node_get_volume_health is not implemented yet"))
    }

    async fn node_get_storage_health(
        &self,
        _request: Request<NodeGetStorageHealthRequest>,
    ) -> Result<Response<NodeGetStorageHealthResponse>, Status> {
        Err(Status::unimplemented("node_get_storage_health is not implemented yet"))
    }

    async fn node_expand_volume(
        &self,
        _request: Request<NodeExpandVolumeRequest>,
    ) -> Result<Response<NodeExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("node_expand_volume is not implemented yet"))
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<NodeGetCapabilitiesRequest>,
    ) -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        Ok(Response::new(NodeGetCapabilitiesResponse {
            capabilities: vec![NodeServiceCapability {
                r#type: Some(node_service_capability::Type::Rpc(node_service_capability::Rpc {
                    r#type: rpc::Type::StageUnstageVolume as i32,
                })),
            }],
        }))
    }

    async fn node_get_info(
        &self,
        _request: Request<NodeGetInfoRequest>,
    ) -> Result<Response<NodeGetInfoResponse>, Status> {
        Ok(Response::new(NodeGetInfoResponse {
            node_id: self.agent.node.clone(),
            max_volumes_per_node: 0,
            accessible_topology: Some(Topology {
                segments: [(TOPOLOGY_KEY.to_string(), self.agent.node.clone())].into(),
            }),
        }))
    }
}

#[cfg(test)]
#[path = "tests/node.rs"]
mod tests;
