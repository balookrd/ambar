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
//! | `delete_replica` | `DeleteReplica` | the DRBD status can be read; rules 1-3 below on the object read at the start of the pass | the DRBD status is read again; rules 1-3a below; 3b uses the same object |
//! | `clean_diskless` | `CleanDiskless` | nothing | no backing file exists |
//!
//! `ambar remove` (the CLI) calls `Reconciler::remove`, which has no guard:
//! it is an operator's decision and bypasses these checks by design.
//!
//! `Task::DeleteReplica` (extends R18, R10). The copy is deleted only if all
//! of these hold; otherwise it is kept, the reason goes into the `message` of
//! this node's `status.nodes` entry and the pass is repeated after 30 s (or
//! earlier, when the object changes):
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
//!    A node that is not `Connected` (or the resource is not configured here at
//!    all) and still has an entry blocks the deletion, whatever role the entry
//!    shows. A connection whose `peer_role` is `Primary` blocks in every
//!    connection state and whatever the entries say: that is the old R18 check
//!    and 3b does not weaken it.
//!
//! A node that is dead for good keeps its entry and so blocks the deletion of
//! the copies of the others. The way out is manual and deliberate: the
//! operator checks that nobody uses the volume, removes the dead node's entry
//! from `status.nodes` (`kubectl patch rv <name> --subresource=status
//! --type=json -p '[{"op":"remove","path":"/status/nodes/<index>"}]'`) and its
//! finalizer from `metadata.finalizers`; the remaining nodes then delete.
//!
//! Known limit (stand, Task 8 fix round 1): a node that holds a copy but whose
//! resource is not configured (after `drbdadm down` or a reboot) while the
//! object is deleting waits for the entries of all the others (3b), and the
//! others, seeing it not connected with its entry still there, wait for it.
//! Nobody deletes until an operator breaks the cycle as above (or
//! `ambar remove` on that node). The rule keeps the copies in this case, which
//! is the safe side.
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
//! when it settled, 60 s when it refused, 5 s on an error. Every 30 s the
//! names of all objects and all local leftovers are queued, which finds
//! leftovers of objects that are gone. `ready` becomes true once every name
//! of the first listing has had one pass without an API error, or has failed
//! on the API 3 times; the loop serves events meanwhile, so a name that keeps
//! failing delays nothing else.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::crd::{NodeStatus, ReplicatedVolume, ReplicatedVolumeSpec};
use crate::drbd::adm::Drbd;
use crate::drbd::status::{ConnectionState, ResourceStatus, Role};
use crate::exec::Runner;
use crate::membership::{collisions, replica_finalizer};
use crate::model::valid_volume_name;
use crate::paths::Paths;
use crate::reconcile::{ReconcileError, Reconciler};

use super::decide::{Local, ObjectView, Refusal, Task, decide};
use super::kube::{ApiError, Volumes};
use super::local::{local, local_volumes};
use super::status::node_status;

/// Next pass after one that ran a node-level action or changed the status.
const PROGRESS_DELAY: Duration = Duration::from_secs(1);
/// Next pass after one that found nothing to do.
const SETTLED_DELAY: Duration = Duration::from_secs(30);
/// Next pass after one that kept the copy of a deleting volume: what blocks
/// the deletion (a Primary, a peer that has not cleaned up) does not go away
/// in a second, and a change of the object queues the name at once.
const KEPT_DELAY: Duration = Duration::from_secs(30);
/// Next pass after a refusal.
const REFUSED_DELAY: Duration = Duration::from_secs(60);
/// Next pass after an error.
const ERROR_DELAY: Duration = Duration::from_secs(5);
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
    /// Something was done or reported, or has to be tried again soon.
    Progress,
    /// The local copy of a deleting volume was kept because deleting it is
    /// not safe yet (the reason is in the status `message`).
    Kept,
    Refused(Refusal),
}

/// Serialises work on one volume on this node: the reconcile loop and the
/// CSI Node calls take it per volume name. Cloning shares the locks.
#[derive(Clone, Default)]
pub struct VolumeLocks {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

impl VolumeLocks {
    pub async fn lock(&self, volume: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            // A lock that only the map holds has no holder and no waiter.
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            locks.entry(volume.to_string()).or_default().clone()
        };
        lock.lock_owned().await
    }
}

pub struct Agent {
    pub node: String,
    pub paths: Paths,
    pub runner: Arc<dyn Runner>,
    pub volumes: Arc<dyn Volumes>,
    /// Serialises work on one node: the reconcile loop and CSI Node calls take it per volume.
    pub locks: VolumeLocks,
    /// Set once, after the first listing of names has been worked through (see
    /// the module doc). It is never reset: when the API server is lost later
    /// the flag stays true. What CSI Node does then is decided in Task 12.
    pub ready: Arc<AtomicBool>,
}

fn log_info(volume: &str, text: impl Display) {
    eprintln!("ambar-agent: volume {volume:?}: {text}");
}

fn log_error(volume: &str, text: impl Display) {
    eprintln!("ambar-agent: error: volume {volume:?}: {text}");
}

/// The node name must be the host name: `drbdadm` picks the `on <host>`
/// section of the resource file by it. The volumes directory must exist: the
/// agent never creates it.
pub fn preflight(node: &str, hostname: &str, paths: &Paths) -> Result<(), AgentError> {
    if node != hostname.trim() {
        return Err(AgentError::Startup(format!(
            "NODE_NAME is {node:?} but /proc/sys/kernel/hostname is {:?}; the node name must be the host name",
            hostname.trim()
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

fn uid_of(object: &ReplicatedVolume) -> Result<String, AgentError> {
    object
        .metadata
        .uid
        .clone()
        .ok_or_else(|| AgentError::Api(ApiError::Other("the object has no metadata.uid".to_string())))
}

fn has_entry(object: &ReplicatedVolume, node: &str) -> bool {
    object
        .status
        .as_ref()
        .is_some_and(|status| status.nodes.iter().any(|n| n.name == node))
}

impl Agent {
    /// Runs blocking node-level work off the async threads.
    async fn blocking<T, F>(&self, work: F) -> Result<T, AgentError>
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
        self.blocking(move |paths, _| local(paths, &name))
            .await?
            .map_err(|e| Self::reconcile_error(volume, e))
    }

    async fn read_status(&self, volume: &str) -> Result<Option<ResourceStatus>, ReconcileError> {
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

    /// One pass for one volume name: read the object and the local state,
    /// decide, act, report status.
    pub async fn reconcile_one(&self, volume: &str) -> Result<Outcome, AgentError> {
        if !valid_volume_name(volume) {
            // The name would become part of file paths: nothing is read or touched.
            let refusal = Refusal::InvalidSpec(format!("{volume:?} is not a valid volume name"));
            log_error(volume, &refusal);
            return Ok(Outcome::Refused(refusal));
        }
        let _guard = self.locks.lock(volume).await;
        let object = self.volumes.get(volume).await?;
        let local = self.read_local(volume).await?;
        let task = match &object {
            None => decide(&self.node, volume, None, local),
            Some(object) => {
                let all: Vec<(String, u32, u16)> = self
                    .volumes
                    .list()
                    .await?
                    .into_iter()
                    .filter_map(|v| Some((v.metadata.name?, v.spec.minor, v.spec.port)))
                    .collect();
                let collides_with = collisions(volume, object.spec.minor, object.spec.port, &all);
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
                    self.report(volume, Some(object), None).await?;
                    if self.drop_finalizer(volume, object).await? {
                        log_info(volume, "nothing is left on this node: finalizer dropped");
                    }
                    Ok(Outcome::Progress)
                }
                None => Ok(Outcome::Settled),
            },
            Task::CleanDiskless => self.clean_diskless(volume, object.as_ref()).await,
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
            return Ok(Outcome::Settled);
        };
        if object.metadata.deletion_timestamp.is_some() && has_entry(object, &self.node) {
            self.report(volume, Some(object), None).await?;
            return Ok(Outcome::Progress);
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
        let applied = self
            .blocking(move |paths, runner| Reconciler { paths, runner }.apply(&spec, &node))
            .await?;
        let actions = match applied {
            Ok(actions) => actions,
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
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                self.report_message(volume, object, &error.to_string()).await.ok();
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

    async fn delete_replica(&self, volume: &str, object: &ReplicatedVolume) -> Result<Outcome, AgentError> {
        // Rules 1-3 of the module doc on the object this pass read.
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                let message = format!("the local copy is kept: cannot read the DRBD status: {error}");
                let entry = node_status(&self.node, None, &message);
                if self.report(volume, Some(object), Some(entry)).await.unwrap_or(false) {
                    log_error(volume, &message);
                }
                return Err(Self::reconcile_error(volume, error));
            }
        };
        let reported = object.status.clone().unwrap_or_default().nodes;
        let reasons = keep_reasons(&self.node, &object.spec, &reported, status.as_ref());
        if !reasons.is_empty() {
            let message = format!("the local copy is kept: {}", reasons.join("; "));
            let entry = node_status(&self.node, status.as_ref(), &message);
            if self.report(volume, Some(object), Some(entry)).await? {
                log_error(volume, &message);
            }
            return Ok(Outcome::Kept);
        }
        // The same rules again under the volume lock, on a status read there.
        let name = volume.to_string();
        let node = self.node.clone();
        let spec = object.spec.clone();
        let removed = self
            .blocking(move |paths, runner| {
                let guard = || {
                    let status = Drbd { runner }
                        .status(&name)
                        .map_err(|e| format!("cannot read the DRBD status: {e}"))?;
                    let reasons = keep_reasons(&node, &spec, &reported, status.as_ref());
                    if reasons.is_empty() {
                        Ok(())
                    } else {
                        Err(reasons.join("; "))
                    }
                };
                Reconciler { paths, runner }.remove_checked(&name, &guard)
            })
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("removed the local copy {steps:?}"));
                self.report(volume, Some(object), None).await?;
                self.drop_finalizer(volume, object).await?;
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(ReconcileError::Refused { reason, .. }) => {
                let message = format!("the local copy is kept: {reason}");
                if self.report_message(volume, Some(object), &message).await? {
                    log_error(volume, &message);
                }
                Ok(Outcome::Kept)
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

    async fn clean_diskless(&self, volume: &str, object: Option<&ReplicatedVolume>) -> Result<Outcome, AgentError> {
        // `remove` deletes a backing file if one is there, so the decision is
        // not enough: the guard looks again under the volume lock.
        let name = volume.to_string();
        let removed = self
            .blocking(move |paths, runner| {
                let guard = || match local(paths, &name) {
                    Ok(state) if state.image => Err("a backing file has appeared since the decision".to_string()),
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("cannot inspect the backing file: {e}")),
                };
                Reconciler { paths, runner }.remove_checked(&name, &guard)
            })
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("cleaned leftovers {steps:?}"));
                self.report(volume, object, None).await?;
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

    /// Names of every object and every local leftover.
    async fn all_names(&self) -> Result<BTreeSet<String>, AgentError> {
        let mut names = self
            .blocking(|paths, _| local_volumes(paths))
            .await?
            .map_err(|e| Self::reconcile_error("*", e))?;
        for object in self.volumes.list().await? {
            names.extend(object.metadata.name);
        }
        Ok(names)
    }

    /// Watches the objects and runs `reconcile_one` for the names that
    /// changed, for the names a pass asked to see again and for all names on
    /// a timer. Returns after `shutdown` is cancelled, once the pass in
    /// progress has finished.
    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), AgentError> {
        let queue = Arc::new(Queue::default());
        let stop_watch = shutdown.child_token();
        let watch = tokio::spawn(feed(self.volumes.watch(), queue.clone(), stop_watch.clone()));
        let result = self.serve(&queue, &shutdown).await;
        stop_watch.cancel();
        if let Err(e) = watch.await {
            log_error("*", format!("the watch task failed: {e}"));
        }
        result
    }

    async fn serve(&self, queue: &Queue, shutdown: &CancellationToken) -> Result<(), AgentError> {
        let mut startup = Startup::default();
        // The first listing is taken at once and, while it fails, again every
        // `ERROR_DELAY`; the queue is served meanwhile (the watch feeds it).
        let mut next_full = Instant::now();
        while !shutdown.is_cancelled() {
            let now = Instant::now();
            if now >= next_full {
                match self.all_names().await {
                    Ok(names) => {
                        next_full = now + FULL_PASS_EVERY;
                        startup.begin(&names);
                        names.iter().for_each(|name| queue.schedule(name, now));
                    }
                    Err(e) => {
                        log_error("*", format!("cannot list the volumes of this node: {e}"));
                        next_full = now + if startup.listed() { FULL_PASS_EVERY } else { ERROR_DELAY };
                    }
                }
                self.publish_readiness(&startup);
            }
            match queue.pop_due(now) {
                Ok(name) => {
                    let result = self.reconcile_one(&name).await;
                    queue.schedule(&name, Instant::now() + settle(&name, &result));
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

/// Logs an error result and says when the name is due again.
fn settle(name: &str, result: &Result<Outcome, AgentError>) -> Duration {
    match result {
        Ok(Outcome::Progress) => PROGRESS_DELAY,
        Ok(Outcome::Settled) => SETTLED_DELAY,
        Ok(Outcome::Kept) => KEPT_DELAY,
        Ok(Outcome::Refused(_)) => REFUSED_DELAY,
        Err(error) => {
            log_error(name, error);
            ERROR_DELAY
        }
    }
}

/// Names with the time of their next pass. A name is queued once; queuing it
/// again keeps the earlier time.
#[derive(Default)]
struct Queue {
    due: Mutex<BTreeMap<String, Instant>>,
    wake: Notify,
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
        let Some((name, at)) = due.iter().min_by_key(|(_, at)| **at).map(|(n, a)| (n.clone(), *a)) else {
            return Err(None);
        };
        if at <= now {
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
            Some(Ok(name)) => queue.schedule(&name, Instant::now()),
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
