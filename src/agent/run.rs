//! The agent's reconcile loop: for a `ReplicatedVolume` name, read the
//! object and the local state, call `decide`, carry the task out with
//! `Reconciler` and report this node's entry of `status.nodes`.
//!
//! Data-safety rules this module enforces on top of `decide`.
//!
//! The node's copy is deleted (and a resource taken down) only through
//! `Reconciler::remove_checked`, which takes the volume `flock` and then calls
//! a guard. These are all the paths to it:
//!
//! | Path | Task | Checked before the lock | Guard, under the lock |
//! |---|---|---|---|
//! | `delete_replica`, peer checks | `DeleteReplica`, resource configured in the kernel, any file state | the DRBD status can be read; rules 1-3 below on the object read at the start of the pass | the DRBD status is read again; rules 1-3a below; 3b uses the same object |
//! | `delete_replica`, no data | `DeleteReplica`, resource not configured, and `prove_no_data` shows the node holds no data (rows 3 and 5 of the table below) | the resource is not configured | under the volume `flock`: the DRBD status is read again and the resource is still not configured, then `prove_no_data` runs; then `Reconciler::remove_unconfigured` reads the status once more and refuses a resource that has appeared |
//! | `retire_replica`, R26 | `RetireReplica`, resource configured in the kernel, any file state | resource matches description (otherwise `apply` first); DRBD status read; R26 (2)–(4) below | DRBD status is read again; R26 (2)–(4) verified under lock |
//! | `retire_replica`, no data | `RetireReplica`, resource not configured, and `prove_no_data` shows the node holds no data | the resource is not configured | under the volume `flock`: the DRBD status is read again and resource still not configured, then `prove_no_data` runs; `remove_unconfigured` |
//! | `clean_diskless` | `CleanDiskless` | nothing | no backing file exists |
//!
//! `DeleteReplica` by what this node holds. The first matching row applies;
//! a state that matches no row that ends in a removal keeps the copy.
//! "Configured" is `drbdsetup status` showing the resource, whatever its disk
//! state.
//!
//! | Resource in the kernel | Backing file | Other facts | Action |
//! |---|---|---|---|
//! | configured | any (absent, no ready marker, ready marker), when the node has an image or traces | | peer checks, rules 1-3 below: DRBD holds the disk, so the file state does not matter. With nothing local `decide` returns `DropFinalizer` and this table is not reached |
//! | not configured | ready marker | | `bring_up`: `Reconciler::apply` with the object's description and `initialSync: Done`, status written, `Progress` (1 s); nothing is deleted. A description that cannot be converted or a failed `apply`: the copy is kept, the error is in the status `message`, `Kept` (30 s) |
//! | not configured | absent | no loop device is attached to the image path (`local::loops_holding` is empty) | no data: removed without the peer checks |
//! | not configured | absent | a loop device still holds the deleted file | kept, `Kept`: "the backing file is gone but a loop device still holds it" |
//! | not configured | no ready marker | the synced marker is absent, the device link `<dev_dir>/<volume>` points at a loop device that `loop_holders` reports for the image under its live path (not `(deleted)`), and `Drbd::metadata` (`drbdadm dump-md`) is `Absent` or `Fresh` | no data: removed without the peer checks |
//! | not configured | no ready marker | the synced marker exists, or the device link is missing or points at another loop device or at one that holds a deleted file, or the metadata is `Used`, or the check fails | kept, `Kept`; the message names the reason (same wording family as `ReadyMarkerLost` and `MetadataPresent` of `reconcile`); the operator decides |
//!
//! Every fact of "no data" is read, never assumed. A path that cannot be
//! inspected, a `losetup` or `drbdadm dump-md` that fails (`dump-md` needs the
//! resource file, the device link and an attached loop device; nothing is
//! prepared on the deletion path), a `losetup --list` line that is not a device
//! followed by a backing file or whose backing file ends in `*` (util-linux cut
//! a long name) and a status that cannot be read all count as "cannot be
//! shown": the copy stays. `dump-md` reads whatever the device link points at,
//! so the link is checked against the loop devices of this volume's image first. `losetup -j <path>` is not used for the
//! loop devices of a missing file: it finds files by path and says nothing for
//! a path that no longer exists, while `losetup --list` shows the device with
//! `<path> (deleted)` (checked on the stand). The listing is read by
//! `backing::loop_holders`, for the proof here (through `local::loops_holding`)
//! and for the detach in `Reconciler::remove_checked`, so both see the same
//! devices, also with the volumes directory behind a symbolic link.
//!
//! Why the bring-up: after a reboot the resource of a deleting object is not
//! configured, so this node sees no peers and the peers see it not connected
//! with its entry still in `status.nodes`; each waits for the other and nobody
//! deletes (Task 8 fix round 1, stand scenario 3). With the resource up the
//! peers see this node `Connected` and `Secondary` and this node sees theirs.
//! `apply` of a ready file plans an attach of the loop device, the device
//! link, the resource file, `up` or `adjust`; it never plans `create-md`, and
//! `new-current-uuid` only for `initialSync: Skip`, which the bring-up turns
//! off.
//!
//! The entry of this node in `status.nodes` is removed with
//! `Agent::clear_entry`: an empty server-side apply, a read of the object, and
//! when the entry is still there (another field manager co-owns it) a JSON
//! patch on the status subresource (see `kube`). While the entry cannot be
//! removed the pass is `Kept` (30 s), the finalizer of this node stays, and
//! the next pass tries again.
//!
//! The delay after a kept copy. `Outcome::KeptDrbd` (5 s) is returned when the
//! reason is a DRBD state that changes without an event on the object: this
//! node is Primary, a peer is Primary, a peer is not connected (the reasons of
//! `keep_reasons`, before and under the lock). Every other kept copy is
//! `Outcome::Kept` (30 s). The delay does not change when a copy may be deleted.
//!
//! `ambar remove` (the CLI) calls `Reconciler::remove`, which has no guard:
//! it is an operator's decision and bypasses these checks by design.
//!
//! `Task::DeleteReplica` by the peer checks (extends R18, R10). The copy is
//! deleted only if all of these hold; otherwise it is kept, the reason goes
//! into the `message` of this node's `status.nodes` entry and the pass is
//! repeated after 5 s when the reason is a DRBD state (`Outcome::KeptDrbd`),
//! after 30 s otherwise (`Outcome::Kept`), or earlier, when the object changes:
//!
//! 1. the local DRBD status was read without error;
//! 2. this node is not `Primary`;
//! 3. for every other node of `spec.nodes`, of any kind, (a) the local status
//!    shows a connection to it in the state `Connected` with `peer_role`
//!    `Secondary`, or (b) the `status.nodes` of the object read at the start
//!    of the pass has no entry for it. A node removes its entry when it has
//!    cleaned up (a `Replica` before it drops its finalizer, a `Witness` or
//!    `Client` when it cleans up for a deleting object), and a node that never
//!    reported never had the volume up.
//!
//!    A node that is not `Connected` and still has an entry blocks the
//!    deletion, whatever role the entry shows. A connection whose `peer_role`
//!    is `Primary` blocks in every connection state and whatever the entries
//!    say: that is the old R18 check and 3b does not weaken it.
//!
//! `Task::RetireReplica` (R26). The copy is deleted only if:
//! 1. The resource is brought to the description first: if the file differs,
//!    is not applied, or the status does not match `spec`, `apply` runs with
//!    `initialSync: Done` and the pass makes `Progress`.
//! 2. Once the resource matches the description:
//!    (a) this node is not `Primary`;
//!    (b) every other member of `spec.nodes` is `Connected`;
//!    (c) every other `Replica` member has its peer-disk `UpToDate`.
//! 3. Order of completion:
//!    (1) local copy is removed (`remove_checked` / `remove_unconfigured`);
//!    (2) status entry is removed and confirmed (`clear_entry`);
//!    (3) finalizer is dropped (`drop_finalizer`);
//!    (4) this node's entry is removed from `spec.nodes` (`replace_members`).
//!
//! Known limit: a node that is dead for good keeps its entry and so blocks the
//! deletion of the copies of the others (they see it not connected with its
//! entry present). The way out is manual and deliberate: the operator checks
//! that nobody uses the volume, removes the dead node's entry from
//! `status.nodes` (`kubectl patch rv <name> --subresource=status --type=json
//! -p '[{"op":"remove","path":"/status/nodes/<index>"}]'`) and its finalizer
//! from `metadata.finalizers`; the remaining nodes then delete. A node that is
//! alive but was restarted does not need this: it brings its resource up as
//! above.
//!
//! Known limit: `status.nodes` is edited by hand only with a JSON patch on the
//! status subresource (`--type=json`, as in the command above). A merge patch
//! (`kubectl patch --type=merge`) replaces the whole list and removes the
//! entries of the other nodes, which their peers read as "cleaned up".
//!
//! Other rules:
//!
//! * `Task::CleanDiskless` deletes no backing file: the guard stops it when
//!   one has appeared since the decision.
//! * `Task::Refuse` changes nothing on the node.
//! * The finalizer of this node is removed only after `remove_checked`
//!   succeeded, or (`Task::DropFinalizer`) when nothing was left on the node.
//!   The edits of the object (the finalizer, `initialSync`) are JSON patches
//!   that carry the uid of the object the pass saw (see `kube`).
//! * A node that holds no copy removes its status entry when it cleans up for
//!   a deleting object, also when the entry is all that is left of it.
//!
//! The loop. One task runs `reconcile_one` for the names of a queue, one
//! after another. A name is queued at once when the watch reports its
//! object, and again after a pass: after 1 s when the pass made progress, 30 s
//! when it settled or kept a copy, 60 s when it refused, 5 s on an error; not
//! at all when there is no object and nothing local (`Outcome::Gone`), so the
//! queue does not grow with every name ever seen. Every 30 s the
//! names of all objects and all local leftovers are queued, which finds
//! leftovers of objects that are gone. `ready` becomes true once every name
//! of the first listing has had one pass without an API error, or has failed
//! on the API 3 times; the loop serves events meanwhile, so a name that keeps
//! failing delays nothing else.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::backing::{attach_loop_read_only, detach_loop, has_any_loop_reference, link_device, loop_holders};
use crate::crd::{MemberNode, NodeKind, NodeStatus, ReplicatedVolume, ReplicatedVolumeSpec, to_volume_spec};
use crate::drbd::adm::{Drbd, Metadata};
use crate::drbd::status::{ConnectionState, DiskState, ResourceStatus, Role};
use crate::exec::Runner;
use crate::membership::{collisions, replica_finalizer};
use crate::model::valid_volume_group_name;
use crate::model::{InitialSync, valid_volume_name};
use crate::paths::Paths;
use crate::reconcile::{FileState, ReconcileError, Reconciler, status_matches_spec, write_atomically};

use crate::cli::AgentOptions;
use crate::metrics::{MetricsHolder, NodeMetricData, VolumeMetricData, render_metrics};

use super::cleanup::{self, CleanupTracker, DEFAULT_CLEANUP_INTERVAL};
use super::decide::{Local, ObjectView, Refusal, Task, decide};
use super::kube::{ApiError, EDIT_ATTEMPTS, Volumes, conflict_backoff, mounted_pod_uids_by_volume};
use super::local::{file_state, local, local_volumes, loops_holding, synced_marker_present};
use super::quorum::{QuorumTracker, VolumeQuorumCheck, check_volume_quorum};
use super::standalone::StandaloneTracker;
use super::status::node_status;
use super::taint::{DEFAULT_TAINT_RESYNC_INTERVAL, TaintTracker, reconcile_node_taint};

/// Next pass after one that ran a node-level action or changed the status.
const PROGRESS_DELAY: Duration = Duration::from_secs(1);
/// Next pass after one that found nothing to do.
const SETTLED_DELAY: Duration = Duration::from_secs(30);
/// Next pass after one that kept the copy of a deleting volume: what blocks
/// the deletion (a Primary, a peer that has not cleaned up) does not go away
/// in a second, and a change of the object queues the name at once.
const KEPT_DELAY: Duration = Duration::from_secs(30);
/// Next pass after a copy was kept because of a DRBD state that changes without
/// any event on the object: this node or a peer is Primary, a peer is not
/// connected. Nothing queues the name when such a state ends, so the pass looks
/// again soon; the rule about when a copy may be deleted is not affected.
const KEPT_DRBD_DELAY: Duration = Duration::from_secs(5);
/// Next pass after a refusal.
const REFUSED_DELAY: Duration = Duration::from_secs(60);
/// Next pass after an error.
const ERROR_DELAY: Duration = Duration::from_secs(5);
/// Next pass after skipping a volume whose lock is currently held elsewhere (e.g. CSI mkfs).
const BUSY_VOLUME_RETRY_DELAY: Duration = Duration::from_secs(1);
/// API errors of one name in the first pass after which `ready` stops waiting for it.
const FIRST_PASS_ATTEMPTS: u32 = 3;
/// Period of the pass over every known name.
const FULL_PASS_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Kubernetes API: {0}")]
    Api(#[from] ApiError),
    #[error("volume {volume:?}: {source}")]
    Reconcile {
        volume: String,
        #[source]
        source: ReconcileError,
    },
    #[error("a blocking task failed: {0}")]
    Join(String),
    #[error("{0}")]
    Startup(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do.
    Settled,
    /// No object has this name and the node holds nothing for it: the name is
    /// not queued again (the watch and the periodic pass find it should it
    /// come back).
    Gone,
    /// Something was done or reported, or has to be tried again soon.
    Progress,
    /// The local copy of a deleting volume was kept because deleting it is
    /// not safe yet (the reason is in the status `message`).
    Kept,
    /// Kept for a reason in the DRBD state (`keep_reasons`), which changes
    /// without an event on the object: the pass is repeated after
    /// `KEPT_DRBD_DELAY`, not `KEPT_DELAY`.
    KeptDrbd,
    Refused(Refusal),
}

/// Serialises work on one volume on this node: the reconcile loop and the
/// CSI Node calls take it per volume name. Cloning shares the locks.
#[derive(Clone, Default)]
pub struct VolumeLocks {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

impl VolumeLocks {
    fn entry(&self, volume: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        // A lock that only the map holds has no holder and no waiter.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(volume.to_string()).or_default().clone()
    }

    pub async fn lock(&self, volume: &str) -> OwnedMutexGuard<()> {
        self.entry(volume).lock_owned().await
    }

    pub fn try_lock(&self, volume: &str) -> Option<OwnedMutexGuard<()>> {
        self.entry(volume).try_lock_owned().ok()
    }
}

#[derive(Clone)]
pub struct Agent {
    pub node: String,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    pub volumes: Arc<dyn Volumes>,
    /// Serialises work on one node: the reconcile loop and CSI Node calls take it per volume.
    pub locks: VolumeLocks,
    /// Set once, after the first listing of names has been worked through (see
    /// the module doc). It is never reset: when the API server is lost later
    /// the flag stays true. `NodeStageVolume` answers `UNAVAILABLE` until it is
    /// set; later loss of the API server is not tracked.
    pub ready: Arc<AtomicBool>,
    pub options: AgentOptions,
    pub metrics: MetricsHolder,
    pub is_tainted: Arc<AtomicBool>,
    pub staging: Arc<Mutex<HashMap<String, usize>>>,
}

/// RAII-гарда активного `NodeStageVolume` для тома. Пока хотя бы одна гарда
/// жива, фоновая очистка `Client` не трогает этот том.
pub struct StageGuard {
    staging: Arc<Mutex<HashMap<String, usize>>>,
    volume: String,
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        let mut map = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = map.get_mut(&self.volume) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.volume);
            }
        }
    }
}

fn log_info(volume: &str, text: impl Display) {
    eprintln!("ambar-agent: volume {volume:?}: {text}");
}

fn log_error(volume: &str, text: impl Display) {
    eprintln!("ambar-agent: error: volume {volume:?}: {text}");
}

/// The node name must be the host name: `drbdadm` picks the `on <host>`
/// section of the resource file by it. The volumes directory must exist: the
/// agent never creates it. An optional LVM volume group name must be valid.
pub fn preflight(node: &str, hostname: &str, paths: &Paths, volume_group: Option<&str>) -> Result<(), AgentError> {
    if node != hostname.trim() {
        return Err(AgentError::Startup(format!(
            "NODE_NAME is {node:?} but /proc/sys/kernel/hostname is {:?}; the node name must be the host name",
            hostname.trim()
        )));
    }
    if let Some(vg) = volume_group
        && !valid_volume_group_name(vg)
    {
        return Err(AgentError::Startup(format!(
            "invalid volume group name {vg:?}; must be 1-127 characters [a-zA-Z0-9+_.-] and not start with '-'"
        )));
    }
    if !Path::new(&paths.volumes_dir).is_dir() {
        return Err(AgentError::Startup(format!(
            "the volumes directory {} does not exist; it must exist on the node before the agent runs",
            paths.volumes_dir.display()
        )));
    }
    Ok(())
}

/// Reasons the local copy of a deleting volume must stay; empty when nothing
/// blocks the deletion. `status` is the local DRBD status (`None`: the
/// resource is not configured), `reported` the `status.nodes` of the object.
/// Rules 2 and 3 of the module doc; rule 1 (the status was read) is the
/// caller's.
fn keep_reasons(
    node: &str,
    spec: &ReplicatedVolumeSpec,
    reported: &[NodeStatus],
    status: Option<&ResourceStatus>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if status.is_some_and(|s| s.role == Role::Primary) {
        reasons.push("this node is Primary".to_string());
    }
    let connections = status.map(|s| s.connections.as_slice()).unwrap_or_default();
    for connection in connections.iter().filter(|c| c.peer_role == Role::Primary) {
        reasons.push(format!("peer {} is Primary", connection.name));
    }
    for member in spec.nodes.iter().filter(|m| m.name != node) {
        let connection = connections.iter().find(|c| c.name == member.name);
        let Some(connection) = connection else {
            // Not connected, or the resource is not configured here at all.
            if reported.iter().any(|entry| entry.name == member.name) {
                reasons.push(format!(
                    "peer {} is not connected and has not reported cleanup",
                    member.name
                ));
            }
            continue;
        };
        if connection.peer_role == Role::Primary {
            continue; // named above
        }
        let connected = connection.connection_state == ConnectionState::Connected;
        if connected && connection.peer_role == Role::Secondary {
            continue; // 3a
        }
        if reported.iter().any(|entry| entry.name == member.name) {
            reasons.push(if connected {
                format!(
                    "peer {} is connected with the role {} and has not reported cleanup",
                    member.name, connection.peer_role
                )
            } else {
                format!("peer {} is not connected and has not reported cleanup", member.name)
            });
        }
    }
    reasons
}

/// R26: What has to hold before the copy of a Retiring node can be removed.
///
/// Returns reasons the copy cannot be removed yet:
/// (2) this node must not be Primary;
/// (3) every other member of `spec.nodes` must be Connected;
/// (4) every other `Replica` member must have its peer-disk UpToDate.
pub(crate) fn retire_reasons(spec: &ReplicatedVolumeSpec, node: &str, status: &ResourceStatus) -> Vec<String> {
    let mut reasons = Vec::new();
    if status.role == Role::Primary {
        reasons.push("this node is Primary".to_string());
    }
    for member in spec.nodes.iter().filter(|m| m.name != node) {
        let connection = status.connections.iter().find(|c| c.name == member.name);
        let Some(connection) = connection else {
            reasons.push(format!("peer {} is not connected", member.name));
            continue;
        };
        if connection.connection_state != ConnectionState::Connected {
            reasons.push(format!("peer {} is not connected", member.name));
            continue;
        }
        if member.kind == NodeKind::Replica {
            let disk = connection
                .peer_devices
                .iter()
                .find(|p| p.volume == 0)
                .map(|p| p.peer_disk_state);
            if disk != Some(DiskState::UpToDate) {
                reasons.push(match disk {
                    Some(state) => format!("peer {} disk is {state}", member.name),
                    None => format!("peer {} has no volume 0 device", member.name),
                });
            }
        }
    }
    reasons
}

/// What has to hold before the copy of a deleting volume is deleted (module doc).
enum Check {
    /// The resource is configured, or the copy is a ready one: rules 2 and 3,
    /// with the `status.nodes` the pass read.
    Peers(Vec<NodeStatus>),
    /// The resource is not configured and `prove_no_data` shows the node holds
    /// no data, for a backing file in this state.
    NoData(FileState),
}

/// What has to hold before the copy of a Retiring node is removed (R26).
enum CheckRetire {
    Peers,
    NoData(FileState),
}

/// Shows that a node whose resource is not configured holds no data for
/// `volume`, for a backing file in the state `file` (`Absent` or `Pending`).
/// `Err` is the reason it cannot be shown, in words; whatever fails to be
/// read counts as "cannot be shown". The caller has seen the resource not
/// configured. Nothing is changed and no device is prepared: `dump-md` needs
/// the resource file, the device link and an attached loop device, and when
/// one is missing the check fails and the copy stays.
///
/// * the state of the backing file is still `file`;
/// * `Absent`: no loop device is attached to the path of the file, so the
///   deleted inode is not held;
/// * `Pending`: the synced marker is absent, the device link of the volume is
///   on a loop device that holds this very file (`prove_device_link`), and
///   `Drbd::metadata` is `Absent` or `Fresh`.
fn prove_no_data(
    paths: &Paths,
    runner: &dyn Runner,
    volume: &str,
    minor: u32,
    file: FileState,
    volume_group: Option<&str>,
) -> Result<(), String> {
    let now = file_state(paths, volume, volume_group).map_err(|e| format!("cannot inspect the backing file: {e}"))?;
    if now != file {
        return Err(format!(
            "the backing file was {file:?} at the decision and is {now:?} now; the next pass decides again"
        ));
    }
    match file {
        FileState::Ready => Err("a backing file with the ready marker has appeared since the decision".to_string()),
        FileState::Absent => {
            if volume_group.is_some() {
                Ok(())
            } else {
                let devices = loops_holding(runner, &paths.image(volume))
                    .map_err(|e| format!("cannot tell whether a loop device holds the backing file: {e}"))?;
                if devices.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "the backing file is gone but a loop device still holds it ({}); it is not detached here, check \
                         it by hand",
                        devices.join(", ")
                    ))
                }
            }
        }
        FileState::Pending => {
            let synced =
                synced_marker_present(paths, volume).map_err(|e| format!("cannot inspect the synced marker: {e}"))?;
            if synced {
                return Err(format!(
                    "the backing file has no ready marker but the synced marker exists, so it held data; it is not \
                     deleted: check it by hand, then restore the ready marker or run `ambar remove --name {volume}`"
                ));
            }
            if volume_group.is_some() {
                check_metadata_result(runner, volume)
            } else {
                prove_pending_metadata(paths, runner, volume, minor)
            }
        }
    }
}

fn check_metadata_result(runner: &dyn Runner, volume: &str) -> Result<(), String> {
    match (Drbd { runner }).metadata(volume) {
        Ok(Metadata::Absent | Metadata::Fresh) => Ok(()),
        Ok(Metadata::Used) => Err(
            "the backing file has no ready marker but already holds DRBD metadata; the metadata was not \
             touched, check the file by hand"
                .to_string(),
        ),
        Err(e) => Err(format!("cannot tell whether the backing file holds DRBD metadata: {e}")),
    }
}

fn prove_pending_metadata(paths: &Paths, runner: &dyn Runner, volume: &str, minor: u32) -> Result<(), String> {
    if prove_device_link(paths, runner, volume).is_ok() {
        return check_metadata_result(runner, volume);
    }
    let image = paths.image(volume);
    let has_ref = has_any_loop_reference(runner, &image)
        .map_err(|e| format!("cannot tell which loop device holds the backing file: {e}"))?;
    if has_ref {
        return prove_device_link(paths, runner, volume);
    }

    let dev =
        attach_loop_read_only(runner, &image).map_err(|e| format!("cannot attach the backing file read-only: {e}"))?;

    struct TempCleanup<'a> {
        runner: &'a dyn Runner,
        dev: String,
        link_created: bool,
        link_path: std::path::PathBuf,
        res_created: bool,
        res_path: std::path::PathBuf,
    }

    impl Drop for TempCleanup<'_> {
        fn drop(&mut self) {
            if self.link_created {
                let _ = fs::remove_file(&self.link_path);
            }
            if self.res_created {
                let _ = fs::remove_file(&self.res_path);
            }
            let _ = detach_loop(self.runner, &self.dev);
        }
    }

    let link_path = paths.device_link(volume);
    let link_existed = link_path.is_symlink();
    if let Err(e) = link_device(&link_path, &dev) {
        let _ = detach_loop(runner, &dev);
        return Err(format!("cannot create temporary device link: {e}"));
    }

    let res_path = paths.res_file(volume);
    let res_created = if !res_path.exists() {
        let dummy = format!(
            "resource {volume} {{\n  device /dev/drbd{minor};\n  disk {};\n  meta-disk internal;\n}}\n",
            link_path.display()
        );
        if let Some(parent) = res_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        write_atomically(&res_path, &dummy).is_ok()
    } else {
        false
    };

    let cleanup = TempCleanup {
        runner,
        dev,
        link_created: !link_existed,
        link_path,
        res_created,
        res_path,
    };

    let result = check_metadata_result(runner, volume);
    drop(cleanup);
    result
}

/// Shows that `drbdadm dump-md <volume>` would read this volume's backing
/// file: the device link `<dev_dir>/<volume>` is a symbolic link that points
/// at a loop device which `loop_holders` reports for the image under its live
/// path, matched by a full path or confirmed by inode (`LoopHolder::confirmed`).
/// A missing link, a link elsewhere, a loop device that holds only the deleted
/// inode, one matched by name alone and a listing that cannot be read all fail
/// the proof.
fn prove_device_link(paths: &Paths, runner: &dyn Runner, volume: &str) -> Result<(), String> {
    let link = paths.device_link(volume);
    let target = match fs::read_link(&link) {
        Ok(target) => target.to_string_lossy().into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "the device link {} is missing, so `drbdadm dump-md` cannot be shown to read the backing file; check \
                 it by hand",
                link.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "the device link {} cannot be read as a symbolic link ({e}); check it by hand",
                link.display()
            ));
        }
    };
    let holders = loop_holders(runner, &paths.image(volume))
        .map_err(|e| format!("cannot tell which loop device holds the backing file: {e}"))?;
    if holders.iter().any(|h| h.device == target && !h.deleted && h.confirmed) {
        return Ok(());
    }
    // Matched by the name `/<name>.img` alone, with nothing to confirm it by inode.
    if holders.iter().any(|h| h.device == target && !h.deleted) {
        return Err(format!(
            "the device link {} points at {target}, which is matched to the backing file by its name only and is not \
             confirmed by inode (the file is missing); check it by hand",
            link.display()
        ));
    }
    if holders.iter().any(|h| h.device == target) {
        return Err(format!(
            "the device link {} points at {target}, which holds a deleted file under the name of the backing file, \
             not the file itself; check it by hand",
            link.display()
        ));
    }
    if holders.is_empty() {
        return Err(format!(
            "the device link {} points at {target} but no loop device holds the backing file; check it by hand",
            link.display()
        ));
    }
    Err(format!(
        "the device link {} points at {target} but the backing file is held by {}; check it by hand",
        link.display(),
        holders.iter().map(|h| h.device.as_str()).collect::<Vec<_>>().join(", ")
    ))
}

pub(crate) fn uid_of(object: &ReplicatedVolume) -> Result<String, AgentError> {
    object
        .metadata
        .uid
        .clone()
        .ok_or_else(|| AgentError::Api(ApiError::Other("the object has no metadata.uid".to_string())))
}

pub(crate) fn has_entry(object: &ReplicatedVolume, node: &str) -> bool {
    object
        .status
        .as_ref()
        .is_some_and(|status| status.nodes.iter().any(|n| n.name == node))
}

impl Agent {
    /// Marks `volume` as actively staging until the returned `StageGuard` is dropped.
    pub fn begin_stage(&self, volume: &str) -> StageGuard {
        let mut map = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        *map.entry(volume.to_string()).or_insert(0) += 1;
        StageGuard {
            staging: self.staging.clone(),
            volume: volume.to_string(),
        }
    }

    /// Returns `true` if at least one `NodeStageVolume` call is active for `volume`.
    pub fn is_staging(&self, volume: &str) -> bool {
        let map = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        map.get(volume).copied().unwrap_or(0) > 0
    }

    /// Runs blocking node-level work off the async threads.
    pub(crate) async fn blocking<T, F>(&self, work: F) -> Result<T, AgentError>
    where
        T: Send + 'static,
        F: FnOnce(&Paths, &dyn Runner) -> T + Send + 'static,
    {
        let paths = self.paths.clone();
        let runner = self.runner.clone();
        tokio::task::spawn_blocking(move || work(&paths, &*runner))
            .await
            .map_err(|e| AgentError::Join(e.to_string()))
    }

    fn reconcile_error(volume: &str, source: ReconcileError) -> AgentError {
        AgentError::Reconcile {
            volume: volume.to_string(),
            source,
        }
    }

    async fn read_local(&self, volume: &str) -> Result<Local, AgentError> {
        let name = volume.to_string();
        let vg = self.options.volume_group.clone();
        self.blocking(move |paths, _| local(paths, &name, vg.as_deref()))
            .await?
            .map_err(|e| Self::reconcile_error(volume, e))
    }

    pub(crate) async fn read_status(&self, volume: &str) -> Result<Option<ResourceStatus>, ReconcileError> {
        let name = volume.to_string();
        match self
            .blocking(move |_, runner| Drbd { runner }.status(&name).map_err(ReconcileError::from))
            .await
        {
            Ok(result) => result,
            Err(e) => Err(ReconcileError::Io {
                action: "read the DRBD status",
                path: volume.to_string(),
                source: std::io::Error::other(e.to_string()),
            }),
        }
    }

    pub(crate) async fn read_all_statuses(&self) -> Result<Vec<ResourceStatus>, ReconcileError> {
        match self
            .blocking(move |_, runner| Drbd { runner }.all_statuses().map_err(ReconcileError::from))
            .await
        {
            Ok(result) => result,
            Err(e) => Err(ReconcileError::Io {
                action: "read all DRBD statuses",
                path: "/proc/drbd".to_string(),
                source: std::io::Error::other(e.to_string()),
            }),
        }
    }

    /// Writes this node's entry unless the object already shows exactly it.
    /// `true` when the call was made. The removal of the entry (`None`) is
    /// always sent: a node that finds no entry may only be looking at an
    /// object that is older than its own write. Nothing is written for an
    /// object that does not exist.
    async fn report(
        &self,
        volume: &str,
        object: Option<&ReplicatedVolume>,
        entry: Option<NodeStatus>,
    ) -> Result<bool, AgentError> {
        let Some(object) = object else {
            return Ok(false);
        };
        if let Some(entry) = &entry {
            let current = object
                .status
                .as_ref()
                .and_then(|status| status.nodes.iter().find(|n| n.name == self.node));
            if current == Some(entry) {
                return Ok(false);
            }
        }
        self.volumes
            .apply_node_status(volume, &self.node, entry.as_ref())
            .await?;
        Ok(true)
    }

    /// Removes this node's entry from `status.nodes` and checks that it is
    /// gone. The empty server-side apply removes only the fields this field
    /// manager owns: when another manager co-owns the entry (an operator's
    /// `kubectl patch` of the status) it stays. The object is then read again
    /// and, when the entry is still there, removed with a JSON patch on the
    /// status subresource (see `kube`). `Ok(true)`: the entry is gone (or the
    /// object is). `Ok(false)`: it could not be removed; the reason is logged
    /// and the caller keeps the pass at `Outcome::Kept`, because the other
    /// nodes wait for the entry and a retry in a second would not change it.
    pub(crate) async fn clear_entry(&self, volume: &str, object: &ReplicatedVolume) -> Result<bool, AgentError> {
        self.report(volume, Some(object), None).await?;
        let Some(fresh) = self.volumes.get(volume).await? else {
            return Ok(true);
        };
        if !has_entry(&fresh, &self.node) {
            return Ok(true);
        }
        let uid = uid_of(object)?;
        match self.volumes.remove_node_entry(volume, &uid, &self.node).await {
            Ok(removed) => {
                if removed {
                    log_info(
                        volume,
                        "the empty apply left the entry of this node in status.nodes (another field manager \
                         co-owns it); removed it with a JSON patch",
                    );
                }
                Ok(true)
            }
            Err(error) => {
                log_error(
                    volume,
                    format!("the entry of this node in status.nodes could not be removed: {error}"),
                );
                Ok(false)
            }
        }
    }

    /// Entry with `message`, built from the status read now (best effort).
    async fn report_message(
        &self,
        volume: &str,
        object: Option<&ReplicatedVolume>,
        message: &str,
    ) -> Result<bool, AgentError> {
        let status = self.read_status(volume).await.ok().flatten();
        let entry = node_status(&self.node, status.as_ref(), message);
        self.report(volume, object, Some(entry)).await
    }

    /// Removes this node's finalizer from the object whose uid the pass saw.
    /// `true` when it was there.
    async fn drop_finalizer(&self, volume: &str, object: &ReplicatedVolume) -> Result<bool, AgentError> {
        let uid = uid_of(object)?;
        Ok(self
            .volumes
            .remove_finalizer(volume, &uid, &replica_finalizer(&self.node))
            .await?)
    }

    fn invalid_volume_outcome(volume: &str) -> Outcome {
        // The name would become part of file paths: nothing is read or touched.
        let refusal = Refusal::InvalidSpec(format!("{volume:?} is not a valid volume name"));
        log_error(volume, &refusal);
        Outcome::Refused(refusal)
    }

    /// One pass for one volume name: read the object and the local state,
    /// decide, act, report status.
    pub async fn reconcile_one(&self, volume: &str) -> Result<Outcome, AgentError> {
        if !valid_volume_name(volume) {
            return Ok(Self::invalid_volume_outcome(volume));
        }
        let _guard = self.locks.lock(volume).await;
        self.reconcile_one_locked(volume, None).await
    }

    /// Non-blocking variant for `serve`: returns `Ok(None)` when the volume lock
    /// is currently held elsewhere (e.g. a long CSI operation).
    async fn try_reconcile_one(
        &self,
        volume: &str,
        peers_snapshot: Option<&[(String, u32, u16)]>,
    ) -> Result<Option<Outcome>, AgentError> {
        if !valid_volume_name(volume) {
            return Ok(Some(Self::invalid_volume_outcome(volume)));
        }
        let Some(_guard) = self.locks.try_lock(volume) else {
            return Ok(None);
        };
        self.reconcile_one_locked(volume, peers_snapshot).await.map(Some)
    }

    async fn reconcile_one_locked(
        &self,
        volume: &str,
        peers_snapshot: Option<&[(String, u32, u16)]>,
    ) -> Result<Outcome, AgentError> {
        let object = self.volumes.get_cached(volume).await?;
        let local = self.read_local(volume).await?;
        let task = match &object {
            None => decide(&self.node, volume, None, local),
            Some(object) => {
                let fetched;
                let all: &[(String, u32, u16)] = match peers_snapshot {
                    Some(peers) => peers,
                    None => {
                        fetched = self
                            .volumes
                            .list()
                            .await?
                            .into_iter()
                            .filter_map(|v| Some((v.metadata.name?, v.spec.minor, v.spec.port)))
                            .collect::<Vec<_>>();
                        &fetched
                    }
                };
                let collides_with = collisions(volume, object.spec.minor, object.spec.port, all);
                let finalizers = object.metadata.finalizers.clone().unwrap_or_default();
                let view = ObjectView {
                    spec: &object.spec,
                    deleting: object.metadata.deletion_timestamp.is_some(),
                    finalizers: &finalizers,
                    collides_with: &collides_with,
                };
                decide(&self.node, volume, Some(&view), local)
            }
        };
        match task {
            Task::Ignore => self.ignore(volume, object.as_ref()).await,
            Task::Apply { spec, mark_done } => self.apply(volume, object.as_ref(), spec, mark_done).await,
            Task::DeleteReplica => match object.as_ref() {
                Some(object) => self.delete_replica(volume, object).await,
                // `decide` returns DeleteReplica only for an object that exists.
                None => Ok(Outcome::Settled),
            },
            Task::DropFinalizer => match object.as_ref() {
                Some(object) => {
                    if !self.clear_entry(volume, object).await? {
                        // The other nodes wait for the entry; the finalizer
                        // stays with it and the pass is repeated.
                        return Ok(Outcome::Kept);
                    }
                    if self.drop_finalizer(volume, object).await? {
                        log_info(volume, "nothing is left on this node: finalizer dropped");
                    }
                    Ok(Outcome::Progress)
                }
                None => Ok(Outcome::Settled),
            },
            Task::CleanDiskless => self.clean_diskless(volume, object.as_ref()).await,
            Task::RetireReplica => match object.as_ref() {
                Some(object) => self.retire_replica(volume, object).await,
                None => Ok(Outcome::Settled),
            },
            Task::FinishRetire => match object.as_ref() {
                Some(object) => self.finish_retire(volume, object).await,
                None => Ok(Outcome::Settled),
            },
            Task::Refuse(refusal) => {
                log_error(volume, format!("refused: {refusal}"));
                self.report_message(volume, object.as_ref(), &refusal.to_string())
                    .await?;
                Ok(Outcome::Refused(refusal))
            }
        }
    }

    /// Nothing on this node concerns the volume. An entry of this node in the
    /// `status.nodes` of a deleting object is all that can be left of it (the
    /// removal that followed a cleanup failed on the API): it is removed,
    /// because the other nodes wait for it to go.
    async fn ignore(&self, volume: &str, object: Option<&ReplicatedVolume>) -> Result<Outcome, AgentError> {
        let Some(object) = object else {
            return Ok(Outcome::Gone);
        };
        if object.metadata.deletion_timestamp.is_some() && has_entry(object, &self.node) {
            return Ok(if self.clear_entry(volume, object).await? {
                Outcome::Progress
            } else {
                Outcome::Kept
            });
        }
        Ok(Outcome::Settled)
    }

    async fn apply(
        &self,
        volume: &str,
        object: Option<&ReplicatedVolume>,
        spec: crate::model::VolumeSpec,
        mark_done: bool,
    ) -> Result<Outcome, AgentError> {
        let node = self.node.clone();
        let vg = self.options.volume_group.clone();
        let applied = self
            .blocking(move |paths, runner| {
                Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                }
                .apply_with_status(&spec, &node)
            })
            .await?;
        let (actions, status) = match applied {
            Ok(res) => res,
            // Another ambar process (the CLI) holds the volume: try again soon.
            Err(ReconcileError::Busy { .. }) => return Ok(Outcome::Progress),
            Err(error) => {
                if self
                    .report_message(volume, object, &error.to_string())
                    .await
                    .unwrap_or(false)
                {
                    log_error(volume, &error);
                }
                return Err(Self::reconcile_error(volume, error));
            }
        };
        let entry = node_status(&self.node, status.as_ref(), "");
        let mut changed = self.report(volume, object, Some(entry)).await?;
        if mark_done && let Some(object) = object {
            // The leader's copy was seen in sync (the synced marker): later
            // passes must not skip the initial sync again.
            let uid = uid_of(object)?;
            if self.volumes.mark_initial_sync_done(volume, &uid).await? {
                log_info(volume, "initialSync is Done");
                changed = true;
            }
        }
        if !actions.is_empty() {
            log_info(volume, format!("applied {actions:?}"));
        }
        Ok(if changed || !actions.is_empty() {
            Outcome::Progress
        } else {
            Outcome::Settled
        })
    }

    /// Keeps the copy: the reason goes into this node's status entry and the
    /// log, the next pass is in `KEPT_DELAY`, or in `KEPT_DRBD_DELAY` when
    /// `drbd_state` says the reason is a DRBD state (`Outcome::KeptDrbd`).
    async fn keep(
        &self,
        volume: &str,
        object: &ReplicatedVolume,
        status: Option<&ResourceStatus>,
        why: &str,
        drbd_state: bool,
    ) -> Result<Outcome, AgentError> {
        let message = format!("the local copy is kept: {why}");
        let entry = node_status(&self.node, status, &message);
        if self.report(volume, Some(object), Some(entry)).await? {
            log_error(volume, &message);
        }
        Ok(if drbd_state { Outcome::KeptDrbd } else { Outcome::Kept })
    }

    async fn delete_replica(&self, volume: &str, object: &ReplicatedVolume) -> Result<Outcome, AgentError> {
        let file = {
            let name = volume.to_string();
            let vg = self.options.volume_group.clone();
            self.blocking(move |paths, _| file_state(paths, &name, vg.as_deref()))
                .await?
        };
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                return self
                    .keep(
                        volume,
                        object,
                        None,
                        &format!("cannot inspect the backing file: {error}"),
                        false,
                    )
                    .await;
            }
        };
        // Rule 1 on the object this pass read.
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                return self
                    .keep(
                        volume,
                        object,
                        None,
                        &format!("cannot read the DRBD status: {error}"),
                        false,
                    )
                    .await;
            }
        };
        let check = if status.is_some() {
            // DRBD holds the disk, whatever the file state says.
            Check::Peers(object.status.clone().unwrap_or_default().nodes)
        } else {
            match file {
                FileState::Ready => return self.bring_up(volume, object).await,
                FileState::Absent | FileState::Pending => Check::NoData(file),
            }
        };
        if let Check::Peers(reported) = &check {
            let reasons = keep_reasons(&self.node, &object.spec, reported, status.as_ref());
            if !reasons.is_empty() {
                return self
                    .keep(volume, object, status.as_ref(), &reasons.join("; "), true)
                    .await;
            }
        }
        // The same checks again under the volume lock, on facts read there.
        let name = volume.to_string();
        let node = self.node.clone();
        let spec = object.spec.clone();
        let minor = object.spec.minor;
        let vg = self.options.volume_group.clone();
        let removed = self
            .blocking(move |paths, runner| {
                // Set when the guard refuses for a reason in the DRBD state.
                let drbd_state = Cell::new(false);
                let vg_guard = vg.clone();
                let guard = || {
                    let status = Drbd { runner }
                        .status(&name)
                        .map_err(|e| format!("cannot read the DRBD status: {e}"))?;
                    match &check {
                        Check::Peers(reported) => {
                            let reasons = keep_reasons(&node, &spec, reported, status.as_ref());
                            if reasons.is_empty() {
                                Ok(())
                            } else {
                                drbd_state.set(true);
                                Err(reasons.join("; "))
                            }
                        }
                        Check::NoData(file) => {
                            if status.is_some() {
                                return Err(
                                    "the resource has been configured in the kernel since the decision".to_string()
                                );
                            }
                            prove_no_data(paths, runner, &name, minor, *file, vg_guard.as_deref())
                        }
                    }
                };
                let reconciler = Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                };
                let result = match &check {
                    // The resource must still be absent when the files go.
                    Check::NoData(_) => reconciler.remove_unconfigured(&name, &guard),
                    Check::Peers(_) => reconciler.remove_checked(&name, &guard),
                };
                (result, drbd_state.get())
            })
            .await?;
        let (removed, refused_by_drbd_state) = removed;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("removed the local copy {steps:?}"));
                if !self.clear_entry(volume, object).await? {
                    // The finalizer stays: the next pass finds nothing local
                    // and tries the entry again (`Task::DropFinalizer`).
                    return Ok(Outcome::Kept);
                }
                self.drop_finalizer(volume, object).await?;
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(ReconcileError::Refused { reason, .. }) => {
                let message = format!("the local copy is kept: {reason}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(if refused_by_drbd_state {
                    Outcome::KeptDrbd
                } else {
                    Outcome::Kept
                })
            }
            Err(error) => {
                // `drbdadm down` is refused while the volume is mounted, for
                // one. The finalizer and the files stay.
                let message = format!("the local copy is kept: {error}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(Outcome::Kept)
            }
        }
    }

    async fn retire_replica(&self, volume: &str, object: &ReplicatedVolume) -> Result<Outcome, AgentError> {
        let file = {
            let name = volume.to_string();
            let vg = self.options.volume_group.clone();
            self.blocking(move |paths, _| file_state(paths, &name, vg.as_deref()))
                .await?
        };
        let file = match file {
            Ok(file) => file,
            Err(error) => {
                return self
                    .keep(
                        volume,
                        object,
                        None,
                        &format!("cannot inspect the backing file: {error}"),
                        false,
                    )
                    .await;
            }
        };
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                return self
                    .keep(
                        volume,
                        object,
                        None,
                        &format!("cannot read the DRBD status: {error}"),
                        false,
                    )
                    .await;
            }
        };
        let check = if let Some(ref st) = status {
            // R26: If the resource differs from the description, not applied, or
            // status does not match spec, apply the description first.
            let mut spec = match to_volume_spec(volume, &object.spec) {
                Ok(spec) => spec,
                Err(error) => {
                    let message = format!("the local copy is kept: {error}");
                    if self.report_message(volume, Some(object), &message).await? {
                        log_error(volume, &message);
                    }
                    return Ok(Outcome::Kept);
                }
            };
            spec.initial_sync = InitialSync::Done;
            let node = self.node.clone();
            let spec_obs = spec.clone();
            let vg = self.options.volume_group.clone();
            let obs = self
                .blocking(move |paths, runner| {
                    Reconciler {
                        paths,
                        runner,
                        volume_group: vg,
                    }
                    .observe(&spec_obs, &node)
                })
                .await?;
            let obs = match obs {
                Ok(obs) => obs,
                Err(error) => {
                    let message = format!("the local copy is kept: {error}");
                    if self.report_message(volume, Some(object), &message).await? {
                        log_error(volume, &message);
                    }
                    return Ok(Outcome::Kept);
                }
            };
            let matches = obs.res_matches && obs.res_applied && status_matches_spec(&spec, &self.node, st);
            if !matches {
                let node = self.node.clone();
                let vg = self.options.volume_group.clone();
                let applied = self
                    .blocking(move |paths, runner| {
                        Reconciler {
                            paths,
                            runner,
                            volume_group: vg,
                        }
                        .apply(&spec, &node)
                    })
                    .await?;
                match applied {
                    Ok(actions) => {
                        log_info(volume, format!("retiring copy: brought resource to spec {actions:?}"));
                        return Ok(Outcome::Progress);
                    }
                    Err(ReconcileError::Busy { .. }) => return Ok(Outcome::Progress),
                    Err(error) => {
                        let message = format!("the local copy is kept: {error}");
                        if self.report_message(volume, Some(object), &message).await? {
                            log_error(volume, &message);
                        }
                        return Ok(Outcome::Kept);
                    }
                }
            }
            let reasons = retire_reasons(&object.spec, &self.node, st);
            if !reasons.is_empty() {
                return self.keep(volume, object, Some(st), &reasons.join("; "), true).await;
            }
            CheckRetire::Peers
        } else {
            match file {
                FileState::Ready => return self.bring_up(volume, object).await,
                FileState::Absent | FileState::Pending => CheckRetire::NoData(file),
            }
        };

        let name = volume.to_string();
        let node = self.node.clone();
        let spec_crd = object.spec.clone();
        let minor = object.spec.minor;
        let vg = self.options.volume_group.clone();
        let removed = self
            .blocking(move |paths, runner| {
                let drbd_state = Cell::new(false);
                let vg_guard = vg.clone();
                let guard = || {
                    let status = Drbd { runner }
                        .status(&name)
                        .map_err(|e| format!("cannot read the DRBD status: {e}"))?;
                    match &check {
                        CheckRetire::Peers => {
                            let Some(ref status) = status else {
                                return Err("the resource has disappeared since the decision".to_string());
                            };
                            let reasons = retire_reasons(&spec_crd, &node, status);
                            if reasons.is_empty() {
                                Ok(())
                            } else {
                                drbd_state.set(true);
                                Err(reasons.join("; "))
                            }
                        }
                        CheckRetire::NoData(file) => {
                            if status.is_some() {
                                return Err(
                                    "the resource has been configured in the kernel since the decision".to_string()
                                );
                            }
                            prove_no_data(paths, runner, &name, minor, *file, vg_guard.as_deref())
                        }
                    }
                };
                let reconciler = Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                };
                let result = match &check {
                    CheckRetire::NoData(_) => reconciler.remove_unconfigured(&name, &guard),
                    CheckRetire::Peers => reconciler.remove_checked(&name, &guard),
                };
                (result, drbd_state.get())
            })
            .await?;
        let (removed, refused_by_drbd_state) = removed;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("retired the local copy {steps:?}"));
                self.finish_retire(volume, object).await
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(ReconcileError::Refused { reason, .. }) => {
                let message = format!("the local copy is kept: {reason}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(if refused_by_drbd_state {
                    Outcome::KeptDrbd
                } else {
                    Outcome::Kept
                })
            }
            Err(error) => {
                let message = format!("the local copy is kept: {error}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(Outcome::Kept)
            }
        }
    }

    async fn finish_retire(&self, volume: &str, object: &ReplicatedVolume) -> Result<Outcome, AgentError> {
        if has_entry(object, &self.node) && !self.clear_entry(volume, object).await? {
            return Ok(Outcome::Kept);
        }
        if object
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&replica_finalizer(&self.node)))
        {
            self.drop_finalizer(volume, object).await?;
        }
        for attempt in 0..EDIT_ATTEMPTS {
            let Some(current) = self.volumes.get(volume).await? else {
                return Ok(Outcome::Progress);
            };
            let uid = uid_of(&current)?;
            let remaining: Vec<MemberNode> = current
                .spec
                .nodes
                .iter()
                .filter(|n| n.name != self.node)
                .cloned()
                .collect();
            if remaining.len() == current.spec.nodes.len() {
                return Ok(Outcome::Progress);
            }
            if self
                .volumes
                .replace_members(volume, &uid, &current.spec.nodes, &remaining)
                .await?
            {
                return Ok(Outcome::Progress);
            }
            if attempt + 1 < EDIT_ATTEMPTS {
                tokio::time::sleep(conflict_backoff(attempt)).await;
            }
        }
        log_info(
            volume,
            "replace_members conflict while removing Retiring member, retrying",
        );
        Ok(Outcome::Progress)
    }

    /// A copy with data whose resource is not configured here (after a reboot
    /// or `drbdadm down`): the checks of the module doc cannot see the peers
    /// and the peers cannot see this node, so each waits for the other. The
    /// resource is brought up with the same `Reconciler::apply` the `Apply`
    /// task uses, and the decision is made on a later pass. Nothing is deleted
    /// here. The description is converted without `initialSync: Skip`: a
    /// deleting object must not run `new-current-uuid`, and the ready backing
    /// file means `create-md` is not planned either.
    async fn bring_up(&self, volume: &str, object: &ReplicatedVolume) -> Result<Outcome, AgentError> {
        let kept =
            |why: String| format!("the local copy is kept: cannot bring the resource up before deleting it: {why}");
        let mut spec = match to_volume_spec(volume, &object.spec) {
            Ok(spec) => spec,
            Err(error) => {
                let message = kept(error.to_string());
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                return Ok(Outcome::Kept);
            }
        };
        spec.initial_sync = InitialSync::Done;
        let node = self.node.clone();
        let vg = self.options.volume_group.clone();
        let applied = self
            .blocking(move |paths, runner| {
                Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                }
                .apply(&spec, &node)
            })
            .await?;
        match applied {
            Ok(actions) => log_info(
                volume,
                format!("the resource was not configured; applied {actions:?} before deciding on the deletion"),
            ),
            Err(ReconcileError::Busy { .. }) => return Ok(Outcome::Progress),
            Err(error) => {
                let message = kept(error.to_string());
                if self
                    .report_message(volume, Some(object), &message)
                    .await
                    .unwrap_or(false)
                {
                    log_error(volume, &message);
                }
                return Ok(Outcome::Kept);
            }
        }
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                self.report_message(volume, Some(object), &kept(error.to_string()))
                    .await
                    .ok();
                return Err(Self::reconcile_error(volume, error));
            }
        };
        let entry = node_status(
            &self.node,
            status.as_ref(),
            "the resource is up; the copy is deleted when no other node can use it",
        );
        self.report(volume, Some(object), Some(entry)).await?;
        Ok(Outcome::Progress)
    }

    async fn clean_diskless(&self, volume: &str, object: Option<&ReplicatedVolume>) -> Result<Outcome, AgentError> {
        // `remove` deletes a backing file if one is there, so the decision is
        // not enough: the guard looks again under the volume lock.
        let name = volume.to_string();
        let vg = self.options.volume_group.clone();
        let removed = self
            .blocking(move |paths, runner| {
                let vg_guard = vg.clone();
                let guard = || match local(paths, &name, vg_guard.as_deref()) {
                    Ok(state) if state.image => Err("a backing file has appeared since the decision".to_string()),
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("cannot inspect the backing file: {e}")),
                };
                Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                }
                .remove_checked(&name, &guard)
            })
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("cleaned leftovers {steps:?}"));
                if let Some(object) = object
                    && !self.clear_entry(volume, object).await?
                {
                    return Ok(Outcome::Kept);
                }
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(ReconcileError::Refused { reason, .. }) => {
                let refusal = if object.is_some() {
                    Refusal::ImageWithoutMembership
                } else {
                    Refusal::OrphanImage
                };
                log_error(volume, format!("nothing was cleaned: {reason}: {refusal}"));
                Ok(Outcome::Refused(refusal))
            }
            Err(error) => {
                let message = format!("cleaning leftovers failed: {error}");
                if self.report_message(volume, object, &message).await.unwrap_or(false) {
                    log_error(volume, &message);
                }
                Err(Self::reconcile_error(volume, error))
            }
        }
    }

    pub(crate) async fn cleanup_stale_client(
        &self,
        volume: &str,
        object: &ReplicatedVolume,
    ) -> Result<Outcome, AgentError> {
        let name = volume.to_string();
        let vg = self.options.volume_group.clone();
        let removed = self
            .blocking(move |paths, runner| {
                let vg_guard = vg.clone();
                let guard = || {
                    match (Drbd { runner }).status(&name) {
                        Ok(Some(status)) if status.role == Role::Primary || status.devices.iter().any(|d| d.open) => {
                            return Err("the resource is in use (Primary or open)".to_string());
                        }
                        Ok(_) => {}
                        Err(e) => return Err(format!("cannot read drbd status: {e}")),
                    }
                    match local(paths, &name, vg_guard.as_deref()) {
                        Ok(state) if state.image => Err("a backing file exists on this node".to_string()),
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("cannot inspect the backing file: {e}")),
                    }
                };
                Reconciler {
                    paths,
                    runner,
                    volume_group: vg,
                }
                .remove_checked(&name, &guard)
            })
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("cleaned stale client {steps:?}"));
                if !self.clear_entry(volume, object).await? {
                    return Ok(Outcome::Kept);
                }
                cleanup::leave_client(&*self.volumes, &self.node, volume).await?;
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(ReconcileError::Refused { reason, .. }) => {
                let message = format!("stale client cleanup refused: {reason}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(Outcome::Kept)
            }
            Err(error) => {
                let message = format!("stale client cleanup failed: {error}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Err(Self::reconcile_error(volume, error))
            }
        }
    }

    /// Names of every object and every local leftover, together with
    /// `(name, minor, port)` tuples from the same volume listing.
    async fn all_names(&self) -> Result<PeerSnapshot, AgentError> {
        let vg = self.options.volume_group.clone();
        let mut names = self
            .blocking(move |paths, _| local_volumes(paths, vg.as_deref()))
            .await?
            .map_err(|e| Self::reconcile_error("*", e))?;
        let mut peers = Vec::new();
        for object in self.volumes.list().await? {
            if let Some(name) = object.metadata.name {
                peers.push((name.clone(), object.spec.minor, object.spec.port));
                names.insert(name);
            }
        }
        Ok((names, peers))
    }

    /// Watches the objects and runs `reconcile_one` for the names that
    /// changed, for the names a pass asked to see again and for all names on
    /// a timer. Returns after `shutdown` is cancelled, once the pass in
    /// progress has finished.
    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), AgentError> {
        let queue = Arc::new(Queue::default());
        let stop_watch = shutdown.child_token();
        let watch = tokio::spawn(feed(self.volumes.watch(), queue.clone(), stop_watch.clone()));
        let stop_failures = shutdown.child_token();
        let stop_failures_task = stop_failures.clone();
        let this = self.clone();
        let failures_task = tokio::spawn(async move {
            this.run_failure_loop(stop_failures_task).await;
        });
        let result = self.serve(&queue, &shutdown).await;
        stop_watch.cancel();
        stop_failures.cancel();
        if let Err(e) = watch.await {
            log_error("*", format!("the watch task failed: {e}"));
        }
        let _ = failures_task.await;
        result
    }

    async fn serve(&self, queue: &Queue, shutdown: &CancellationToken) -> Result<(), AgentError> {
        let mut startup = Startup::default();
        // The first listing is taken at once and, while it fails, again every
        // `ERROR_DELAY`; the queue is served meanwhile (the watch feeds it).
        let mut next_full = Instant::now();
        let mut full_pass_peers: Option<PeerSnapshot> = None;
        while !shutdown.is_cancelled() {
            let now = Instant::now();
            if now >= next_full {
                queue.watch_dirty.store(false, Ordering::SeqCst);
                match self.all_names().await {
                    Ok((names, peers)) => {
                        next_full = now + FULL_PASS_EVERY;
                        startup.begin(&names);
                        names.iter().for_each(|name| queue.schedule(name, now));
                        full_pass_peers = Some((names, peers));
                    }
                    Err(e) => {
                        full_pass_peers = None;
                        log_error("*", format!("cannot list the volumes of this node: {e}"));
                        next_full = now + if startup.listed() { FULL_PASS_EVERY } else { ERROR_DELAY };
                    }
                }
                self.publish_readiness(&startup);
            }
            if queue.watch_dirty.swap(false, Ordering::SeqCst) {
                full_pass_peers = None;
            }
            match queue.pop_due(now) {
                Ok(name) => {
                    let peers_snapshot = full_pass_peers
                        .as_ref()
                        .and_then(|(names, peers)| names.contains(&name).then_some(peers.as_slice()));
                    let result = match self.try_reconcile_one(&name, peers_snapshot).await {
                        Ok(None) => {
                            queue.schedule(&name, Instant::now() + BUSY_VOLUME_RETRY_DELAY);
                            continue;
                        }
                        Ok(Some(outcome)) => Ok(outcome),
                        Err(err) => Err(err),
                    };
                    if let Some(delay) = settle(&name, &result) {
                        queue.schedule(&name, Instant::now() + delay);
                    }
                    if let Some((names, _)) = &mut full_pass_peers {
                        names.remove(&name);
                        if names.is_empty() {
                            full_pass_peers = None;
                        }
                    }
                    startup.record(&name, &result);
                    self.publish_readiness(&startup);
                }
                Err(next) => {
                    let wake = next.map_or(next_full, |at| at.min(next_full));
                    tokio::select! {
                        biased;
                        () = shutdown.cancelled() => {}
                        () = queue.wake.notified() => {}
                        () = sleep_until(wake) => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Single evaluation of node-level failure checks and metrics:
    /// 1. Taints node with no-quorum if any mounted volume lost quorum, or removes taint if quorum intact.
    /// 2. Evicts pods for volumes whose quorum loss grace period expired.
    /// 3. Auto-adjusts Secondary connections in StandAlone (up to max attempts).
    /// 4. Cleans up stale Client entries without pods.
    /// 5. Renders and stores latest Prometheus metrics.
    pub async fn tick_failures(
        &self,
        quorum_tracker: &mut QuorumTracker,
        standalone_tracker: &mut StandaloneTracker,
        taint_tracker: &mut TaintTracker,
        cleanup_tracker: &mut CleanupTracker,
        now: std::time::Instant,
    ) -> Result<(), AgentError> {
        let statuses = match self.read_all_statuses().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ambar-agent: warning: failed to read drbd statuses: {e}");
                return Err(Self::reconcile_error("*", e));
            }
        };

        let mounted_by_volume =
            tokio::task::spawn_blocking(|| mounted_pod_uids_by_volume(Path::new("/var/lib/kubelet/pods")))
                .await
                .unwrap_or_default();

        // 1. Taint management (R32)
        let should_taint = crate::agent::taint::should_taint_node(&statuses);
        if taint_tracker.needs_reconcile(should_taint, now)
            && reconcile_node_taint(&*self.volumes, &self.node, should_taint)
                .await
                .is_ok()
        {
            self.is_tainted.store(should_taint, Ordering::SeqCst);
            taint_tracker.record_synced(should_taint, now);
        }

        // 2. Quorum loss tracking and pod eviction (R33)
        for status in &statuses {
            let is_mounted = status.role == Role::Primary || status.devices.iter().any(|d| d.open);
            let has_quorum = status.quorum();
            let mounted_uids = mounted_by_volume.get(&status.name).map(Vec::as_slice).unwrap_or(&[]);
            let check = VolumeQuorumCheck {
                volume: &status.name,
                is_mounted,
                has_quorum,
                node: &self.node,
                mounted_uids,
                now,
            };
            if let Err(e) = check_volume_quorum(quorum_tracker, &*self.volumes, check).await {
                eprintln!(
                    "ambar-agent: warning: volume {:?}: failed to check quorum / evict pods: {e}",
                    status.name
                );
            }
        }

        // 3. Auto-adjust StandAlone Secondary connections and split-brain tracking (R34, R35)
        for status in &statuses {
            if !crate::model::valid_volume_name(&status.name) {
                continue;
            }
            if let crate::agent::standalone::StandaloneAction::RunAdjust { attempt } =
                standalone_tracker.check(status, now)
            {
                let name = status.name.clone();
                let vg = self.options.volume_group.clone();
                let Some(_lock) = self.locks.try_lock(&name) else {
                    continue;
                };
                let adjusted = self
                    .blocking(move |paths, runner| {
                        let reconciler = Reconciler {
                            paths,
                            runner,
                            volume_group: vg,
                        };
                        let _file_lock = match reconciler.lock_volume(&name) {
                            Ok(l) => l,
                            Err(e) => return Err(format!("cannot acquire volume lock: {e}")),
                        };
                        let drbd = Drbd { runner };
                        let out = drbd.adjust(&name).map_err(|e| format!("cannot adjust {name}: {e}"))?;
                        if !out.success() {
                            return Err(format!("drbdadm adjust {name} failed: {}", out.stderr));
                        }
                        Ok(())
                    })
                    .await;
                match adjusted {
                    Ok(Ok(())) => {
                        log_info(
                            &status.name,
                            format!("auto-adjusted StandAlone connection (attempt {attempt})"),
                        );
                    }
                    Ok(Err(e)) => {
                        eprintln!("ambar-agent: warning: volume {:?}: {e}", status.name);
                    }
                    Err(e) => {
                        eprintln!(
                            "ambar-agent: warning: volume {:?}: blocking task failed: {e}",
                            status.name
                        );
                    }
                }
            }
        }

        // Prune vanished volumes from in-memory trackers after successful status read.
        let active_volumes: HashSet<&str> = statuses.iter().map(|s| s.name.as_str()).collect();
        quorum_tracker.retain_volumes(&active_volumes);
        standalone_tracker.retain_volumes(&active_volumes);

        // 4. Stale Client cleanup without pods (R36), debounced via CleanupTracker
        if self.ready.load(Ordering::Acquire)
            && cleanup_tracker.should_run(now)
            && let Ok(objects) = self.volumes.list().await
        {
            let status_by_name: HashMap<&str, &ResourceStatus> =
                statuses.iter().map(|s| (s.name.as_str(), s)).collect();
            for obj in &objects {
                if let Some(ref name) = obj.metadata.name {
                    let snapshot = status_by_name.get(name.as_str()).copied();
                    let mounted_uids = mounted_by_volume.get(name).map(Vec::as_slice).unwrap_or(&[]);
                    let _ =
                        cleanup::check_and_cleanup_client_with_snapshot(self, name, obj, snapshot, mounted_uids).await;
                }
            }
        }

        // 5. Prometheus metrics update (R38)
        static DRBD_VERSION: OnceLock<Option<String>> = OnceLock::new();
        let drbd_ver: Option<&str> = if let Some(cached) = DRBD_VERSION.get() {
            cached.as_deref()
        } else {
            let loaded = tokio::task::spawn_blocking(|| {
                std::fs::read_to_string(Path::new("/sys/module/drbd/version"))
                    .ok()
                    .map(|s| s.trim().to_string())
            })
            .await
            .ok()
            .flatten();
            DRBD_VERSION.get_or_init(|| loaded).as_deref()
        };
        let mut volume_metrics = Vec::new();
        let mut total_allocated: u64 = 0;
        for status in &statuses {
            let manual = standalone_tracker.is_manual_check_needed(&status.name);
            let size = status.device().filter(|d| !d.client).map_or(0, |d| d.size * 1024);
            total_allocated += size;
            volume_metrics.push(VolumeMetricData {
                name: &status.name,
                status: Some(status),
                manual_check_needed: manual,
            });
        }
        let metric_data = NodeMetricData {
            node: &self.node,
            drbd_version: drbd_ver,
            is_tainted: self.is_tainted.load(Ordering::SeqCst),
            allocated_bytes: total_allocated,
            volumes: volume_metrics,
        };
        self.metrics.update(render_metrics(&metric_data));

        Ok(())
    }

    /// Background loop running failure evaluations periodically.
    pub async fn run_failure_loop(&self, shutdown: CancellationToken) {
        let mut quorum_tracker = QuorumTracker::new(self.options.quorum_loss_grace_period);
        let mut standalone_tracker =
            StandaloneTracker::new(self.options.adjust_attempts as usize, self.options.adjust_interval);
        let mut taint_tracker = TaintTracker::new(DEFAULT_TAINT_RESYNC_INTERVAL);
        let mut cleanup_tracker =
            CleanupTracker::with_initial_delay(DEFAULT_CLEANUP_INTERVAL, std::time::Instant::now());
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let now = std::time::Instant::now();
                    let _ = self
                        .tick_failures(
                            &mut quorum_tracker,
                            &mut standalone_tracker,
                            &mut taint_tracker,
                            &mut cleanup_tracker,
                            now,
                        )
                        .await;
                }
            }
        }
    }

    /// Serves Prometheus HTTP metrics on `addr`.
    pub async fn serve_metrics(&self, addr: &str, shutdown: CancellationToken) -> std::io::Result<()> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.serve_metrics_listener(listener, shutdown).await
    }

    /// Serves Prometheus HTTP metrics on an already-bound `TcpListener`.
    pub async fn serve_metrics_listener(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: CancellationToken,
    ) -> std::io::Result<()> {
        let holder = self.metrics.clone();
        crate::agent::metrics_server::serve_listener(listener, Arc::new(move || holder.get()), shutdown).await
    }

    fn publish_readiness(&self, startup: &Startup) {
        if startup.complete() {
            self.ready.store(true, Ordering::SeqCst);
        }
    }
}

/// The names of the first listing and how often each failed on the API.
/// `ready` waits until each has passed or has failed `FIRST_PASS_ATTEMPTS` times.
#[derive(Default)]
struct Startup {
    /// `None` until the first listing succeeded.
    waiting: Option<BTreeMap<String, u32>>,
}

impl Startup {
    fn listed(&self) -> bool {
        self.waiting.is_some()
    }

    /// Takes the names of the first listing; later listings change nothing.
    fn begin(&mut self, names: &BTreeSet<String>) {
        if self.waiting.is_none() {
            self.waiting = Some(names.iter().map(|name| (name.clone(), 0)).collect());
        }
    }

    fn record(&mut self, name: &str, result: &Result<Outcome, AgentError>) {
        let Some(waiting) = self.waiting.as_mut() else {
            return;
        };
        let Some(failures) = waiting.get_mut(name) else {
            return;
        };
        if matches!(result, Err(AgentError::Api(_))) {
            *failures += 1;
            if *failures < FIRST_PASS_ATTEMPTS {
                return;
            }
            log_error(
                name,
                format!(
                    "the first pass failed {FIRST_PASS_ATTEMPTS} times on the API; readiness no longer waits for it"
                ),
            );
        }
        waiting.remove(name);
    }

    fn complete(&self) -> bool {
        self.waiting.as_ref().is_some_and(BTreeMap::is_empty)
    }
}

/// Logs an error result and says when the name is due again; `None`: not at all.
fn settle(name: &str, result: &Result<Outcome, AgentError>) -> Option<Duration> {
    Some(match result {
        Ok(Outcome::Progress) => PROGRESS_DELAY,
        Ok(Outcome::Settled) => SETTLED_DELAY,
        Ok(Outcome::Gone) => return None,
        Ok(Outcome::Kept) => KEPT_DELAY,
        Ok(Outcome::KeptDrbd) => KEPT_DRBD_DELAY,
        Ok(Outcome::Refused(_)) => REFUSED_DELAY,
        Err(error) => {
            log_error(name, error);
            ERROR_DELAY
        }
    })
}

/// Names with the time of their next pass. A name is queued once; queuing it
/// again keeps the earlier time.
type PeerSnapshot = (BTreeSet<String>, Vec<(String, u32, u16)>);

#[derive(Default)]
struct Queue {
    due: Mutex<BTreeMap<String, Instant>>,
    wake: Notify,
    watch_dirty: AtomicBool,
}

impl Queue {
    fn schedule(&self, name: &str, at: Instant) {
        {
            let mut due = self.due.lock().unwrap_or_else(|e| e.into_inner());
            let slot = due.entry(name.to_string()).or_insert(at);
            *slot = (*slot).min(at);
        }
        self.wake.notify_one();
    }

    /// Takes the name that is due earliest, if it is due at `now`; otherwise
    /// says when the next one is due (`None`: the queue is empty).
    fn pop_due(&self, now: Instant) -> Result<String, Option<Instant>> {
        let mut due = self.due.lock().unwrap_or_else(|e| e.into_inner());
        let Some((name, &at)) = due.iter().min_by_key(|(_, at)| **at) else {
            return Err(None);
        };
        if at <= now {
            let name = name.clone();
            due.remove(&name);
            Ok(name)
        } else {
            Err(Some(at))
        }
    }
}

/// Queues the name of every object the watch reports, until `stop`.
async fn feed(
    mut events: futures_util::stream::BoxStream<'static, Result<String, ApiError>>,
    queue: Arc<Queue>,
    stop: CancellationToken,
) {
    loop {
        let item = tokio::select! {
            () = stop.cancelled() => return,
            item = events.next() => item,
        };
        match item {
            Some(Ok(name)) => {
                queue.watch_dirty.store(true, Ordering::SeqCst);
                queue.schedule(&name, Instant::now());
            }
            Some(Err(e)) => log_error("*", e),
            None => {
                log_error("*", "the watch ended; changes are picked up by the periodic pass only");
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "tests/run.rs"]
mod tests;
