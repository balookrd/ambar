use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::sync::atomic::{AtomicU32, AtomicUsize};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;

use super::*;
use crate::agent::kube::{Created, Objects, initial_sync_done_ops, patch_retrying, remove_finalizer_ops};
use crate::crd::ReplicatedVolumeSpec;
use crate::model::InitialSync;
use crate::tests::{FakeRunner, THREE_UPTODATE, configured_node, fake_node, out, prepare_copy, temp_paths};

const VOLUME: &str = "spike";
const DV1: &str = "lima-dv-1";
const DV2: &str = "lima-dv-2";
const EMPTY_KERNEL: &str = "[\n]\n\n";

// ---- fake API server ----

/// Objects (as JSON, so that fields this build does not know survive) in a
/// map and a log of the calls the agent made. `delete` follows the API
/// server: an object with finalizers gets a deletion timestamp, one without is
/// gone; removing the last finalizer of a deleting object removes it. `patch`
/// applies the JSON-patch operations `test`, `replace` and `remove`; a failed
/// `test` is `ApiError::Rejected`, as the API server's 422.
#[derive(Default)]
struct FakeVolumes {
    objects: Mutex<BTreeMap<String, Value>>,
    calls: Mutex<Vec<String>>,
    status_writes: Mutex<Vec<(String, Option<NodeStatus>)>>,
    /// The next this many `patch` calls fail with a conflict.
    conflicts: Mutex<u32>,
    version: AtomicU32,
    fail: AtomicBool,
    /// `get` of these names fails.
    failing: Mutex<BTreeSet<String>>,
    /// Runs once at the start of the next `list`.
    on_list: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    events: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

fn typed(value: &Value) -> ReplicatedVolume {
    serde_json::from_value(value.clone()).unwrap()
}

/// Applies `test`, `replace` and `remove` operations to `document`.
fn apply_ops(document: &mut Value, ops: &[Value]) -> Result<(), String> {
    for op in ops {
        let path = op["path"].as_str().ok_or("no path")?;
        match op["op"].as_str().ok_or("no op")? {
            "test" => {
                if document.pointer(path) != Some(&op["value"]) {
                    return Err(format!("test of {path} failed"));
                }
            }
            "replace" => {
                *document.pointer_mut(path).ok_or(format!("no {path}"))? = op["value"].clone();
            }
            "remove" => {
                let (parent, last) = path.rsplit_once('/').ok_or("bad path")?;
                match document.pointer_mut(parent).ok_or(format!("no {parent}"))? {
                    Value::Array(items) => {
                        let index: usize = last.parse().map_err(|_| "bad index")?;
                        if index >= items.len() {
                            return Err(format!("no {path}"));
                        }
                        items.remove(index);
                    }
                    Value::Object(map) => {
                        map.remove(last).ok_or(format!("no {path}"))?;
                    }
                    _ => return Err(format!("cannot remove {path}")),
                }
            }
            other => return Err(format!("unsupported op {other}")),
        }
    }
    Ok(())
}

impl FakeVolumes {
    fn log(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn next_version(&self) -> String {
        self.version.fetch_add(1, Ordering::SeqCst).to_string()
    }

    /// Stores the object; one without a uid gets a new one, as the API server would give.
    fn put(&self, mut volume: ReplicatedVolume) {
        volume.metadata.resource_version = Some(self.next_version());
        if volume.metadata.uid.is_none() {
            volume.metadata.uid = Some(format!("uid-{}", self.next_version()));
        }
        let name = volume.metadata.name.clone().unwrap();
        self.objects
            .lock()
            .unwrap()
            .insert(name, serde_json::to_value(&volume).unwrap());
    }

    /// Stores the JSON as it is.
    fn put_raw(&self, name: &str, value: Value) {
        self.objects.lock().unwrap().insert(name.to_string(), value);
    }

    fn raw(&self, name: &str) -> Option<Value> {
        self.objects.lock().unwrap().get(name).cloned()
    }

    fn object(&self, name: &str) -> Option<ReplicatedVolume> {
        self.raw(name).map(|value| typed(&value))
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

    /// Writes an entry the way `apply_node_status` does, without a call being logged.
    fn set_entry(&self, name: &str, entry: NodeStatus) {
        let mut objects = self.objects.lock().unwrap();
        let object = objects.get_mut(name).unwrap();
        let mut nodes: Vec<NodeStatus> = serde_json::from_value(object["status"]["nodes"].clone()).unwrap_or_default();
        nodes.retain(|n| n.name != entry.name);
        nodes.push(entry);
        object["status"] = json!({ "nodes": nodes });
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

    async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        let injected = {
            let mut left = self.conflicts.lock().unwrap();
            let injected = *left > 0;
            *left = left.saturating_sub(1);
            injected
        };
        if injected {
            self.log(format!("patch {name} conflict"));
            return Err(ApiError::Conflict("the object has been modified".into()));
        }
        let mut objects = self.objects.lock().unwrap();
        let Some(stored) = objects.get(name) else {
            return Ok(false);
        };
        let mut next = stored.clone();
        if let Err(why) = apply_ops(&mut next, &ops) {
            drop(objects);
            self.log(format!("patch {name} rejected"));
            return Err(ApiError::Rejected(why));
        }
        next["metadata"]["resourceVersion"] = json!(self.next_version());
        let empty = next["metadata"]["finalizers"]
            .as_array()
            .is_some_and(|finalizers| finalizers.is_empty());
        if empty {
            next["metadata"].as_object_mut().unwrap().remove("finalizers");
        }
        let gone = next["metadata"].get("deletionTimestamp").is_some() && next["metadata"].get("finalizers").is_none();
        if gone {
            objects.remove(name);
        } else {
            objects.insert(name.to_string(), next);
        }
        drop(objects);
        self.log(format!("patch {name}"));
        Ok(true)
    }
}

#[async_trait]
impl Volumes for FakeVolumes {
    async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        if self.fail.load(Ordering::SeqCst) || self.failing.lock().unwrap().contains(name) {
            self.log(format!("get {name} failed"));
            return Err(ApiError::Other("the API server is down".into()));
        }
        self.log(format!("get {name}"));
        Ok(self.object(name))
    }

    async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
        let hook = self.on_list.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(self.objects.lock().unwrap().values().map(typed).collect())
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
            .is_some_and(|o| o["metadata"]["finalizers"].as_array().is_some_and(|f| !f.is_empty()));
        if has_finalizers {
            objects.get_mut(name).unwrap()["metadata"]["deletionTimestamp"] = json!("1970-01-01T00:00:00Z");
        } else {
            objects.remove(name);
        }
        Ok(())
    }

    async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &|object| remove_finalizer_ops(object, finalizer)).await
    }

    async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
        patch_retrying(self, name, uid, &initial_sync_done_ops).await
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
        if self.object(name).is_some() {
            if let Some(entry) = entry {
                self.set_entry(name, entry.clone());
            } else {
                let mut objects = self.objects.lock().unwrap();
                let object = objects.get_mut(name).unwrap();
                let mut nodes: Vec<NodeStatus> =
                    serde_json::from_value(object["status"]["nodes"].clone()).unwrap_or_default();
                nodes.retain(|n| n.name != node);
                object["status"] = json!({ "nodes": nodes });
            }
        }
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
    env_sharing(node, runner, volumes_dir_exists, Arc::new(FakeVolumes::default()))
}

/// A node that talks to `volumes`, which other nodes may share.
fn env_sharing(node: &str, runner: FakeRunner, volumes_dir_exists: bool, volumes: Arc<FakeVolumes>) -> Env {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    if volumes_dir_exists {
        fs::create_dir_all(&paths.volumes_dir).unwrap();
    }
    let runner = Arc::new(runner);
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
    assert!(!e.volumes.calls().iter().any(|c| c.starts_with("patch")));
}

#[tokio::test]
async fn an_edit_that_conflicts_twice_succeeds_on_the_third_attempt() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Skip"));
    *e.volumes.conflicts.lock().unwrap() = 2;

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(e.volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Done);
    let patches: Vec<String> = e
        .volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch"))
        .collect();
    assert_eq!(patches, ["patch spike conflict", "patch spike conflict", "patch spike"]);
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

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);

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
async fn a_status_that_fails_under_the_lock_stops_the_deletion() {
    // The first status read (before the lock) works, the second (the guard
    // under the lock) fails.
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

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
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

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);

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
    // The removal of the entry is sent although the object shows none.
    assert_eq!(e.volumes.status_writes(), [(DV1.to_string(), None)]);
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

// ---- reconcile_one: when a copy may be deleted (P1) ----

const DV3: &str = "lima-dv-3";

/// `spike` with the third node of the given kind.
fn spike_with_third(kind: &str, finalizer_nodes: &[&str]) -> ReplicatedVolume {
    let mut volume = object(VOLUME, "Done", 1000, 7000, finalizer_nodes);
    volume.spec.nodes[2] = serde_json::from_value(json!({
        "name": DV3, "nodeId": 2, "address": "192.168.104.4", "kind": kind,
    }))
    .unwrap();
    volume
}

/// The status as a node with the role `role` sees `peers`: (name, connection state, peer role).
fn kernel_peers(role: &str, peers: &[(&str, &str, &str)]) -> String {
    let mut all: Value = serde_json::from_str(THREE_UPTODATE).unwrap();
    let template = all[0]["connections"][0].clone();
    let connections: Vec<Value> = peers
        .iter()
        .enumerate()
        .map(|(i, (name, state, peer_role))| {
            let mut c = template.clone();
            c["name"] = json!(name);
            c["peer-node-id"] = json!(i + 1);
            c["connection-state"] = json!(state);
            c["peer-role"] = json!(peer_role);
            c
        })
        .collect();
    all[0]["connections"] = json!(connections);
    all[0]["role"] = json!(role);
    serde_json::to_string(&all).unwrap()
}

fn reported(name: &str) -> NodeStatus {
    NodeStatus {
        name: name.to_string(),
        role: "Secondary".to_string(),
        disk: "UpToDate".to_string(),
        quorum: true,
        connections: Vec::new(),
        message: String::new(),
    }
}

/// `lima-dv-1` holds the copy of a deleting `spike`; the kernel shows
/// `kernel`; the nodes in `entries` have a `status.nodes` entry.
async fn deleting(kernel: &str, entries: &[&str], third_kind: &str) -> Env {
    let e = env(DV1, node_with_status(Arc::new(Mutex::new(kernel.to_string())), true));
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_with_third(third_kind, &[DV1, DV2]));
    for node in entries {
        e.volumes.set_entry(VOLUME, reported(node));
    }
    e.volumes.delete(VOLUME).await.unwrap();
    e
}

fn assert_kept(e: &Env, mentions: &[&str]) {
    assert!(
        commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty(),
        "{:?}",
        e.runner.calls()
    );
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    for text in mentions {
        assert!(entry.message.contains(text), "{text:?} not in {:?}", entry.message);
    }
}

fn assert_deleted(e: &Env) {
    assert!(!e.paths.image(VOLUME).exists());
    assert!(!e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
}

#[tokio::test]
async fn a_resource_that_is_not_configured_keeps_the_copy_while_another_node_has_an_entry() {
    // After a reboot nothing configures the resource of a deleting object:
    // the status shows no peers, so it proves nothing about who uses the volume.
    let e = deleting(EMPTY_KERNEL, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &[DV2, "not connected", "has not reported cleanup"]);
}

#[tokio::test]
async fn a_resource_that_is_not_configured_is_deleted_when_nobody_else_has_an_entry() {
    let e = deleting(EMPTY_KERNEL, &[], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_deleted(&e);
}

#[tokio::test]
async fn a_peer_that_is_connecting_and_has_an_entry_keeps_the_copy() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &[DV2, "not connected", "has not reported cleanup"]);
}

#[tokio::test]
async fn a_peer_that_is_connecting_and_has_no_entry_does_not_block() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_deleted(&e);
}

#[tokio::test]
async fn connected_secondary_peers_do_not_block_whatever_they_have_reported() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2, DV3], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_deleted(&e);
}

#[tokio::test]
async fn a_connected_peer_with_an_unknown_role_and_an_entry_keeps_the_copy() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &[DV2, "connected with the role Unknown"]);
}

#[tokio::test]
async fn a_connected_primary_peer_keeps_the_copy_with_or_without_an_entry() {
    for entries in [&[][..], &[DV2][..]] {
        let kernel = kernel_peers(
            "Secondary",
            &[(DV2, "Connected", "Primary"), (DV3, "Connected", "Secondary")],
        );
        let e = deleting(&kernel, entries, "Witness").await;
        assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
        assert_kept(&e, &["peer lima-dv-2 is Primary"]);
    }
}

#[tokio::test]
async fn a_client_peer_that_is_primary_keeps_the_copy() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Primary")],
    );
    let e = deleting(&kernel, &[DV3], "Client").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &["peer lima-dv-3 is Primary"]);
}

#[tokio::test]
async fn the_reason_names_every_node_that_blocks() {
    let kernel = kernel_peers(
        "Primary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &["this node is Primary", DV2]);
}

#[tokio::test]
async fn a_peer_that_becomes_primary_before_the_lock_keeps_the_copy() {
    // The first status read (before the lock) shows only Secondary peers; by
    // the time the guard reads it under the lock a peer is Primary.
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let calm = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
    );
    let busy = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Primary"), (DV3, "Connected", "Secondary")],
    );
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| match (program, args) {
            ("drbdsetup", _) => out(
                0,
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    &calm
                } else {
                    &busy
                },
                "",
            ),
            ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_kept(&e, &["peer lima-dv-2 is Primary"]);
}

#[tokio::test]
async fn the_three_replicas_of_a_deleting_volume_all_end_deleted() {
    // Three Replicas, all connected and Secondary; each has reported. The
    // volume is deleted: the first node deletes and removes its entry, the
    // second then sees the first neither connected nor reported, and so on.
    let volumes = Arc::new(FakeVolumes::default());
    let mut volume = spike_with_third("Replica", &[DV1, DV2, DV3]);
    volume.spec.nodes[2].kind = crate::crd::NodeKind::Replica;
    volumes.put(volume);
    for node in [DV1, DV2, DV3] {
        volumes.set_entry(VOLUME, reported(node));
    }
    volumes.delete(VOLUME).await.unwrap();

    let node_of = |node: &str, peers: &[(&str, &str, &str)]| {
        let kernel = Arc::new(Mutex::new(kernel_peers("Secondary", peers)));
        let e = env_sharing(node, node_with_status(kernel, true), true, volumes.clone());
        prepare_copy(&e.paths, true, true);
        e
    };
    let a = node_of(DV1, &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")]);
    let b = node_of(DV2, &[(DV1, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")]);
    let c = node_of(DV3, &[(DV1, "Connecting", "Unknown"), (DV2, "Connecting", "Unknown")]);

    // B and C must wait for A's entry to go.
    assert_eq!(b.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_eq!(c.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(b.paths.image(VOLUME).exists() && c.paths.image(VOLUME).exists());

    assert_eq!(a.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!a.paths.image(VOLUME).exists());
    assert!(volumes.entry_of(VOLUME, DV1).is_none());

    assert_eq!(b.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!b.paths.image(VOLUME).exists());
    assert_eq!(c.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!c.paths.image(VOLUME).exists());
    assert!(
        volumes.object(VOLUME).is_none(),
        "the object is gone with the last finalizer"
    );
}

#[tokio::test]
async fn a_witness_removes_its_entry_when_it_cleans_up_for_a_deleting_object() {
    let e = env(DV3, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.set_entry(VOLUME, reported(DV3));
    e.volumes.delete(VOLUME).await.unwrap();
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
}

#[tokio::test]
async fn the_removal_of_an_entry_is_sent_even_when_the_object_shows_none() {
    let e = env(DV3, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());

    e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(e.volumes.status_writes(), [(DV3.to_string(), None)]);
}

#[tokio::test]
async fn a_leftover_entry_of_a_node_with_nothing_local_is_removed_from_a_deleting_object() {
    // The cleanup worked but the removal of the entry failed on the API: the
    // node holds nothing any more, only its entry is left, and the other
    // nodes wait for it.
    let e = env(DV3, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.set_entry(VOLUME, reported(DV3));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
}

#[tokio::test]
async fn an_entry_on_a_live_object_is_not_touched_by_a_node_with_nothing_local() {
    // Not deleting: an entry of a node that holds nothing is not this node's to remove here.
    let e = env("lima-dv-4", fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.set_entry(VOLUME, reported("lima-dv-4"));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
    assert!(e.volumes.status_writes().is_empty());
}

#[tokio::test]
async fn a_failed_down_with_an_exit_code_keeps_the_copy_the_finalizer_and_reports_the_error() {
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| match (program, args) {
            ("drbdadm", ["down", _]) => out(11, "", "resource is busy"),
            ("drbdsetup", _) => out(0, &kernel.lock().unwrap(), ""),
            ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);

    assert_eq!(commands(&e.runner, &["drbdadm down"]).len(), 1);
    assert!(commands(&e.runner, &["losetup -d"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).unwrap();
    assert!(entry.message.contains("resource is busy"), "{}", entry.message);
}

#[tokio::test]
async fn an_object_that_is_created_again_during_the_pass_keeps_its_finalizers() {
    // `drbdadm down` runs in the middle of the pass; while it runs the object
    // is deleted and created again under the same name (another uid, a
    // finalizer of this node on it). The pass must not edit the new object.
    let volumes = Arc::new(FakeVolumes::default());
    let hook_volumes = volumes.clone();
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let kernel_for_runner = kernel.clone();
    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("drbdadm", ["down", _]) => {
            *kernel_for_runner.lock().unwrap() = EMPTY_KERNEL.to_string();
            let mut again = spike("Done");
            again.metadata.uid = Some("uid-of-the-new-object".to_string());
            again.metadata.deletion_timestamp = Some(serde_json::from_value(json!("1970-01-01T00:00:00Z")).unwrap());
            hook_volumes.put(again);
            out(0, "", "")
        }
        ("drbdsetup", _) => out(0, &kernel_for_runner.lock().unwrap(), ""),
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        _ => out(0, "", ""),
    });
    let e = env_sharing(DV1, runner, true, volumes.clone());
    prepare_copy(&e.paths, true, true);
    volumes.put(spike("Done"));
    volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    // The copy was deleted for the old object; the new one is untouched.
    assert!(!e.paths.image(VOLUME).exists());
    assert_eq!(uid_of_object(&volumes, VOLUME), "uid-of-the-new-object");
    assert_eq!(volumes.finalizers(VOLUME).len(), 2);
    assert!(
        !volumes
            .calls()
            .iter()
            .any(|c| c.starts_with("patch") && !c.ends_with("rejected"))
    );
}

#[tokio::test]
async fn a_kernel_resource_without_a_file_of_ambars_is_left_alone() {
    // A DRBD resource of someone else: no object, no `.res`, no link.
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let e = env(DV1, configured_node(kernel));
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Settled);
    assert!(e.runner.calls().is_empty(), "{:?}", e.runner.calls());
    assert!(e.volumes.status_writes().is_empty());
}

#[tokio::test]
async fn a_kernel_resource_with_an_applied_copy_of_ambars_is_cleaned_up() {
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let status = kernel.clone();
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| match (program, args) {
            ("drbdadm", ["down", _]) => {
                *status.lock().unwrap() = EMPTY_KERNEL.to_string();
                out(0, "", "")
            }
            ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
            _ => out(0, "", ""),
        }),
    );
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.applied_res(VOLUME), "resource spike {}").unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(commands(&e.runner, &["drbdadm down"]), ["drbdadm down spike"]);
    assert!(!e.paths.applied_res(VOLUME).exists());
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
async fn cleaning_leftovers_stops_when_a_backing_file_appears_between_the_decision_and_the_lock() {
    // The object is listed after the local state was read and before the
    // decision: the backing file that appears there is not in what `decide`
    // saw, and only the guard under the volume lock can notice it.
    let e = env("lima-dv-4", fake_node());
    e.volumes.put(spike("Done"));
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();
    let image = e.paths.image(VOLUME);
    *e.volumes.on_list.lock().unwrap() = Some(Box::new(move || fs::write(image, "data").unwrap()));

    assert_eq!(
        e.agent.reconcile_one(VOLUME).await.unwrap(),
        Outcome::Refused(Refusal::ImageWithoutMembership)
    );
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

fn uid_of_object(volumes: &FakeVolumes, name: &str) -> String {
    volumes.object(name).unwrap().metadata.uid.unwrap()
}

#[tokio::test]
async fn an_edit_gives_up_after_five_conflicts() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    let uid = uid_of_object(&volumes, VOLUME);
    *volumes.conflicts.lock().unwrap() = 5;
    let err = volumes.mark_initial_sync_done(VOLUME, &uid).await.unwrap_err();
    assert!(matches!(err, ApiError::Conflict(_)), "{err}");
    assert_eq!(volumes.calls().len(), 5);
    assert_eq!(volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Skip);
}

#[tokio::test]
async fn an_edit_without_a_change_or_without_an_object_writes_nothing() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Done"));
    let uid = uid_of_object(&volumes, VOLUME);
    // Already Done: nothing to change. No such finalizer: nothing to remove. No such object.
    assert!(!volumes.mark_initial_sync_done(VOLUME, &uid).await.unwrap());
    assert!(
        !volumes
            .remove_finalizer(VOLUME, &uid, &replica_finalizer("lima-dv-9"))
            .await
            .unwrap()
    );
    assert!(!volumes.mark_initial_sync_done("missing", &uid).await.unwrap());
    assert!(volumes.calls().is_empty());
}

#[tokio::test]
async fn an_edit_reads_the_object_again_after_a_conflict() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    let uid = uid_of_object(&volumes, VOLUME);
    *volumes.conflicts.lock().unwrap() = 1;
    let seen = Mutex::new(0);
    let ops = |object: &ReplicatedVolume| {
        *seen.lock().unwrap() += 1;
        initial_sync_done_ops(object)
    };
    assert!(patch_retrying(&volumes, VOLUME, &uid, &ops).await.unwrap());
    assert_eq!(*seen.lock().unwrap(), 2);
}

#[tokio::test]
async fn an_edit_of_an_object_with_another_uid_changes_nothing() {
    // The object was deleted and created again under the same name.
    let volumes = FakeVolumes::default();
    volumes.put(object(VOLUME, "Skip", 1000, 7000, &[DV1, DV2]));
    let old_uid = "uid-of-the-first-object";
    assert!(!volumes.mark_initial_sync_done(VOLUME, old_uid).await.unwrap());
    assert!(
        !volumes
            .remove_finalizer(VOLUME, old_uid, &replica_finalizer(DV1))
            .await
            .unwrap()
    );
    let kept = volumes.object(VOLUME).unwrap();
    assert_eq!(kept.spec.initial_sync, InitialSync::Skip);
    assert_eq!(kept.metadata.finalizers.unwrap().len(), 2);
    assert!(volumes.calls().is_empty(), "nothing was patched: {:?}", volumes.calls());
}

#[tokio::test]
async fn the_server_rejects_a_patch_when_the_uid_changed_after_the_read() {
    // The object is replaced between the read and the patch: the `test` of
    // the uid in the patch fails, the retry reads the new object and sees
    // another uid.
    struct Swapping<'a> {
        inner: &'a FakeVolumes,
    }
    #[async_trait]
    impl Objects for Swapping<'_> {
        async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
            let seen = self.inner.read(name).await;
            // A new object takes the place right after the read.
            let mut again = object(name, "Skip", 1000, 7000, &[DV1, DV2]);
            again.metadata.uid = Some("uid-of-the-new-object".into());
            self.inner.put(again);
            seen
        }

        async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
            self.inner.patch(name, ops).await
        }
    }
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    let uid = uid_of_object(&volumes, VOLUME);
    let swapping = Swapping { inner: &volumes };

    let patched = patch_retrying(&swapping, VOLUME, &uid, &initial_sync_done_ops).await;
    assert!(!patched.unwrap());
    assert_eq!(volumes.object(VOLUME).unwrap().spec.initial_sync, InitialSync::Skip);
    assert!(volumes.calls().contains(&format!("patch {VOLUME} rejected")));
}

/// Objects whose stored object is changed by `change` right after the first read.
struct Changing<'a> {
    inner: &'a FakeVolumes,
    change: Box<dyn Fn(&mut Value) + Send + Sync>,
    once: AtomicBool,
}

#[async_trait]
impl Objects for Changing<'_> {
    async fn read(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
        let seen = self.inner.read(name).await;
        if !self.once.swap(true, Ordering::SeqCst) {
            let mut raw = self.inner.raw(name).unwrap();
            (self.change)(&mut raw);
            self.inner.put_raw(name, raw);
        }
        seen
    }

    async fn patch(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        self.inner.patch(name, ops).await
    }
}

#[tokio::test]
async fn a_finalizer_is_removed_by_its_value_not_by_the_index_that_was_read() {
    // Between the read and the patch the list is reordered: index 0 is now the
    // finalizer of the other node. The patch must not remove that one.
    let volumes = FakeVolumes::default();
    volumes.put(spike("Done"));
    let uid = uid_of_object(&volumes, VOLUME);
    let reorder = Changing {
        inner: &volumes,
        change: Box::new(|raw| raw["metadata"]["finalizers"].as_array_mut().unwrap().reverse()),
        once: AtomicBool::new(false),
    };
    let finalizer = replica_finalizer(DV1);

    let removed = patch_retrying(&reorder, VOLUME, &uid, &|o| remove_finalizer_ops(o, &finalizer)).await;
    assert!(removed.unwrap());
    assert_eq!(volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert!(volumes.calls().contains(&format!("patch {VOLUME} rejected")));
}

#[tokio::test]
async fn the_initial_sync_is_set_only_from_the_value_that_was_read() {
    // Between the read and the patch someone else sets the field: the patch
    // that was computed from `Skip` is not applied, and the second read finds nothing to do.
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    let uid = uid_of_object(&volumes, VOLUME);
    let someone_else = Changing {
        inner: &volumes,
        change: Box::new(|raw| raw["spec"]["initialSync"] = json!("Done")),
        once: AtomicBool::new(false),
    };
    let patched = patch_retrying(&someone_else, VOLUME, &uid, &initial_sync_done_ops).await;
    assert!(!patched.unwrap(), "the change was not ours");
    assert!(volumes.calls().contains(&format!("patch {VOLUME} rejected")));
}

#[tokio::test]
async fn an_edit_keeps_the_fields_this_build_does_not_know() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Skip"));
    let mut raw = volumes.raw(VOLUME).unwrap();
    raw["spec"]["fieldOfALaterVersion"] = json!({"a": [1, 2]});
    raw["metadata"]["annotations"] = json!({"owner": "someone"});
    volumes.put_raw(VOLUME, raw);
    let uid = uid_of_object(&volumes, VOLUME);

    assert!(volumes.mark_initial_sync_done(VOLUME, &uid).await.unwrap());
    assert!(
        volumes
            .remove_finalizer(VOLUME, &uid, &replica_finalizer(DV1))
            .await
            .unwrap()
    );
    let after = volumes.raw(VOLUME).unwrap();
    assert_eq!(after["spec"]["fieldOfALaterVersion"], json!({"a": [1, 2]}));
    assert_eq!(after["metadata"]["annotations"], json!({"owner": "someone"}));
    assert_eq!(after["spec"]["initialSync"], json!("Done"));
    assert_eq!(after["metadata"]["finalizers"], json!([replica_finalizer(DV2)]));
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
    assert_eq!(settle("v", &Ok(Outcome::Kept)), Duration::from_secs(30));
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

// ---- the first pass does not block the loop (P3) ----

fn api_error() -> Result<Outcome, AgentError> {
    Err(AgentError::Api(ApiError::Other("down".into())))
}

fn names_of(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn startup_is_complete_when_every_name_of_the_first_listing_has_passed() {
    let mut startup = Startup::default();
    assert!(!startup.complete(), "nothing is known before the first listing");
    startup.begin(&names_of(&["a", "b"]));
    assert!(!startup.complete());
    startup.record("a", &Ok(Outcome::Settled));
    startup.record("not-in-the-listing", &Ok(Outcome::Settled));
    assert!(!startup.complete());
    // Any outcome that is not an API error is a pass.
    startup.record("b", &Ok(Outcome::Refused(Refusal::OrphanImage)));
    assert!(startup.complete());
}

#[test]
fn startup_with_no_names_is_complete_at_once() {
    let mut startup = Startup::default();
    startup.begin(&BTreeSet::new());
    assert!(startup.complete());
}

#[test]
fn startup_stops_waiting_for_a_name_after_three_api_errors() {
    let mut startup = Startup::default();
    startup.begin(&names_of(&["bad", "good"]));
    startup.record("good", &Ok(Outcome::Settled));
    startup.record("bad", &api_error());
    startup.record("bad", &api_error());
    assert!(!startup.complete(), "two failures are not enough");
    startup.record("bad", &api_error());
    assert!(startup.complete());
}

#[test]
fn an_error_that_is_not_from_the_api_counts_as_a_pass() {
    let mut startup = Startup::default();
    startup.begin(&names_of(&["a"]));
    startup.record("a", &Err(AgentError::Join("x".into())));
    assert!(startup.complete());
}

#[test]
fn a_later_listing_does_not_change_the_names_readiness_waits_for() {
    let mut startup = Startup::default();
    startup.begin(&names_of(&["a"]));
    startup.begin(&names_of(&["a", "late"]));
    startup.record("a", &Ok(Outcome::Settled));
    assert!(startup.complete());
}

/// Like `wait_for`, for tests that run in virtual time: the clock jumps while
/// the node's blocking work is still running, so it is polled with a real pause.
async fn wait_for_virtual(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..6000 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(start_paused = true)]
async fn a_name_that_always_fails_does_not_stop_the_others_and_readiness_comes() {
    let e = env(DV1, fake_node());
    e.volumes.put(object("bad", "Skip", 1001, 7001, &[DV1, DV2]));
    e.volumes.put(spike("Skip"));
    e.volumes.failing.lock().unwrap().insert("bad".to_string());
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });

    wait_for_virtual("the healthy volume", || e.volumes.entry_of(VOLUME, DV1).is_some()).await;
    wait_for_virtual("readiness", || e.agent.ready.load(Ordering::SeqCst)).await;
    let failures = e
        .volumes
        .calls()
        .iter()
        .filter(|c| c.as_str() == "get bad failed")
        .count();
    assert_eq!(failures, 3, "ready waits for three failures and no more");
    assert!(e.paths.image(VOLUME).exists());

    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn readiness_comes_when_the_only_name_fails_three_times() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Done"));
    // `list` works but `get` fails for the only name: ready after its third failure.
    e.volumes.failing.lock().unwrap().insert(VOLUME.to_string());
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    wait_for_virtual("readiness", || e.agent.ready.load(Ordering::SeqCst)).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
}

// ---- shutdown ----

#[tokio::test]
async fn shutdown_waits_for_the_pass_in_progress() {
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let inner = fake_node();
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| {
            if program == "drbdadm" && args.first() == Some(&"up") {
                started_tx.lock().unwrap().send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            inner.run(program, args).unwrap()
        }),
    );
    e.volumes.put(spike("Skip"));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    let started = Mutex::new(started_rx);
    wait_for("the pass to reach drbdadm up", || {
        started.lock().unwrap().try_recv().is_ok()
    })
    .await;

    shutdown.cancel();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!task.is_finished(), "run returned while a pass was in progress");
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());

    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("run returns once the pass has finished")
        .unwrap()
        .unwrap();
    assert!(
        e.volumes.entry_of(VOLUME, DV1).is_some(),
        "the pass ran to its end, status included"
    );
}
