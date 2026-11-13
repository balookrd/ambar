use std::collections::BTreeMap;
use std::fs::{self, File};
use std::sync::atomic::{AtomicU32, AtomicUsize};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;

use super::*;
use crate::agent::kube::{Created, Edit, Objects, edit_retrying};
use crate::crd::ReplicatedVolumeSpec;
use crate::tests::{FakeRunner, THREE_UPTODATE, fake_node, out, prepare_copy, temp_paths};

const VOLUME: &str = "spike";
const DV1: &str = "lima-dv-1";
const DV2: &str = "lima-dv-2";
const EMPTY_KERNEL: &str = "[\n]\n\n";

// ---- fake API server ----

/// Objects in a map and a log of the calls the agent made. `delete` follows
/// the API server: an object with finalizers gets a deletion timestamp, one
/// without is gone; removing the last finalizer of a deleting object removes it.
#[derive(Default)]
struct FakeVolumes {
    objects: Mutex<BTreeMap<String, ReplicatedVolume>>,
    calls: Mutex<Vec<String>>,
    status_writes: Mutex<Vec<(String, Option<NodeStatus>)>>,
    /// The next this many `replace` calls fail with a conflict.
    conflicts: Mutex<u32>,
    version: AtomicU32,
    fail: AtomicBool,
    events: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl FakeVolumes {
    fn log(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn put(&self, mut volume: ReplicatedVolume) {
        volume.metadata.resource_version = Some(self.version.fetch_add(1, Ordering::SeqCst).to_string());
        let name = volume.metadata.name.clone().unwrap();
        self.objects.lock().unwrap().insert(name, volume);
    }

    fn object(&self, name: &str) -> Option<ReplicatedVolume> {
        self.objects.lock().unwrap().get(name).cloned()
    }

    fn finalizers(&self, name: &str) -> Vec<String> {
        self.object(name)
            .and_then(|o| o.metadata.finalizers)
            .unwrap_or_default()
    }

    fn status_writes(&self) -> Vec<(String, Option<NodeStatus>)> {
        self.status_writes.lock().unwrap().clone()
    }

    fn entry_of(&self, name: &str, node: &str) -> Option<NodeStatus> {
        self.object(name)?.status?.nodes.into_iter().find(|n| n.name == node)
    }

    fn events(&self) -> mpsc::UnboundedSender<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.events.lock().unwrap() = Some(rx);
        tx
    }
}

#[async_trait]
impl Objects for FakeVolumes {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        Ok(self.object(name))
    }

    async fn replace(&self, name: &str, volume: &ReplicatedVolume) -> Result<ReplicatedVolume, ApiError> {
        let mut objects = self.objects.lock().unwrap();
        let injected = {
            let mut left = self.conflicts.lock().unwrap();
            let injected = *left > 0;
            *left = left.saturating_sub(1);
            injected
        };
        let stored = objects.get(name).ok_or_else(|| ApiError::Other("not found".into()))?;
        if injected || stored.metadata.resource_version != volume.metadata.resource_version {
            drop(objects);
            self.log(format!("replace {name} conflict"));
            return Err(ApiError::Conflict("the object has been modified".into()));
        }
        let mut next = volume.clone();
        next.metadata.resource_version = Some(self.version.fetch_add(1, Ordering::SeqCst).to_string());
        // The status is a subresource: a replace never changes it.
        next.status = stored.status.clone();
        let gone = next.metadata.deletion_timestamp.is_some() && next.metadata.finalizers.is_none();
        if gone {
            objects.remove(name);
        } else {
            objects.insert(name.to_string(), next.clone());
        }
        drop(objects);
        self.log(format!("replace {name}"));
        Ok(next)
    }
}

#[async_trait]
impl Volumes for FakeVolumes {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(ApiError::Other("the API server is down".into()));
        }
        self.log(format!("get {name}"));
        Ok(self.object(name))
    }

    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        Ok(self.objects.lock().unwrap().values().cloned().collect())
    }

    async fn create(&self, volume: &ReplicatedVolume) -> Result<Created, ApiError> {
        let name = volume.metadata.name.clone().unwrap();
        if self.object(&name).is_some() {
            return Ok(Created::Exists);
        }
        self.put(volume.clone());
        Ok(Created::New)
    }

    async fn delete(&self, name: &str) -> Result<(), ApiError> {
        let mut objects = self.objects.lock().unwrap();
        let has_finalizers = objects
            .get(name)
            .is_some_and(|o| o.metadata.finalizers.as_ref().is_some_and(|f| !f.is_empty()));
        if has_finalizers {
            objects.get_mut(name).unwrap().metadata.deletion_timestamp =
                Some(Time(k8s_openapi::jiff::Timestamp::UNIX_EPOCH));
        } else {
            objects.remove(name);
        }
        Ok(())
    }

    async fn edit(&self, name: &str, edit: Edit<'_>) -> Result<Option<ReplicatedVolume>, ApiError> {
        edit_retrying(self, name, edit).await
    }

    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError> {
        self.log(format!(
            "status {name} {node} {}",
            if entry.is_some() { "set" } else { "removed" }
        ));
        self.status_writes
            .lock()
            .unwrap()
            .push((node.to_string(), entry.cloned()));
        let mut objects = self.objects.lock().unwrap();
        let Some(object) = objects.get_mut(name) else {
            return Ok(());
        };
        let status = object.status.get_or_insert_with(Default::default);
        status.nodes.retain(|n| n.name != node);
        status.nodes.extend(entry.cloned());
        Ok(())
    }

    fn watch(&self) -> BoxStream<'static, Result<String, ApiError>> {
        match self.events.lock().unwrap().take() {
            Some(rx) => stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|name| (Ok(name), rx)) }).boxed(),
            None => stream::pending().boxed(),
        }
    }
}

// ---- objects ----

fn spec_json(initial_sync: &str, minor: u32, port: u16) -> ReplicatedVolumeSpec {
    serde_json::from_value(json!({
        "sizeBytes": 1073741824u64, "protocol": "C", "minor": minor, "port": port, "maxPeers": 7,
        "initialSync": initial_sync,
        "nodes": [
            {"name": DV1, "nodeId": 0, "address": "192.168.104.1", "kind": "Replica"},
            {"name": DV2, "nodeId": 1, "address": "192.168.104.3", "kind": "Replica"},
            {"name": "lima-dv-3", "nodeId": 2, "address": "192.168.104.4", "kind": "Witness"},
        ],
    }))
    .unwrap()
}

fn object(name: &str, initial_sync: &str, minor: u32, port: u16, finalizer_nodes: &[&str]) -> ReplicatedVolume {
    let mut volume = ReplicatedVolume::new(name, spec_json(initial_sync, minor, port));
    volume.metadata.finalizers = Some(finalizer_nodes.iter().map(|n| replica_finalizer(n)).collect());
    volume
}

/// The volume of the stand description, finalizers of both Replica nodes.
fn spike(initial_sync: &str) -> ReplicatedVolume {
    object(VOLUME, initial_sync, 1000, 7000, &[DV1, DV2])
}

// ---- nodes ----

/// `drbdsetup status --json` of `spike` after the skip of the initial sync,
/// as `node` sees it, with `edit` applied.
fn kernel_json(node: &str, edit: impl FnOnce(&mut Value)) -> String {
    let mut all: Value = serde_json::from_str(THREE_UPTODATE).unwrap();
    let status = &mut all[0];
    if node == DV2 {
        // The sample was captured on lima-dv-1: this is the same volume seen from lima-dv-2.
        status["node-id"] = json!(1);
        status["connections"][0]["name"] = json!(DV1);
        status["connections"][0]["peer-node-id"] = json!(0);
    }
    edit(status);
    serde_json::to_string(&all).unwrap()
}

/// A node where the copy of `spike` is attached to /dev/loop7 and the kernel
/// shows `status`; `drbdadm down` empties it when `down_clears`.
fn node_with_status(status: Arc<Mutex<String>>, down_clears: bool) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["down", _]) if down_clears => {
            *status.lock().unwrap() = EMPTY_KERNEL.to_string();
            out(0, "", "")
        }
        ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
        _ => out(0, "", ""),
    })
}

struct Env {
    paths: Paths,
    runner: Arc<FakeRunner>,
    volumes: Arc<FakeVolumes>,
    agent: Arc<Agent>,
    _tmp: TempDir,
}

fn env_for(node: &str, runner: FakeRunner, volumes_dir_exists: bool) -> Env {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    if volumes_dir_exists {
        fs::create_dir_all(&paths.volumes_dir).unwrap();
    }
    let runner = Arc::new(runner);
    let volumes = Arc::new(FakeVolumes::default());
    let agent = Arc::new(Agent {
        node: node.to_string(),
        paths: paths.clone(),
        runner: runner.clone(),
        volumes: volumes.clone(),
        locks: VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(false)),
    });
    Env {
        paths,
        runner,
        volumes,
        agent,
        _tmp: tmp,
    }
}

fn env(node: &str, runner: FakeRunner) -> Env {
    env_for(node, runner, true)
}

/// A node that holds a synced, applied copy of `spike` and shows `kernel`.
fn configured_env(node: &str, kernel: String, down_clears: bool) -> (Env, Arc<Mutex<String>>) {
    let status = Arc::new(Mutex::new(kernel));
    let e = env(node, node_with_status(status.clone(), down_clears));
    prepare_copy(&e.paths, true, true);
    (e, status)
}

fn commands(runner: &FakeRunner, prefixes: &[&str]) -> Vec<String> {
    runner
        .calls()
        .into_iter()
        .filter(|c| prefixes.iter().any(|p| c.starts_with(p)))
        .collect()
}

// ---- reconcile_one: apply ----

#[tokio::test]
async fn a_new_replica_is_applied_and_its_status_reported() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Skip"));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(commands(&e.runner, &["drbdadm up"]), ["drbdadm up spike"]);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("status written");
    assert_eq!(entry.disk, "UpToDate");
    assert_eq!(entry.message, "");
    assert_eq!(e.volumes.status_writes().len(), 1);
}

#[tokio::test]
async fn an_unchanged_volume_does_not_write_its_status_twice() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Done"));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
    assert_eq!(e.volumes.status_writes().len(), 1);
}

#[tokio::test]
async fn a_changed_status_is_written_again() {
    let (e, status) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Done"));
    e.agent.reconcile_one(VOLUME).await.unwrap();

    // A peer drops out: the entry differs from what the object shows.
    *status.lock().unwrap() = kernel_json(DV1, |s| s["connections"][0]["connection-state"] = json!("Connecting"));
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(e.volumes.status_writes().len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).unwrap();
    assert!(entry.connections.iter().any(|c| c.state == "Connecting"));
}

#[tokio::test]
async fn the_leader_marks_the_initial_sync_done_once_the_synced_marker_exists() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Skip"));

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(e.volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Done);
}

#[tokio::test]
async fn a_node_that_is_not_the_leader_leaves_the_object_alone() {
    let (e, _) = configured_env(DV2, kernel_json(DV2, |_| {}), true);
    e.volumes.put(spike("Skip"));

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(e.volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Skip);
    assert!(!e.volumes.calls().iter().any(|c| c.starts_with("replace")));
}

#[tokio::test]
async fn an_edit_that_conflicts_twice_succeeds_on_the_third_attempt() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Skip"));
    *e.volumes.conflicts.lock().unwrap() = 2;

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(e.volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Done);
    let replaces: Vec<String> = e
        .volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("replace"))
        .collect();
    assert_eq!(
        replaces,
        ["replace spike conflict", "replace spike conflict", "replace spike"]
    );
}

#[tokio::test]
async fn a_busy_volume_is_retried_without_writing_status() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Skip"));
    // Another ambar process (the CLI) holds the volume lock.
    let held = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(e.paths.lock_file(VOLUME))
        .unwrap();
    held.try_lock().unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.status_writes().is_empty());
    assert!(!e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn a_failed_apply_reports_its_error_in_the_status() {
    // The volumes directory does not exist: `apply` fails and never creates it.
    let e = env_for(DV1, fake_node(), false);
    e.volumes.put(spike("Skip"));

    let err = e.agent.reconcile_one(VOLUME).await.unwrap_err();
    assert!(matches!(err, AgentError::Reconcile { .. }), "{err}");
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("status written");
    assert!(entry.message.contains("does not exist"), "{}", entry.message);
    assert!(!e.paths.volumes_dir.exists());
}

#[tokio::test]
async fn an_api_error_is_returned_as_such() {
    let e = env(DV1, fake_node());
    e.volumes.fail.store(true, Ordering::SeqCst);
    let err = e.agent.reconcile_one(VOLUME).await.unwrap_err();
    assert!(matches!(err, AgentError::Api(_)), "{err}");
}

// ---- reconcile_one: deletion ----

#[tokio::test]
async fn a_deleted_replica_is_removed_and_only_its_finalizer_goes() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Done"));
    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert!(e.volumes.entry_of(VOLUME, DV1).is_some());
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    assert_eq!(commands(&e.runner, &["drbdadm down"]), ["drbdadm down spike"]);
    assert!(!e.paths.image(VOLUME).exists());
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none(), "the status entry is removed");
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert_eq!(e.volumes.status_writes().last().unwrap(), &(DV1.to_string(), None));
}

#[tokio::test]
async fn the_last_replica_that_removes_its_finalizer_lets_the_object_go() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(object(VOLUME, "Done", 1000, 7000, &[DV1]));
    e.volumes.delete(VOLUME).await.unwrap();

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert!(e.volumes.object(VOLUME).is_none());
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
}

/// R18: a deleting volume with a Primary on this node or on a peer keeps its copy.
async fn kept_while_primary(edit: impl FnOnce(&mut Value), expected_reason: &str) {
    let (e, _) = configured_env(DV1, kernel_json(DV1, edit), true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    assert!(
        commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty(),
        "{:?}",
        e.runner.calls()
    );
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains("Primary"), "{}", entry.message);
    assert!(entry.message.contains(expected_reason), "{}", entry.message);
}

#[tokio::test]
async fn a_copy_is_kept_while_a_peer_is_primary() {
    kept_while_primary(|s| s["connections"][1]["peer-role"] = json!("Primary"), "lima-dv-3").await;
}

#[tokio::test]
async fn a_copy_is_kept_while_this_node_is_primary() {
    kept_while_primary(|s| s["role"] = json!("Primary"), "this node").await;
}

#[tokio::test]
async fn a_copy_is_kept_when_the_status_cannot_be_read() {
    let e = env(
        DV1,
        FakeRunner::new(|program, _| match program {
            "drbdsetup" => out(1, "", "no kernel module"),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    // The state cannot even be read before the decision: nothing is deleted.
    assert!(e.agent.reconcile_one(VOLUME).await.is_err());
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
}

#[tokio::test]
async fn a_status_that_fails_after_the_decision_stops_the_deletion() {
    // The first status read (the local state) works, the second (R18) fails.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let kernel = kernel_json(DV1, |_| {});
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| match (program, args) {
            ("drbdsetup", _) => {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    out(0, &kernel, "")
                } else {
                    out(1, "", "netlink busy")
                }
            }
            ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert!(e.agent.reconcile_one(VOLUME).await.is_err());
    assert!(commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(
        entry.message.contains("cannot read the DRBD status"),
        "{}",
        entry.message
    );
}

#[tokio::test]
async fn a_failed_down_keeps_the_copy_the_finalizer_and_reports_the_error() {
    // The kernel keeps showing the resource after `drbdadm down`, as it does
    // while the device is mounted.
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), false);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    assert_eq!(commands(&e.runner, &["drbdadm down"]).len(), 1);
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).unwrap();
    assert!(entry.message.contains("did not take effect"), "{}", entry.message);
}

#[tokio::test]
async fn a_deleting_replica_with_nothing_local_only_drops_its_finalizer() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
}

#[tokio::test]
async fn a_stray_finalizer_of_a_node_that_is_no_replica_is_dropped() {
    let e = env("lima-dv-4", fake_node());
    e.volumes
        .put(object(VOLUME, "Done", 1000, 7000, &[DV1, DV2, "lima-dv-4"]));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(
        e.volumes.finalizers(VOLUME),
        [replica_finalizer(DV1), replica_finalizer(DV2)]
    );
}

// ---- reconcile_one: no object, not a member, refusals ----

#[tokio::test]
async fn an_image_without_an_object_is_refused_and_nothing_is_touched() {
    let e = env(DV1, fake_node());
    prepare_copy(&e.paths, true, true);

    assert_eq!(
        e.agent.reconcile_one(VOLUME).await.unwrap(),
        Outcome::Refused(Refusal::OrphanImage)
    );
    assert!(
        commands(&e.runner, &["drbdadm", "losetup"]).is_empty(),
        "{:?}",
        e.runner.calls()
    );
    for path in [
        e.paths.image(VOLUME),
        e.paths.ready_marker(VOLUME),
        e.paths.synced_marker(VOLUME),
        e.paths.res_file(VOLUME),
        e.paths.applied_res(VOLUME),
    ] {
        assert!(path.exists(), "{} was deleted", path.display());
    }
    assert!(e.volumes.status_writes().is_empty());
}

#[tokio::test]
async fn leftovers_of_a_node_that_left_the_volume_are_cleaned() {
    let e = env("lima-dv-4", fake_node());
    e.volumes.put(spike("Done"));
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.res_file(VOLUME).exists());
}

#[tokio::test]
async fn leftovers_without_an_object_are_cleaned() {
    let e = env(DV1, fake_node());
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.res_file(VOLUME).exists());
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
}

#[tokio::test]
async fn cleaning_leftovers_stops_when_a_backing_file_appears_after_the_decision() {
    // The second status read (the one before `remove`) is the moment a copy
    // shows up: the decision was made without it.
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let tmp_image = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let image_slot = tmp_image.clone();
    let e = env(
        DV1,
        FakeRunner::new(move |program, _| match program {
            "drbdsetup" => {
                if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                    let image = image_slot.lock().unwrap().clone().unwrap();
                    fs::write(image, "data").unwrap();
                }
                out(0, EMPTY_KERNEL, "")
            }
            _ => out(0, "", ""),
        }),
    );
    *tmp_image.lock().unwrap() = Some(e.paths.image(VOLUME));
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();

    assert_eq!(
        e.agent.reconcile_one(VOLUME).await.unwrap(),
        Outcome::Refused(Refusal::OrphanImage)
    );
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(fs::read_to_string(e.paths.image(VOLUME)).unwrap(), "data");
    assert!(e.paths.res_file(VOLUME).exists());
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
}

#[tokio::test]
async fn a_minor_collision_refuses_and_names_the_other_volume() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Skip"));
    e.volumes.put(object("other", "Skip", 1000, 7001, &[DV1, DV2]));

    let outcome = e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(outcome, Outcome::Refused(Refusal::Collision(vec!["other".to_string()])));
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
    assert!(!e.paths.image(VOLUME).exists());
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains("other"), "{}", entry.message);
}

#[tokio::test]
async fn a_refusal_does_not_change_the_local_state() {
    // A member Witness holding a backing file: refused, files stay.
    let e = env("lima-dv-3", fake_node());
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));

    let outcome = e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(outcome, Outcome::Refused(Refusal::ImageWithoutMembership));
    assert!(e.paths.image(VOLUME).exists());
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
    assert!(
        e.volumes
            .entry_of(VOLUME, "lima-dv-3")
            .unwrap()
            .message
            .contains("not a Replica")
    );
}

#[tokio::test]
async fn a_name_that_is_not_a_volume_name_is_refused_before_anything_is_read() {
    let e = env(DV1, fake_node());
    for name in ["../escape", "UPPER", "all", ""] {
        let outcome = e.agent.reconcile_one(name).await.unwrap();
        assert!(matches!(outcome, Outcome::Refused(Refusal::InvalidSpec(_))), "{name:?}");
    }
    assert!(e.runner.calls().is_empty());
    assert!(e.volumes.calls().is_empty());
}

// ---- the edit contract ----

#[tokio::test]
async fn edit_gives_up_after_five_conflicts() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    *volumes.conflicts.lock().unwrap() = 5;
    let err = volumes.edit(VOLUME, &|_| true).await.unwrap_err();
    assert!(matches!(err, ApiError::Conflict(_)), "{err}");
    assert_eq!(volumes.calls().len(), 5);
}

#[tokio::test]
async fn edit_without_a_change_or_without_an_object_writes_nothing() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    assert!(volumes.edit(VOLUME, &|_| false).await.unwrap().is_none());
    assert!(volumes.edit("missing", &|_| true).await.unwrap().is_none());
    assert!(volumes.calls().is_empty());
}

#[tokio::test]
async fn edit_reads_the_object_again_after_a_conflict() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    *volumes.conflicts.lock().unwrap() = 1;
    let seen = Mutex::new(Vec::new());
    let edit = |object: &mut ReplicatedVolume| {
        seen.lock().unwrap().push(object.metadata.resource_version.clone());
        object.metadata.labels = Some([("a".to_string(), "b".to_string())].into());
        true
    };
    assert!(volumes.edit(VOLUME, &edit).await.unwrap().is_some());
    assert_eq!(seen.lock().unwrap().len(), 2);
}

// ---- startup, locks, queue ----

#[test]
fn preflight_accepts_a_matching_host_name_and_an_existing_volumes_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    assert!(preflight("node-a", "node-a\n", &paths).is_ok());
}

#[test]
fn preflight_rejects_a_node_name_that_is_not_the_host_name() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let err = preflight("node-a", "node-b\n", &paths).unwrap_err();
    assert!(err.to_string().contains("node-b"), "{err}");
}

#[test]
fn preflight_rejects_a_missing_volumes_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let err = preflight("node-a", "node-a", &paths).unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
    assert!(!paths.volumes_dir.exists());
}

#[tokio::test]
async fn the_lock_of_a_volume_excludes_a_second_holder_but_not_another_volume() {
    let locks = VolumeLocks::default();
    let first = locks.lock("a").await;
    let _other = tokio::time::timeout(Duration::from_millis(500), locks.lock("b"))
        .await
        .expect("another volume is not blocked");
    let blocked = tokio::time::timeout(Duration::from_millis(50), locks.lock("a")).await;
    assert!(blocked.is_err(), "the same volume must wait");
    drop(first);
    tokio::time::timeout(Duration::from_millis(500), locks.lock("a"))
        .await
        .expect("released");
}

#[test]
fn the_delay_after_a_pass_depends_on_its_outcome() {
    assert_eq!(settle("v", &Ok(Outcome::Progress)), Duration::from_secs(1));
    assert_eq!(settle("v", &Ok(Outcome::Settled)), Duration::from_secs(30));
    assert_eq!(
        settle("v", &Ok(Outcome::Refused(Refusal::OrphanImage))),
        Duration::from_secs(60)
    );
    assert_eq!(
        settle("v", &Err(AgentError::Api(ApiError::Other("down".into())))),
        Duration::from_secs(5)
    );
}

#[test]
fn the_queue_keeps_a_name_once_at_its_earliest_time() {
    let queue = Queue::default();
    let now = Instant::now();
    queue.schedule("a", now + Duration::from_secs(30));
    queue.schedule("a", now);
    queue.schedule("a", now + Duration::from_secs(60));
    queue.schedule("b", now + Duration::from_secs(5));

    assert_eq!(queue.pop_due(now), Ok("a".to_string()));
    assert_eq!(queue.pop_due(now), Err(Some(now + Duration::from_secs(5))));
    assert_eq!(queue.pop_due(now + Duration::from_secs(5)), Ok("b".to_string()));
    assert_eq!(queue.pop_due(now + Duration::from_secs(100)), Err(None));
}

// ---- run ----

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn run_cleans_leftovers_in_the_first_pass_and_becomes_ready() {
    let e = env(DV1, fake_node());
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file("leftover"), "resource leftover {}").unwrap();
    assert!(!e.agent.ready.load(Ordering::SeqCst));

    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    wait_for("ready", || e.agent.ready.load(Ordering::SeqCst)).await;
    assert!(
        !e.paths.res_file("leftover").exists(),
        "ready only after the first pass"
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("run returns after the shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn run_reconciles_an_object_the_watch_reports() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    let events = e.volumes.events();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    wait_for("ready", || e.agent.ready.load(Ordering::SeqCst)).await;
    assert!(e.volumes.status_writes().is_empty());

    e.volumes.put(spike("Done"));
    events.send(VOLUME.to_string()).unwrap();
    wait_for("the status of the new object", || {
        e.volumes.entry_of(VOLUME, DV1).is_some()
    })
    .await;

    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn run_is_not_ready_while_the_api_fails() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.fail.store(true, Ordering::SeqCst);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!e.agent.ready.load(Ordering::SeqCst));

    shutdown.cancel();
    task.await.unwrap().unwrap();
}
