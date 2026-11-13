//! The agent's reconcile loop: for a `ReplicatedVolume` name, read the
//! object and the local state, call `decide`, carry the task out with
//! `Reconciler` and report this node's entry of `status.nodes`.
//!
//! Data-safety rules this module enforces on top of `decide`:
//!
//! * The node's copy is deleted (`Reconciler::remove` of a node with a
//!   backing file) only for `Task::DeleteReplica`, and only when the local
//!   DRBD status shows neither this node nor a peer in the role `Primary`
//!   (R18). A status that cannot be read means "do not delete".
//! * `Task::CleanDiskless` reads the local state again under the volume lock
//!   right before `remove`; a backing file that has appeared since the
//!   decision stops it, because `Reconciler::remove` deletes a backing file.
//! * `Task::Refuse` changes nothing on the node.
//! * The finalizer of this node is removed only after `remove` succeeded, or
//!   (`Task::DropFinalizer`) when nothing was left on the node.
//!
//! The loop. One task runs `reconcile_one` for the names of a queue, one
//! after another. A name is queued at once when the watch reports its
//! object, and again after a pass: after 1 s when the pass made progress, 30 s
//! when it settled, 60 s when it refused, 5 s on an error. Every 30 s the
//! names of all objects and all local leftovers are queued, which finds
//! leftovers of objects that are gone. `ready` becomes true after the first
//! pass over all those names finished without an API error.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::crd::{NodeStatus, ReplicatedVolume};
use crate::drbd::adm::Drbd;
use crate::drbd::status::{ResourceStatus, Role};
use crate::exec::Runner;
use crate::membership::{collisions, replica_finalizer};
use crate::model::{InitialSync, valid_volume_name};
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
/// Next pass after a refusal.
const REFUSED_DELAY: Duration = Duration::from_secs(60);
/// Next pass after an error.
const ERROR_DELAY: Duration = Duration::from_secs(5);
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
    /// Set after the first full pass; CSI Node answers UNAVAILABLE before that.
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

/// What the node holds for one volume: the kernel is asked first, then the files.
fn local_state(paths: &Paths, runner: &dyn Runner, volume: &str) -> Result<Local, ReconcileError> {
    let status = Drbd { runner }.status(volume)?;
    let configured: BTreeSet<String> = status.iter().map(|s| s.name.clone()).collect();
    local(paths, &configured, volume)
}

/// Why the local copy must not be deleted, when the status shows a Primary.
fn primary_in(status: &ResourceStatus) -> Option<String> {
    if status.role == Role::Primary {
        return Some("this node is Primary".to_string());
    }
    status
        .connections
        .iter()
        .find(|c| c.peer_role == Role::Primary)
        .map(|c| format!("peer {} is Primary", c.name))
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
        self.blocking(move |paths, runner| local_state(paths, runner, &name))
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
    /// `true` when the entry was written. Nothing is written for an object
    /// that does not exist.
    async fn report(
        &self,
        volume: &str,
        object: Option<&ReplicatedVolume>,
        entry: Option<NodeStatus>,
    ) -> Result<bool, AgentError> {
        let Some(object) = object else {
            return Ok(false);
        };
        let current = object
            .status
            .as_ref()
            .and_then(|status| status.nodes.iter().find(|n| n.name == self.node));
        if current == entry.as_ref() {
            return Ok(false);
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

    /// Removes this node's finalizer from the object. `true` when it was there.
    async fn drop_finalizer(&self, volume: &str) -> Result<bool, AgentError> {
        let finalizer = replica_finalizer(&self.node);
        let edit = move |object: &mut ReplicatedVolume| {
            let Some(finalizers) = object.metadata.finalizers.as_mut() else {
                return false;
            };
            let before = finalizers.len();
            finalizers.retain(|f| *f != finalizer);
            let removed = finalizers.len() != before;
            if finalizers.is_empty() {
                object.metadata.finalizers = None;
            }
            removed
        };
        Ok(self.volumes.edit(volume, &edit).await?.is_some())
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
            Task::Ignore => Ok(Outcome::Settled),
            Task::Apply { spec, mark_done } => self.apply(volume, object.as_ref(), spec, mark_done).await,
            Task::DeleteReplica => self.delete_replica(volume, object.as_ref()).await,
            Task::DropFinalizer => {
                self.report(volume, object.as_ref(), None).await?;
                let dropped = self.drop_finalizer(volume).await?;
                if dropped {
                    log_info(volume, "nothing is left on this node: finalizer dropped");
                }
                Ok(Outcome::Progress)
            }
            Task::CleanDiskless => self.clean_diskless(volume, object.as_ref()).await,
            Task::Refuse(refusal) => {
                log_error(volume, format!("refused: {refusal}"));
                self.report_message(volume, object.as_ref(), &refusal.to_string())
                    .await?;
                Ok(Outcome::Refused(refusal))
            }
        }
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
        if mark_done {
            // The leader's copy was seen in sync (the synced marker): later
            // passes must not skip the initial sync again.
            let edit = |object: &mut ReplicatedVolume| {
                if object.spec.initial_sync == InitialSync::Skip {
                    object.spec.initial_sync = InitialSync::Done;
                    true
                } else {
                    false
                }
            };
            if self.volumes.edit(volume, &edit).await?.is_some() {
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

    async fn delete_replica(&self, volume: &str, object: Option<&ReplicatedVolume>) -> Result<Outcome, AgentError> {
        // R18: no copy is deleted while this node or a peer is Primary, and
        // a status that cannot be read is no proof that nobody is.
        let status = match self.read_status(volume).await {
            Ok(status) => status,
            Err(error) => {
                let message = format!("the local copy is kept: cannot read the DRBD status: {error}");
                let entry = node_status(&self.node, None, &message);
                if self.report(volume, object, Some(entry)).await.unwrap_or(false) {
                    log_error(volume, &message);
                }
                return Err(Self::reconcile_error(volume, error));
            }
        };
        if let Some(who) = status.as_ref().and_then(primary_in) {
            let message = format!("the local copy is kept while a Primary exists: {who}");
            let entry = node_status(&self.node, status.as_ref(), &message);
            if self.report(volume, object, Some(entry)).await? {
                log_error(volume, &message);
            }
            return Ok(Outcome::Progress);
        }
        let name = volume.to_string();
        let removed = self
            .blocking(move |paths, runner| Reconciler { paths, runner }.remove(&name))
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("removed the local copy {steps:?}"));
                self.report(volume, object, None).await?;
                self.drop_finalizer(volume).await?;
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
            Err(error) => {
                // `drbdadm down` is refused while the volume is mounted, for
                // one. The finalizer and the files stay.
                let message = format!("the local copy is kept: {error}");
                if self.report_message(volume, object, &message).await? {
                    log_error(volume, &message);
                }
                Ok(Outcome::Progress)
            }
        }
    }

    async fn clean_diskless(&self, volume: &str, object: Option<&ReplicatedVolume>) -> Result<Outcome, AgentError> {
        // `remove` deletes a backing file if one is there, so the decision
        // is not enough: the state is read again under the lock.
        let fresh = self.read_local(volume).await?;
        if fresh.image {
            let refusal = if object.is_some() {
                Refusal::ImageWithoutMembership
            } else {
                Refusal::OrphanImage
            };
            log_error(
                volume,
                format!("a backing file has appeared since the decision; nothing was cleaned: {refusal}"),
            );
            return Ok(Outcome::Refused(refusal));
        }
        let name = volume.to_string();
        let removed = self
            .blocking(move |paths, runner| Reconciler { paths, runner }.remove(&name))
            .await?;
        match removed {
            Ok(steps) => {
                log_info(volume, format!("cleaned leftovers {steps:?}"));
                self.report(volume, object, None).await?;
                Ok(Outcome::Progress)
            }
            Err(ReconcileError::Busy { .. }) => Ok(Outcome::Progress),
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
            .blocking(|paths, runner| local_volumes(paths, runner))
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
        if !self.first_pass(queue, shutdown).await {
            return Ok(());
        }
        let mut next_full = Instant::now() + FULL_PASS_EVERY;
        while !shutdown.is_cancelled() {
            let now = Instant::now();
            if now >= next_full {
                next_full = now + FULL_PASS_EVERY;
                match self.all_names().await {
                    Ok(names) => names.iter().for_each(|name| queue.schedule(name, now)),
                    Err(e) => log_error("*", format!("cannot list the volumes of this node: {e}")),
                }
            }
            match queue.pop_due(now) {
                Ok(name) => {
                    let result = self.reconcile_one(&name).await;
                    queue.schedule(&name, Instant::now() + settle(&name, &result));
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

    /// The first pass over all names. `ready` is set once no name failed on
    /// the API. `false` when `shutdown` came first.
    async fn first_pass(&self, queue: &Queue, shutdown: &CancellationToken) -> bool {
        let mut pending = loop {
            match self.all_names().await {
                Ok(names) => break names,
                Err(e) => log_error("*", format!("cannot list the volumes of this node: {e}")),
            }
            if !pause(ERROR_DELAY, shutdown).await {
                return false;
            }
        };
        loop {
            let mut api_failed = BTreeSet::new();
            for name in std::mem::take(&mut pending) {
                if shutdown.is_cancelled() {
                    return false;
                }
                let result = self.reconcile_one(&name).await;
                if matches!(result, Err(AgentError::Api(_))) {
                    api_failed.insert(name);
                } else {
                    queue.schedule(&name, Instant::now() + settle(&name, &result));
                }
            }
            if api_failed.is_empty() {
                self.ready.store(true, Ordering::SeqCst);
                return true;
            }
            pending = api_failed;
            if !pause(ERROR_DELAY, shutdown).await {
                return false;
            }
        }
    }
}

/// Logs an error result and says when the name is due again.
fn settle(name: &str, result: &Result<Outcome, AgentError>) -> Duration {
    match result {
        Ok(Outcome::Progress) => PROGRESS_DELAY,
        Ok(Outcome::Settled) => SETTLED_DELAY,
        Ok(Outcome::Refused(_)) => REFUSED_DELAY,
        Err(error) => {
            log_error(name, error);
            ERROR_DELAY
        }
    }
}

/// Sleeps; `false` when `shutdown` came first.
async fn pause(delay: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        () = shutdown.cancelled() => false,
        () = sleep(delay) => true,
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
