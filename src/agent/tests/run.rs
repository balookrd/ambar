use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::agent::kube::{
    Objects, initial_sync_done_ops, patch_retrying, patch_status_retrying, remove_finalizer_ops, remove_node_entry_ops,
};
use crate::crd::{MemberNode, ReplicatedVolumeSpec};
use crate::drbd::status::DiskState;
use crate::exec::Output;
use crate::model::InitialSync;
use crate::tests::volumes::FakeVolumes;
use crate::tests::{FakeRunner, THREE_UPTODATE, configured_node, fake_node, out, prepare_copy, temp_paths};

const VOLUME: &str = "spike";
const DV1: &str = "lima-dv-1";
const DV2: &str = "lima-dv-2";
const EMPTY_KERNEL: &str = "[\n]\n\n";

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
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Gone);
}

/// R18: a deleting volume with a Primary on this node or on a peer keeps its copy.
async fn kept_while_primary(edit: impl FnOnce(&mut Value), expected_reason: &str) {
    let (e, _) = configured_env(DV1, kernel_json(DV1, edit), true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    // A Primary goes away without any event on the object: look again in 5 s.
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::KeptDrbd);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(5)));

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

    // The state cannot even be read before the decision: nothing is deleted,
    // the reason is in the status and the next pass is in 30 s, not 5.
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::Kept);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(30)));
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(
        entry.message.contains("cannot read the DRBD status") && entry.message.contains("no kernel module"),
        "{}",
        entry.message
    );
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

    // A status that cannot be read is not a DRBD state that changes by itself: 30 s.
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::Kept);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(30)));
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

/// A node whose resource is not configured: `drbdadm up` makes the kernel
/// show `after_up` and calls `on_up`; `drbdadm down` empties it again.
fn node_coming_up(after_up: String, on_up: impl Fn() + Send + Sync + 'static) -> FakeRunner {
    let status = Arc::new(Mutex::new(EMPTY_KERNEL.to_string()));
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["up", _]) => {
            *status.lock().unwrap() = after_up.clone();
            on_up();
            out(0, "", "")
        }
        ("drbdadm", ["down", _]) => {
            *status.lock().unwrap() = EMPTY_KERNEL.to_string();
            out(0, "", "")
        }
        ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
        _ => out(0, "", ""),
    })
}

/// Like `deleting`, for a node that holds a ready copy whose resource is not
/// configured; `drbdadm up` makes the kernel show `after_up`.
async fn deleting_unconfigured(after_up: String, entries: &[&str], third_kind: &str) -> Env {
    let e = env(DV1, node_coming_up(after_up, || {}));
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_with_third(third_kind, &[DV1, DV2]));
    for node in entries {
        e.volumes.set_entry(VOLUME, reported(node));
    }
    e.volumes.delete(VOLUME).await.unwrap();
    e
}

#[tokio::test]
async fn a_resource_that_is_not_configured_is_brought_up_and_then_kept_while_another_node_has_an_entry() {
    // After a reboot nothing configures the resource of a deleting object, so
    // the status shows no peers. The copy is not deleted on that: the
    // resource is brought up first (round 2), and the next pass, which sees
    // the peers, keeps the copy while one of them has not reported cleanup.
    let after_up = kernel_peers(
        "Secondary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting_unconfigured(after_up, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.paths.image(VOLUME).exists());
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::KeptDrbd);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(5)));
    assert_kept(&e, &[DV2, "not connected", "has not reported cleanup"]);
}

#[tokio::test]
async fn a_resource_that_is_not_configured_is_brought_up_and_then_deleted_when_nobody_else_has_an_entry() {
    let after_up = kernel_peers(
        "Secondary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting_unconfigured(after_up, &[], "Witness").await;
    // The first pass brings the resource up and deletes nothing.
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
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
        assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert_kept(&e, &["peer lima-dv-3 is Primary"]);
}

#[tokio::test]
async fn the_reason_names_every_node_that_blocks() {
    let kernel = kernel_peers(
        "Primary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2], "Witness").await;
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert_kept(&e, &["this node is Primary", DV2]);
}

#[tokio::test]
async fn a_peer_that_is_not_connected_and_has_reported_is_looked_at_again_in_five_seconds() {
    let kernel = kernel_peers(
        "Secondary",
        &[(DV2, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting(&kernel, &[DV2], "Witness").await;
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::KeptDrbd);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(5)));
    assert_kept(&e, &[DV2, "not connected"]);
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

    // The reason comes from the guard under the lock: also a DRBD state, also 5 s.
    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::KeptDrbd);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(5)));
    assert_kept(&e, &["peer lima-dv-2 is Primary"]);
}

#[tokio::test]
async fn the_three_replicas_of_a_deleting_volume_all_end_deleted() {
    // Three Replicas, all connected and Secondary, each seeing the others
    // `Connected`; each has reported. The volume is deleted: the first node
    // deletes and removes its entry, the second then sees the first neither
    // connected (it is down) nor reported, and so on.
    let volumes = Arc::new(FakeVolumes::default());
    let mut volume = spike_with_third("Replica", &[DV1, DV2, DV3]);
    volume.spec.nodes[2].kind = crate::crd::NodeKind::Replica;
    volumes.put(volume);
    for node in [DV1, DV2, DV3] {
        volumes.set_entry(VOLUME, reported(node));
    }
    volumes.delete(VOLUME).await.unwrap();

    let node_of = |node: &str, peers: &[(&str, &str, &str)]| {
        let kernel = Arc::new(Mutex::new(kernel_of(node, "Secondary", peers)));
        let e = env_sharing(node, node_with_status(kernel.clone(), true), true, volumes.clone());
        prepare_copy(&e.paths, true, true);
        (e, kernel)
    };
    let (a, _) = node_of(DV1, &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")]);
    let (b, kernel_b) = node_of(DV2, &[(DV1, "Connected", "Secondary"), (DV3, "Connected", "Secondary")]);
    let (c, kernel_c) = node_of(DV3, &[(DV1, "Connected", "Secondary"), (DV2, "Connected", "Secondary")]);

    assert_eq!(a.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!a.paths.image(VOLUME).exists());
    assert!(volumes.entry_of(VOLUME, DV1).is_none());

    // dv-1 is down now: the others see it `Connecting` and without an entry.
    *kernel_b.lock().unwrap() = kernel_of(
        DV2,
        "Secondary",
        &[(DV1, "Connecting", "Unknown"), (DV3, "Connected", "Secondary")],
    );
    *kernel_c.lock().unwrap() = kernel_of(
        DV3,
        "Secondary",
        &[(DV1, "Connecting", "Unknown"), (DV2, "Connected", "Secondary")],
    );
    assert_eq!(b.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!b.paths.image(VOLUME).exists());

    *kernel_c.lock().unwrap() = kernel_of(
        DV3,
        "Secondary",
        &[(DV1, "Connecting", "Unknown"), (DV2, "Connecting", "Unknown")],
    );
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Gone);
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

// ---- reconcile_one: a copy with data rejoins before it decides (round 2) ----

/// The status as `node` sees `peers` with the role `role`; node ids as in `spike`.
fn kernel_of(node: &str, role: &str, peers: &[(&str, &str, &str)]) -> String {
    let id_of = |name: &str| match name {
        DV1 => 0,
        DV2 => 1,
        _ => 2,
    };
    let mut all: Value = serde_json::from_str(&kernel_peers(role, peers)).unwrap();
    all[0]["node-id"] = json!(id_of(node));
    for connection in all[0]["connections"].as_array_mut().unwrap() {
        let peer = connection["name"].as_str().unwrap().to_string();
        connection["peer-node-id"] = json!(id_of(&peer));
    }
    serde_json::to_string(&all).unwrap()
}

fn secondary<'a>(a: &'a str, b: &'a str) -> [(&'a str, &'static str, &'static str); 2] {
    [(a, "Connected", "Secondary"), (b, "Connected", "Secondary")]
}

fn assert_nothing_destructive_ran(e: &Env) {
    for call in e.runner.calls() {
        for forbidden in ["create-md", "new-current-uuid", "primary", "drbdadm down", "losetup -d"] {
            assert!(!call.contains(forbidden), "{call:?} ran: {:?}", e.runner.calls());
        }
    }
}

#[tokio::test]
async fn a_ready_copy_whose_resource_is_not_configured_is_brought_up_before_it_decides() {
    let after_up = kernel_of(
        DV1,
        "Secondary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
    );
    let e = deleting_unconfigured(after_up, &[DV2], "Witness").await;

    // Pass 1: the resource is brought up, nothing is deleted.
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(commands(&e.runner, &["drbdadm up"]), ["drbdadm up spike"]);
    assert_nothing_destructive_ran(&e);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.ready_marker(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the status is written");
    assert_eq!(entry.role, "Secondary");
    assert!(!entry.connections.is_empty(), "the status shows the resource up");

    // Pass 2: peers Connected and Secondary: deleted.
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_deleted(&e);
    assert_eq!(commands(&e.runner, &["drbdadm up"]).len(), 1, "no second bring-up");
}

#[tokio::test]
async fn the_bring_up_of_a_deleting_object_never_skips_the_initial_sync() {
    // initialSync is Skip, this node is the leader, the synced marker is
    // missing and the peers show the fresh state the skip waits for: `apply`
    // of a live object would run `new-current-uuid` here.
    let e = env(DV1, node_coming_up(crate::tests::THREE_INCONSISTENT.to_string(), || {}));
    prepare_copy(&e.paths, true, false);
    e.volumes.put(spike("Skip"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(commands(&e.runner, &["drbdadm up"]).len(), 1);
    assert_nothing_destructive_ran(&e);
    assert!(e.volumes.object(VOLUME).unwrap().spec.initial_sync == InitialSync::Skip);
}

#[tokio::test]
async fn the_scenario_of_a_reboot_with_three_replicas_ends_with_all_three_deleted() {
    // dv-1 was rebooted (resource not configured); dv-2 and dv-3 are
    // configured, see dv-1 `Connecting` and wait for its entry to go. dv-1
    // waited for theirs. Bringing the resource up lets everyone see everyone.
    let volumes = Arc::new(FakeVolumes::default());
    let mut volume = spike_with_third("Replica", &[DV1, DV2, DV3]);
    volume.spec.nodes[2].kind = crate::crd::NodeKind::Replica;
    volumes.put(volume);
    for node in [DV1, DV2, DV3] {
        volumes.set_entry(VOLUME, reported(node));
    }
    volumes.delete(VOLUME).await.unwrap();

    let lonely = |node: &str| {
        let other = if node == DV2 { DV3 } else { DV2 };
        kernel_of(
            node,
            "Secondary",
            &[(DV1, "Connecting", "Unknown"), (other, "Connected", "Secondary")],
        )
    };
    let kernel2 = Arc::new(Mutex::new(lonely(DV2)));
    let kernel3 = Arc::new(Mutex::new(lonely(DV3)));
    let (hook2, hook3) = (kernel2.clone(), kernel3.clone());
    let a = env_sharing(
        DV1,
        node_coming_up(kernel_of(DV1, "Secondary", &secondary(DV2, DV3)), move || {
            // dv-1 is up: the others see it connect.
            *hook2.lock().unwrap() = kernel_of(DV2, "Secondary", &secondary(DV1, DV3));
            *hook3.lock().unwrap() = kernel_of(DV3, "Secondary", &secondary(DV1, DV2));
        }),
        true,
        volumes.clone(),
    );
    prepare_copy(&a.paths, true, true);
    let others = [(DV2, kernel2), (DV3, kernel3)].map(|(node, kernel)| {
        let e = env_sharing(node, node_with_status(kernel, true), true, volumes.clone());
        prepare_copy(&e.paths, true, true);
        e
    });
    let [b, c] = others;

    // Before dv-1 is up nobody can delete.
    assert_eq!(b.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert_eq!(c.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert!(b.paths.image(VOLUME).exists() && c.paths.image(VOLUME).exists());

    // dv-1 brings the resource up and keeps its copy.
    assert_eq!(a.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(a.paths.image(VOLUME).exists());
    assert_nothing_destructive_ran(&a);
    assert_eq!(commands(&a.runner, &["drbdadm up"]).len(), 1);

    // Now everyone sees everyone Connected and Secondary.
    assert_eq!(a.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!a.paths.image(VOLUME).exists());
    assert_eq!(b.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!b.paths.image(VOLUME).exists());
    assert_eq!(c.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!c.paths.image(VOLUME).exists());
    assert!(volumes.object(VOLUME).is_none());
}

#[tokio::test]
async fn a_ready_copy_whose_spec_cannot_be_converted_is_kept() {
    let e = env(DV1, node_coming_up(kernel_json(DV1, |_| {}), || {}));
    prepare_copy(&e.paths, true, true);
    let mut volume = spike("Done");
    volume.spec.nodes[2].address = "bogus".to_string();
    e.volumes.put(volume);
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(commands(&e.runner, &["drbdadm"]).is_empty(), "{:?}", e.runner.calls());
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains("bogus"), "{}", entry.message);
    assert!(entry.message.contains("kept"), "{}", entry.message);
}

#[tokio::test]
async fn a_ready_copy_whose_bring_up_fails_is_kept_with_the_error() {
    let e = env(
        DV1,
        FakeRunner::new(|program, args| match (program, args) {
            ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
            ("drbdadm", ["up", _]) => out(10, "", "cannot load the drbd module"),
            ("drbdsetup", _) => out(0, EMPTY_KERNEL, ""),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_nothing_destructive_ran(&e);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the error is in the status");
    assert!(
        entry.message.contains("cannot load the drbd module"),
        "{}",
        entry.message
    );
}

#[tokio::test]
async fn a_busy_volume_is_not_brought_up_and_is_retried() {
    let e = env(DV1, node_coming_up(kernel_json(DV1, |_| {}), || {}));
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();
    let held = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(e.paths.lock_file(VOLUME))
        .unwrap();
    held.try_lock().unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(commands(&e.runner, &["drbdadm up"]).is_empty());
    assert!(e.volumes.status_writes().is_empty());
    assert!(e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn a_node_with_only_traces_removes_without_the_peer_checks() {
    // No backing file: nothing to lose. Another node has an entry and the
    // resource is not configured, which would keep a copy with data.
    let e = env(DV1, node_coming_up(kernel_json(DV1, |_| {}), || {}));
    prepare_copy(&e.paths, true, true);
    fs::remove_file(e.paths.image(VOLUME)).unwrap();
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    fs::remove_file(e.paths.synced_marker(VOLUME)).unwrap();
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.set_entry(VOLUME, reported(DV2));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(
        commands(&e.runner, &["drbdadm up"]).is_empty(),
        "{:?}",
        e.runner.calls()
    );
    assert!(!e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
}

/// A deleting `spike` on dv-1 whose backing file has no ready marker.
async fn deleting_pending(kernel: &str, entries: &[&str]) -> Env {
    let e = env(DV1, node_with_status(Arc::new(Mutex::new(kernel.to_string())), true));
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    for node in entries {
        e.volumes.set_entry(VOLUME, reported(node));
    }
    e.volumes.delete(VOLUME).await.unwrap();
    e
}

#[tokio::test]
async fn a_copy_without_data_is_kept_while_this_node_is_primary() {
    for pending in [false, true] {
        let kernel = kernel_peers(
            "Primary",
            &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
        );
        let e = if pending {
            deleting_pending(&kernel, &[]).await
        } else {
            let e = env(DV1, node_with_status(Arc::new(Mutex::new(kernel)), true));
            prepare_copy(&e.paths, true, false);
            fs::remove_file(e.paths.image(VOLUME)).unwrap();
            fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
            e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
            e.volumes.delete(VOLUME).await.unwrap();
            e
        };
        assert_eq!(
            e.agent.reconcile_one(VOLUME).await.unwrap(),
            Outcome::KeptDrbd,
            "pending={pending}"
        );
        assert!(commands(&e.runner, &["drbdadm down"]).is_empty());
        assert!(e.paths.res_file(VOLUME).exists());
        assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
        let entry = e.volumes.entry_of(VOLUME, DV1).unwrap();
        assert!(entry.message.contains("this node is Primary"), "{}", entry.message);
    }
}

#[tokio::test]
async fn a_copy_without_data_is_kept_when_the_status_cannot_be_read() {
    let e = env(
        DV1,
        FakeRunner::new(|program, _| match program {
            "drbdsetup" => out(1, "", "no kernel module"),
            _ => out(0, "", ""),
        }),
    );
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
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
async fn a_ready_file_that_appears_before_the_lock_stops_the_removal() {
    // The pass sees no backing file and no configured resource; before the
    // lock is taken the file and its ready marker appear (the CLI, a CSI
    // call). The guard under the lock must stop the removal.
    let paths_cell: Arc<Mutex<Option<Paths>>> = Arc::new(Mutex::new(None));
    let cell = paths_cell.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let e = env(
        DV1,
        FakeRunner::new(move |program, args| match (program, args) {
            // `read_status` runs before the volume lock; simulate the file and
            // ready marker appearing before the guard checks under the lock.
            ("drbdsetup", _) => {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    let paths = cell.lock().unwrap().clone().unwrap();
                    fs::write(paths.image(VOLUME), "data").unwrap();
                    fs::write(paths.ready_marker(VOLUME), "").unwrap();
                }
                out(0, EMPTY_KERNEL, "")
            }
            _ => out(0, "", ""),
        }),
    );
    *paths_cell.lock().unwrap() = Some(e.paths.clone());
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.image(VOLUME)).unwrap();
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.ready_marker(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
}

#[tokio::test]
async fn a_node_that_becomes_primary_before_the_lock_keeps_a_copy_without_data() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let calm = kernel_peers(
        "Secondary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
    );
    let busy = kernel_peers(
        "Primary",
        &[(DV2, "Connected", "Secondary"), (DV3, "Connected", "Secondary")],
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
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    e.volumes.put(spike("Done"));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert!(commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty());
    assert!(e.paths.image(VOLUME).exists());
    let entry = e.volumes.entry_of(VOLUME, DV1).unwrap();
    assert!(entry.message.contains("this node is Primary"), "{}", entry.message);
}

// ---- reconcile_one: "no data" is proven, not assumed (round 3) ----

/// `dump-md` right after `create-md` (experiment P10 (1)).
const FRESH_DUMP: &str = include_str!("../../drbd/tests/fixtures/p10-1-just-created.txt");
/// `dump-md` of metadata with a file system on it (experiment P10 (5)).
const USED_DUMP: &str = include_str!("../../drbd/tests/fixtures/p10-5-filesystem-and-file.txt");

/// The parts of a node the "no data" proof reads: the kernel, the loop
/// devices and `drbdadm dump-md`. A test changes them, also from a hook that
/// runs while the pass is between the decision and the lock.
type Hook = Arc<dyn Fn(&str, usize) + Send + Sync>;

#[derive(Clone)]
struct Script {
    kernel: Arc<Mutex<String>>,
    /// The answer of `losetup --list`.
    loops: Arc<Mutex<Output>>,
    /// The answer of `losetup -j <image>`: the devices that hold the file's inode.
    same_inode: Arc<Mutex<Output>>,
    /// The answers of `drbdadm dump-md`, in order; the last one repeats.
    dump_md: Arc<Mutex<Vec<Output>>>,
    dump_calls: Arc<AtomicUsize>,
    loop_calls: Arc<AtomicUsize>,
    status_calls: Arc<AtomicUsize>,
    /// Runs after the n-th (from 0) call of `"dump-md"`, `"losetup"` or `"drbdsetup"` has been answered.
    after: Arc<Mutex<Option<Hook>>>,
}

impl Script {
    fn new(kernel: &str, dump_md: Output) -> Self {
        Self {
            kernel: Arc::new(Mutex::new(kernel.to_string())),
            loops: Arc::new(Mutex::new(out(0, "", ""))),
            same_inode: Arc::new(Mutex::new(out(0, "", ""))),
            dump_md: Arc::new(Mutex::new(vec![dump_md])),
            dump_calls: Arc::new(AtomicUsize::new(0)),
            loop_calls: Arc::new(AtomicUsize::new(0)),
            status_calls: Arc::new(AtomicUsize::new(0)),
            after: Arc::new(Mutex::new(None)),
        }
    }

    fn no_metadata() -> Output {
        out(255, "", crate::tests::P7_NO_METADATA_STDERR)
    }

    fn hold_loop(&self, image: &Path) {
        *self.loops.lock().unwrap() = out(0, &format!("/dev/loop7 {} (deleted)\n", image.display()), "");
    }

    /// `/dev/loop7` holds the file under its live path, as the device link of `prepare_copy` says.
    fn attach_loop(&self, image: &Path) {
        *self.loops.lock().unwrap() = out(0, &format!("/dev/loop7 {}\n", image.display()), "");
    }

    /// `losetup --list` shows `/dev/loop7` as `/<name>.img`, the form of a loop device
    /// attached by a pod that is gone (`live`: without `(deleted)`); `inode`: `losetup -j`
    /// confirms the device holds the file.
    fn detached_form(&self, live: bool, inode: bool) {
        let suffix = if live { "" } else { " (deleted)" };
        *self.loops.lock().unwrap() = out(0, &format!("/dev/loop7 /{VOLUME}.img{suffix}\n"), "");
        let confirmed = if inode {
            format!("/dev/loop7: [65025]:262217 (/{VOLUME}.img)\n")
        } else {
            String::new()
        };
        *self.same_inode.lock().unwrap() = out(0, &confirmed, "");
    }

    /// The listing is `text` as it is.
    fn list(&self, text: &str) {
        *self.loops.lock().unwrap() = out(0, text, "");
    }

    fn after(&self, hook: impl Fn(&str, usize) + Send + Sync + 'static) {
        *self.after.lock().unwrap() = Some(Arc::new(hook));
    }

    fn runner(&self) -> FakeRunner {
        let script = self.clone();
        FakeRunner::new(move |program, args| {
            let announce = |command: &str, n: usize| {
                let hook = script.after.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook(command, n);
                }
            };
            match (program, args) {
                ("losetup", ["--list", ..]) => {
                    let n = script.loop_calls.fetch_add(1, Ordering::SeqCst);
                    let reply = script.loops.lock().unwrap().clone();
                    announce("losetup", n);
                    reply
                }
                ("losetup", ["-j", _]) => script.same_inode.lock().unwrap().clone(),
                ("losetup", ["--find", ..]) => out(0, "/dev/loop3\n", ""),
                ("losetup", ["-d", _]) => out(0, "", ""),
                ("drbdadm", ["dump-md", _]) => {
                    let n = script.dump_calls.fetch_add(1, Ordering::SeqCst);
                    let reply = {
                        let answers = script.dump_md.lock().unwrap();
                        answers[n.min(answers.len() - 1)].clone()
                    };
                    announce("dump-md", n);
                    reply
                }
                ("drbdadm", ["down", _]) => {
                    *script.kernel.lock().unwrap() = EMPTY_KERNEL.to_string();
                    out(0, "", "")
                }
                ("drbdsetup", _) => {
                    let n = script.status_calls.fetch_add(1, Ordering::SeqCst);
                    let reply = out(0, &script.kernel.lock().unwrap(), "");
                    announce("drbdsetup", n);
                    reply
                }
                _ => out(0, "", ""),
            }
        })
    }
}

#[derive(Clone, Copy)]
enum Backing {
    Absent,
    /// A backing file without the ready marker; whether the synced marker exists.
    Pending {
        synced: bool,
    },
}

/// A deleting `spike` on dv-1, with the backing file as `backing` says and
/// the other nodes in `entries` having reported.
async fn deleting_with(backing: Backing, script: &Script, entries: &[&str]) -> Env {
    let e = env(DV1, script.runner());
    prepare_copy(&e.paths, true, matches!(backing, Backing::Pending { synced: true }));
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    if matches!(backing, Backing::Absent) {
        fs::remove_file(e.paths.image(VOLUME)).unwrap();
    } else {
        // The device link of `prepare_copy` points at /dev/loop7, which holds this file.
        script.attach_loop(&e.paths.image(VOLUME));
    }
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    for node in entries {
        e.volumes.set_entry(VOLUME, reported(node));
    }
    e.volumes.delete(VOLUME).await.unwrap();
    e
}

fn assert_copy_kept(e: &Env, reason: &str) {
    assert!(
        commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty(),
        "{:?}",
        e.runner.calls()
    );
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains(reason), "{reason:?} not in {:?}", entry.message);
}

fn assert_copy_removed(e: &Env) {
    assert!(!e.paths.image(VOLUME).exists());
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(!e.paths.synced_marker(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
}

/// dv-3 is connected and Secondary; dv-2 is connected with the role `dv2_role`.
fn calm_kernel(dv2_role: &str) -> String {
    kernel_peers(
        "Secondary",
        &[(DV2, "Connected", dv2_role), (DV3, "Connected", "Secondary")],
    )
}

async fn pending_with_metadata_is_removed_without_the_peer_checks(dump_md: Output) {
    // The resource is not configured and dv-2 has an entry and is not
    // connected: the peer checks would keep a copy with data.
    let script = Script::new(EMPTY_KERNEL, dump_md);
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[DV2]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert!(!commands(&e.runner, &["drbdadm dump-md"]).is_empty(), "the proof ran");
}

#[tokio::test]
async fn a_pending_file_without_the_synced_marker_and_without_metadata_is_removed_without_the_peer_checks() {
    pending_with_metadata_is_removed_without_the_peer_checks(Script::no_metadata()).await;
}

#[tokio::test]
async fn a_pending_file_without_the_synced_marker_and_with_fresh_metadata_is_removed_without_the_peer_checks() {
    pending_with_metadata_is_removed_without_the_peer_checks(out(0, FRESH_DUMP, "")).await;
}

#[tokio::test]
async fn a_pending_file_without_loop_device_after_reboot_is_checked_read_only_and_removed() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = env(DV1, script.runner());
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    // Simulate reboot: device link is absent, no loop device attached.
    let _ = fs::remove_file(e.paths.device_link(VOLUME));
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.set_entry(VOLUME, reported(DV2));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert!(
        e.runner
            .calls()
            .iter()
            .any(|c| c.contains("losetup --find --show --read-only")),
        "read-only loop attachment ran"
    );
    assert!(
        e.runner.calls().iter().any(|c| c.starts_with("losetup -d")),
        "temporary loop device was detached"
    );
}

#[tokio::test]
async fn a_pending_file_without_loop_device_after_reboot_kept_when_metadata_used() {
    let script = Script::new(EMPTY_KERNEL, out(0, USED_DUMP, ""));
    let e = env(DV1, script.runner());
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    let _ = fs::remove_file(e.paths.device_link(VOLUME));
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.set_entry(VOLUME, reported(DV2));
    e.volumes.delete(VOLUME).await.unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.res_file(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains("already holds DRBD metadata"));
    assert!(
        e.runner.calls().iter().any(|c| c.starts_with("losetup -d")),
        "temporary read-only loop device was detached"
    );
}

#[tokio::test]
async fn a_pending_file_with_the_synced_marker_is_kept() {
    // The synced marker says the file held data; the metadata check would say "absent".
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: true }, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "synced marker");
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.synced_marker(VOLUME).exists());
}

#[tokio::test]
async fn a_pending_file_with_used_metadata_is_kept() {
    let script = Script::new(EMPTY_KERNEL, out(0, USED_DUMP, ""));
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "already holds DRBD metadata");
    assert!(e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn a_pending_file_whose_metadata_cannot_be_checked_is_kept() {
    // `dump-md` needs the resource file, the device link and an attached loop
    // device; when one is missing it fails, which proves nothing.
    let script = Script::new(EMPTY_KERNEL, out(10, "", "'spike' not defined in your config"));
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "not defined in your config");
    assert!(e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn a_pending_file_under_a_configured_resource_with_a_primary_peer_is_kept() {
    // DRBD holds the disk: whatever the file marker says, the peer checks decide.
    let script = Script::new(&calm_kernel("Primary"), Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert_copy_kept(&e, "peer lima-dv-2 is Primary");
    assert!(e.paths.image(VOLUME).exists());
    assert!(commands(&e.runner, &["drbdadm dump-md"]).is_empty());
}

#[tokio::test]
async fn a_pending_file_under_a_configured_resource_is_deleted_by_the_peer_checks() {
    let script = Script::new(&calm_kernel("Secondary"), Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[DV2]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert_eq!(commands(&e.runner, &["drbdadm down"]), ["drbdadm down spike"]);
    assert!(commands(&e.runner, &["drbdadm dump-md"]).is_empty());
}

#[tokio::test]
async fn a_missing_file_under_a_configured_resource_with_a_primary_peer_is_kept() {
    // The loop device of the deleted file still serves the disk.
    let script = Script::new(&calm_kernel("Primary"), Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert_copy_kept(&e, "peer lima-dv-2 is Primary");
    assert!(commands(&e.runner, &["losetup"]).is_empty());
}

#[tokio::test]
async fn a_missing_file_under_a_configured_resource_is_deleted_by_the_peer_checks() {
    let script = Script::new(&calm_kernel("Secondary"), Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[DV2]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert_eq!(commands(&e.runner, &["drbdadm down"]), ["drbdadm down spike"]);
}

#[tokio::test]
async fn a_missing_file_that_a_loop_device_still_holds_is_kept() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[]).await;
    script.hold_loop(&e.paths.image(VOLUME));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "the backing file is gone but a loop device still holds it");
    assert!(fs::symlink_metadata(e.paths.device_link(VOLUME)).is_ok());
}

#[tokio::test]
async fn a_missing_file_held_by_a_loop_device_in_the_detached_mount_form_is_kept() {
    // After the agent pod that attached the loop is replaced, the kernel lists
    // `/<name>.img (deleted)` (Task 13, stand).
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[]).await;
    script.detached_form(false, false);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(
        &e,
        "the backing file is gone but a loop device still holds it (/dev/loop7)",
    );
}

#[tokio::test]
async fn a_missing_file_that_no_loop_device_holds_is_removed_without_the_peer_checks() {
    // Another file's loop device is not this file's. dv-2 has an entry and is
    // not connected: the peer checks would keep the copy.
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    *script.loops.lock().unwrap() = out(0, "/dev/loop3 /var/lib/other/volume.img\n", "");
    let e = deleting_with(Backing::Absent, &script, &[DV2]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert!(!commands(&e.runner, &["losetup --list"]).is_empty(), "the proof ran");
}

#[tokio::test]
async fn a_missing_file_whose_loop_devices_cannot_be_listed_is_kept() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    *script.loops.lock().unwrap() = out(1, "", "losetup: cannot open /dev/loop-control");
    let e = deleting_with(Backing::Absent, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "cannot open /dev/loop-control");
}

// The guard re-establishes every fact of a "no data" row under the lock.

#[tokio::test]
async fn a_synced_marker_that_appears_before_the_lock_stops_the_removal() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;
    let marker = e.paths.synced_marker(VOLUME);
    // `drbdsetup` n == 0 is `read_status` before the volume lock is taken.
    script.after(move |command, n| {
        if command == "drbdsetup" && n == 0 {
            fs::write(&marker, "").unwrap();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "synced marker");
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.paths.synced_marker(VOLUME).exists());
}

#[tokio::test]
async fn a_resource_that_is_configured_before_the_lock_stops_the_removal() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;
    let (kernel, up) = (script.kernel.clone(), calm_kernel("Secondary"));
    script.after(move |command, n| {
        if command == "drbdsetup" && n == 0 {
            *kernel.lock().unwrap() = up.clone();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "configured");
    assert!(e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn metadata_that_is_used_by_the_time_of_the_lock_stops_the_removal() {
    // `prove_no_data` runs only once, under the volume lock.
    let script = Script::new(EMPTY_KERNEL, out(0, USED_DUMP, ""));
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "already holds DRBD metadata");
    assert!(e.paths.image(VOLUME).exists());
    assert_eq!(script.dump_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_loop_device_that_holds_the_file_by_the_time_of_the_lock_stops_the_removal() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[]).await;
    let (held, image) = (script.clone(), e.paths.image(VOLUME));
    script.after(move |command, n| {
        if command == "drbdsetup" && n == 0 {
            held.hold_loop(&image);
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "a loop device still holds it");
    assert_eq!(script.loop_calls.load(Ordering::SeqCst), 1);
}

// ---- round 4: `dump-md` reads this volume's file ----

/// A deleting `spike` with a file without the ready marker, the resource not
/// configured and a `Fresh` `dump-md`: it is removed when the device link is on
/// the loop device of the file.
async fn fresh_pending_copy() -> (Script, Env) {
    let script = Script::new(EMPTY_KERNEL, out(0, FRESH_DUMP, ""));
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[DV2]).await;
    (script, e)
}

fn assert_no_dump_md(e: &Env, script: &Script) {
    assert!(commands(&e.runner, &["drbdadm dump-md"]).is_empty());
    assert_eq!(script.dump_calls.load(Ordering::SeqCst), 0);
    assert!(e.paths.image(VOLUME).exists());
}

#[tokio::test]
async fn a_pending_file_whose_device_link_is_on_its_loop_device_is_removed_when_the_metadata_is_fresh() {
    let (script, e) = fresh_pending_copy().await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert!(script.dump_calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn a_pending_file_whose_device_link_is_on_a_loop_in_the_detached_mount_form_is_removed_once_losetup_j_confirms_it()
 {
    let (script, e) = fresh_pending_copy().await;
    script.detached_form(true, true);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
}

#[tokio::test]
async fn a_pending_file_whose_loop_in_the_detached_mount_form_is_not_confirmed_by_inode_is_kept() {
    let (script, e) = fresh_pending_copy().await;
    script.detached_form(true, false);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "no loop device holds");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_pending_file_whose_loop_is_matched_by_name_only_while_the_image_goes_missing_is_kept() {
    // The listing shows the loop device as `/<name>.img`; the image is removed after the
    // listing was read, so `loop_holders` cannot confirm the name by inode and keeps the
    // holder unconfirmed. A name-only match is no proof that `dump-md` reads this file.
    let (script, e) = fresh_pending_copy().await;
    script.detached_form(true, false);
    let image = e.paths.image(VOLUME);
    script.after(move |command, n| {
        if command == "losetup" && n == 0 {
            fs::remove_file(&image).unwrap();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(entry.message.contains("by its name"), "{}", entry.message);
    assert!(commands(&e.runner, &["drbdadm dump-md"]).is_empty());
    assert_eq!(script.dump_calls.load(Ordering::SeqCst), 0);
    assert!(commands(&e.runner, &["drbdadm down", "losetup -d"]).is_empty());
}

#[tokio::test]
async fn a_pending_file_without_a_device_link_is_kept_and_dump_md_does_not_run() {
    let (script, e) = fresh_pending_copy().await;
    fs::remove_file(e.paths.device_link(VOLUME)).unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "device link");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_pending_file_whose_device_link_is_not_a_link_is_kept() {
    let (script, e) = fresh_pending_copy().await;
    fs::remove_file(e.paths.device_link(VOLUME)).unwrap();
    fs::write(e.paths.device_link(VOLUME), "").unwrap();

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "device link");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_pending_file_whose_device_link_points_at_the_loop_of_another_file_is_kept() {
    // /dev/loop7 holds somebody else's file, and the image has a loop of its own.
    let (script, e) = fresh_pending_copy().await;
    script.list(&format!(
        "/dev/loop7 /var/lib/other/volume.img\n/dev/loop8 {}\n",
        e.paths.image(VOLUME).display()
    ));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "points at /dev/loop7 but the backing file is held by /dev/loop8");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_pending_file_that_no_loop_device_holds_is_removed_when_fresh() {
    let (script, e) = fresh_pending_copy().await;
    script.list("");

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert!(
        e.runner
            .calls()
            .iter()
            .any(|c| c.contains("losetup --find --show --read-only")),
        "read-only loop attachment ran"
    );
    assert!(
        e.runner.calls().iter().any(|c| c.starts_with("losetup -d")),
        "temporary loop device was detached"
    );
}

#[tokio::test]
async fn a_pending_file_that_no_loop_device_holds_is_kept_when_metadata_used() {
    let script = Script::new(EMPTY_KERNEL, out(0, USED_DUMP, ""));
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[DV2]).await;
    script.list("");

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(e.paths.image(VOLUME).exists());
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("the reason is in the status");
    assert!(
        entry.message.contains("already holds DRBD metadata"),
        "{}",
        entry.message
    );
    assert!(
        e.runner
            .calls()
            .iter()
            .any(|c| c.contains("losetup --find --show --read-only")),
        "read-only loop attachment ran"
    );
    assert!(
        e.runner.calls().iter().any(|c| c.starts_with("losetup -d")),
        "temporary loop device was detached"
    );
}

#[tokio::test]
async fn a_pending_file_whose_device_link_is_on_a_loop_that_holds_a_deleted_file_is_kept() {
    let (script, e) = fresh_pending_copy().await;
    script.hold_loop(&e.paths.image(VOLUME));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "deleted file");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_pending_file_whose_loop_devices_cannot_be_listed_is_kept() {
    let (script, e) = fresh_pending_copy().await;
    *script.loops.lock().unwrap() = out(1, "", "losetup: cannot open /dev/loop-control");

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "cannot open /dev/loop-control");
    assert_no_dump_md(&e, &script);
}

#[tokio::test]
async fn a_device_link_that_goes_away_before_the_lock_stops_the_removal() {
    // The guard checks the link under the lock before `dump-md`.
    let (script, e) = fresh_pending_copy().await;
    let link = e.paths.device_link(VOLUME);
    script.after(move |command, n| {
        if command == "drbdsetup" && n == 0 {
            fs::remove_file(&link).unwrap();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "device link");
    assert_eq!(script.dump_calls.load(Ordering::SeqCst), 0);
}

// ---- round 4: an unparsable listing is "not proven" ----

#[tokio::test]
async fn a_listing_that_cannot_be_parsed_keeps_a_missing_file_and_a_pending_one() {
    for text in [
        "/dev/loop3\n",
        "/dev/loop3 /var/lib/rancher/ambar/volumes/a-very-long-na*\n",
    ] {
        let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
        let e = deleting_with(Backing::Absent, &script, &[]).await;
        script.list(text);
        assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept, "{text:?}");
        assert_copy_kept(&e, "unexpected output");

        let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
        let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;
        script.list(text);
        assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept, "{text:?}");
        assert_copy_kept(&e, "unexpected output");
        assert_no_dump_md(&e, &script);
    }
}

// ---- round 4: the resource must still be absent when the copy is removed ----

#[tokio::test]
async fn a_resource_that_appears_after_the_guard_stops_the_removal_of_a_missing_file() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[]).await;
    let (kernel, up) = (script.kernel.clone(), calm_kernel("Secondary"));
    // The listing is the proof inside the guard, the last read before
    // `remove_unconfigured` looks at the status again.
    script.after(move |command, n| {
        if command == "losetup" && n == 0 {
            *kernel.lock().unwrap() = up.clone();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "configured in the kernel");
    assert_eq!(script.loop_calls.load(Ordering::SeqCst), 1);
    assert!(e.paths.res_file(VOLUME).exists());
    assert!(fs::symlink_metadata(e.paths.device_link(VOLUME)).is_ok());
}

#[tokio::test]
async fn a_resource_that_appears_after_the_guard_stops_the_removal_of_a_pending_file() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = deleting_with(Backing::Pending { synced: false }, &script, &[]).await;
    let (kernel, up) = (script.kernel.clone(), calm_kernel("Secondary"));
    script.after(move |command, n| {
        if command == "dump-md" && n == 0 {
            *kernel.lock().unwrap() = up.clone();
        }
    });

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_copy_kept(&e, "configured in the kernel");
    assert_eq!(script.dump_calls.load(Ordering::SeqCst), 1);
    assert!(e.paths.image(VOLUME).exists());
}

// ---- round 4: the peer checks detach every loop device of a missing file ----

#[tokio::test]
async fn a_missing_file_held_by_a_loop_device_is_detached_when_the_peer_checks_pass() {
    // The resource is configured, the file was deleted under it, the loop
    // device holds the inode. The peer checks pass: `down`, detach, clean up.
    let script = Script::new(&calm_kernel("Secondary"), Script::no_metadata());
    let e = deleting_with(Backing::Absent, &script, &[DV2]).await;
    script.hold_loop(&e.paths.image(VOLUME));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_copy_removed(&e);
    assert_eq!(commands(&e.runner, &["drbdadm down"]), ["drbdadm down spike"]);
    assert_eq!(commands(&e.runner, &["losetup -d"]), ["losetup -d /dev/loop7"]);
    assert!(fs::symlink_metadata(e.paths.device_link(VOLUME)).is_err());
    assert!(!e.paths.ready_marker(VOLUME).exists());
}

// ---- the removal of the status entry is verified (round 3) ----

/// dv-3 holds nothing, the object is deleting and still shows the entries of
/// dv-2 and dv-3: the removal of dv-3's entry is all that is left to do.
async fn leftover_entry_of_a_witness() -> Env {
    let e = env(DV3, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.set_entry(VOLUME, reported(DV2));
    e.volumes.set_entry(VOLUME, reported(DV3));
    e.volumes.delete(VOLUME).await.unwrap();
    e
}

fn patches_of_the_status(volumes: &FakeVolumes) -> Vec<String> {
    volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch-status"))
        .collect()
}

#[tokio::test]
async fn an_entry_that_survives_the_empty_apply_is_removed_with_a_patch() {
    // Another field manager co-owns the entry: the empty apply leaves it.
    let e = leftover_entry_of_a_witness().await;
    e.volumes.co_owned.store(true, Ordering::SeqCst);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
    assert!(
        e.volumes.entry_of(VOLUME, DV2).is_some(),
        "the entry of another node stays"
    );
    assert_eq!(patches_of_the_status(&e.volumes), [format!("patch-status {VOLUME}")]);
}

#[tokio::test]
async fn an_entry_that_the_empty_apply_removes_needs_no_patch() {
    let e = leftover_entry_of_a_witness().await;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
    assert!(patches_of_the_status(&e.volumes).is_empty());
}

#[tokio::test]
async fn an_entry_that_cannot_be_removed_keeps_the_pass_at_thirty_seconds() {
    let e = leftover_entry_of_a_witness().await;
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    e.volumes.fail_status_patch.store(true, Ordering::SeqCst);

    let outcome = e.agent.reconcile_one(VOLUME).await;
    assert_eq!(outcome.as_ref().unwrap(), &Outcome::Kept);
    assert_eq!(settle(VOLUME, &outcome), Some(Duration::from_secs(30)));
    assert!(e.volumes.entry_of(VOLUME, DV3).is_some());
    // One empty apply and one patch in a pass, not a loop.
    assert_eq!(
        patches_of_the_status(&e.volumes),
        [format!("patch-status {VOLUME} failed")]
    );
    assert_eq!(e.volumes.status_writes().len(), 1);
}

#[tokio::test]
async fn a_conflicting_status_patch_is_tried_again() {
    let e = leftover_entry_of_a_witness().await;
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    *e.volumes.conflicts.lock().unwrap() = 2;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
    assert_eq!(
        patches_of_the_status(&e.volumes),
        [
            format!("patch-status {VOLUME} conflict"),
            format!("patch-status {VOLUME} conflict"),
            format!("patch-status {VOLUME}"),
        ]
    );
}

#[tokio::test]
async fn a_status_patch_that_never_applies_gives_up_after_five_attempts() {
    let e = leftover_entry_of_a_witness().await;
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    *e.volumes.conflicts.lock().unwrap() = 99;

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_eq!(patches_of_the_status(&e.volumes).len(), 5);
    assert!(e.volumes.entry_of(VOLUME, DV3).is_some());
}

#[tokio::test]
async fn the_status_patch_removes_the_entry_by_its_name_not_by_the_index_that_was_read() {
    // Between the read and the patch the list is reordered: the index that was
    // read now holds the entry of the other node, which must not be removed.
    let volumes = FakeVolumes::default();
    volumes.put(spike("Done"));
    volumes.set_entry(VOLUME, reported(DV2));
    volumes.set_entry(VOLUME, reported(DV3));
    let uid = uid_of_object(&volumes, VOLUME);
    let reorder = Changing {
        inner: &volumes,
        change: Box::new(|raw| raw["status"]["nodes"].as_array_mut().unwrap().reverse()),
        once: AtomicBool::new(false),
    };

    let removed = patch_status_retrying(&reorder, VOLUME, &uid, &|o| remove_node_entry_ops(o, DV3)).await;
    assert!(removed.unwrap());
    assert!(volumes.entry_of(VOLUME, DV3).is_none());
    assert!(volumes.entry_of(VOLUME, DV2).is_some());
    assert!(volumes.calls().contains(&format!("patch-status {VOLUME} rejected")));
}

#[tokio::test]
async fn the_status_patch_of_an_object_with_another_uid_changes_nothing() {
    let volumes = FakeVolumes::default();
    volumes.put(spike("Done"));
    volumes.set_entry(VOLUME, reported(DV3));

    assert!(!volumes.remove_node_entry(VOLUME, "another-uid", DV3).await.unwrap());
    assert!(volumes.entry_of(VOLUME, DV3).is_some());
    assert!(patches_of_the_status(&volumes).is_empty());
}

#[tokio::test]
async fn a_deleted_copy_whose_entry_cannot_be_removed_keeps_its_finalizer_until_it_can() {
    let kernel = Arc::new(Mutex::new(kernel_peers("Secondary", &secondary(DV2, DV3))));
    let e = env(DV1, node_with_status(kernel, true));
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.set_entry(VOLUME, reported(DV1));
    e.volumes.delete(VOLUME).await.unwrap();
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    e.volumes.fail_status_patch.store(true, Ordering::SeqCst);

    // The copy is removed, the entry stays: the finalizer stays with it.
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(!e.paths.image(VOLUME).exists());
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);
    assert!(e.volumes.entry_of(VOLUME, DV1).is_some());

    // Nothing is left on the node but the entry: still Kept, finalizer kept.
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert_eq!(e.volumes.finalizers(VOLUME).len(), 2);

    // The API works again: the patch removes the entry, then the finalizer goes.
    e.volumes.fail_status_patch.store(false, Ordering::SeqCst);
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
    assert_eq!(e.volumes.finalizers(VOLUME), [replica_finalizer(DV2)]);
}

#[tokio::test]
async fn a_deleted_copy_whose_entry_is_co_owned_ends_deleted() {
    let kernel = Arc::new(Mutex::new(kernel_peers("Secondary", &secondary(DV2, DV3))));
    let e = env(DV1, node_with_status(kernel, true));
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_with_third("Witness", &[DV1, DV2]));
    e.volumes.set_entry(VOLUME, reported(DV1));
    e.volumes.delete(VOLUME).await.unwrap();
    e.volumes.co_owned.store(true, Ordering::SeqCst);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_deleted(&e);
}

#[tokio::test]
async fn cleaning_leftovers_whose_entry_cannot_be_removed_is_kept() {
    let e = env(DV3, fake_node());
    e.volumes.put(spike("Done"));
    e.volumes.set_entry(VOLUME, reported(DV3));
    e.volumes.delete(VOLUME).await.unwrap();
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    e.volumes.fail_status_patch.store(true, Ordering::SeqCst);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.volumes.entry_of(VOLUME, DV3).is_some());
}

#[tokio::test]
async fn a_stray_finalizer_stays_while_the_entry_of_the_node_cannot_be_removed() {
    // dv-3 is not a Replica but carries the finalizer, holds nothing and has an entry.
    let e = env(DV3, fake_node());
    let mut volume = spike("Done");
    volume.metadata.finalizers = Some(vec![replica_finalizer(DV1), replica_finalizer(DV3)]);
    e.volumes.put(volume);
    e.volumes.set_entry(VOLUME, reported(DV3));
    e.volumes.delete(VOLUME).await.unwrap();
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    e.volumes.fail_status_patch.store(true, Ordering::SeqCst);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV3)));
}

// ---- small items (round 3) ----

#[tokio::test]
async fn a_backing_file_state_that_cannot_be_read_keeps_the_copy() {
    // The ready marker is a symbolic link to itself: `stat` fails with ELOOP.
    // `local` does not look at the marker, so the pass reaches `delete_replica`.
    // With the resource configured and the peers fine, a file state read as
    // "absent" would let the peer checks delete the copy.
    for kernel in [EMPTY_KERNEL.to_string(), calm_kernel("Secondary")] {
        let e = env(DV1, node_with_status(Arc::new(Mutex::new(kernel.clone())), true));
        prepare_copy(&e.paths, true, true);
        fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
        std::os::unix::fs::symlink(e.paths.ready_marker(VOLUME), e.paths.ready_marker(VOLUME)).unwrap();
        e.volumes.put(spike("Done"));
        e.volumes.delete(VOLUME).await.unwrap();

        let outcome = e.agent.reconcile_one(VOLUME).await;
        assert_eq!(outcome.as_ref().unwrap(), &Outcome::Kept, "{kernel}");
        assert_copy_kept(&e, "cannot inspect the backing file");
        assert!(e.paths.image(VOLUME).exists());
        assert!(commands(&e.runner, &["drbdadm", "losetup"]).is_empty());
    }
}

#[tokio::test]
async fn a_name_with_no_object_and_nothing_local_is_not_queued_again() {
    let e = env(DV1, fake_node());
    assert_eq!(e.agent.reconcile_one("ghost").await.unwrap(), Outcome::Gone);

    let queue = Queue::default();
    queue.schedule("ghost", Instant::now());
    // `serve` lists the names first and then runs the queued pass; the stop is
    // requested from that listing, so the loop ends right after the pass has
    // run and has been settled. No timing is involved.
    let shutdown = CancellationToken::new();
    *e.volumes.on_list.lock().unwrap() = Some(Box::new({
        let shutdown = shutdown.clone();
        move || shutdown.cancel()
    }));
    e.agent.serve(&queue, &shutdown).await.unwrap();

    let gets = e.volumes.calls().iter().filter(|c| *c == "get ghost").count();
    assert_eq!(gets, 2, "the pass over the name ran");
    assert!(
        queue.due.lock().unwrap().is_empty(),
        "{:?}",
        queue.due.lock().unwrap().keys().collect::<Vec<_>>()
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
    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Gone);
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

        async fn patch_status(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
            self.inner.patch_status(name, ops).await
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

    async fn patch_status(&self, name: &str, ops: Vec<Value>) -> Result<bool, ApiError> {
        self.inner.patch_status(name, ops).await
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
    assert!(preflight("node-a", "node-a\n", &paths, None).is_ok());
    assert!(preflight("node-a", "node-a\n", &paths, Some("k3s-vg")).is_ok());
}

#[test]
fn preflight_rejects_a_node_name_that_is_not_the_host_name() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let err = preflight("node-a", "node-b\n", &paths, None).unwrap_err();
    assert!(err.to_string().contains("node-b"), "{err}");
}

#[test]
fn preflight_rejects_a_missing_volumes_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let err = preflight("node-a", "node-a", &paths, None).unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
    assert!(!paths.volumes_dir.exists());
}

#[test]
fn preflight_rejects_an_invalid_volume_group_name() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let err = preflight("node-a", "node-a\n", &paths, Some("../evil-vg")).unwrap_err();
    assert!(err.to_string().contains("invalid volume group name"), "{err}");
    let err = preflight("node-a", "node-a\n", &paths, Some("-lead-dash")).unwrap_err();
    assert!(err.to_string().contains("invalid volume group name"), "{err}");
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
    assert_eq!(settle("v", &Ok(Outcome::Progress)), Some(Duration::from_secs(1)));
    assert_eq!(settle("v", &Ok(Outcome::Settled)), Some(Duration::from_secs(30)));
    assert_eq!(settle("v", &Ok(Outcome::Kept)), Some(Duration::from_secs(30)));
    assert_eq!(settle("v", &Ok(Outcome::KeptDrbd)), Some(Duration::from_secs(5)));
    assert_eq!(
        settle("v", &Ok(Outcome::Refused(Refusal::OrphanImage))),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        settle("v", &Err(AgentError::Api(ApiError::Other("down".into())))),
        Some(Duration::from_secs(5))
    );
    // A name with no object and nothing local is not queued again.
    assert_eq!(settle("v", &Ok(Outcome::Gone)), None);
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

#[tokio::test(start_paused = true)]
async fn the_first_listing_is_retried_and_readiness_waits_for_it() {
    let e = env(DV1, fake_node());
    e.volumes.put(spike("Skip"));
    e.volumes.fail_list.store(true, Ordering::SeqCst);
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });

    // 12 s of virtual time: the listing is tried at 0, 5 and 10 s.
    for _ in 0..120 {
        std::thread::sleep(Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(e.volumes.list_failures.load(Ordering::SeqCst) >= 3);
    assert!(!e.agent.ready.load(Ordering::SeqCst), "ready before any listing worked");
    assert!(!e.paths.image(VOLUME).exists(), "nothing is known to reconcile yet");

    e.volumes.fail_list.store(false, Ordering::SeqCst);
    wait_for_virtual("readiness", || e.agent.ready.load(Ordering::SeqCst)).await;
    assert!(e.volumes.entry_of(VOLUME, DV1).is_some());

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

// ---- retire_reasons (Task 6) ----

#[test]
fn retire_reasons_checks_all_rules() {
    let spec: ReplicatedVolumeSpec = serde_json::from_value(json!({
        "sizeBytes": 1073741824u64, "protocol": "C", "minor": 1000, "port": 7000, "maxPeers": 7,
        "initialSync": "Done",
        "nodes": [
            {"name": "dv-1", "nodeId": 0, "address": "192.168.104.1", "kind": "Retiring"},
            {"name": "dv-2", "nodeId": 1, "address": "192.168.104.3", "kind": "Replica"},
            {"name": "dv-3", "nodeId": 2, "address": "192.168.104.4", "kind": "Witness"},
            {"name": "dv-4", "nodeId": 3, "address": "192.168.104.5", "kind": "Client"},
        ],
    }))
    .unwrap();

    let build_status = |role: Role,
                        dv2_state: ConnectionState,
                        dv2_disk: DiskState,
                        dv3_state: ConnectionState,
                        dv4: Option<ConnectionState>|
     -> ResourceStatus {
        let dv2_conn = crate::drbd::status::Connection {
            name: "dv-2".to_string(),
            peer_node_id: 1,
            connection_state: dv2_state,
            peer_role: Role::Primary, // peer Primary does NOT prevent retiring
            peer_devices: vec![crate::drbd::status::PeerDevice {
                volume: 0,
                replication_state: "Established".to_string(),
                peer_disk_state: dv2_disk,
                peer_client: false,
                out_of_sync: 0,
            }],
        };
        let dv3_conn = crate::drbd::status::Connection {
            name: "dv-3".to_string(),
            peer_node_id: 2,
            connection_state: dv3_state,
            peer_role: Role::Secondary,
            peer_devices: vec![crate::drbd::status::PeerDevice {
                volume: 0,
                replication_state: "Established".to_string(),
                peer_disk_state: DiskState::Diskless,
                peer_client: true,
                out_of_sync: 0,
            }],
        };
        let mut connections = vec![dv2_conn, dv3_conn];
        if let Some(dv4_state) = dv4 {
            connections.push(crate::drbd::status::Connection {
                name: "dv-4".to_string(),
                peer_node_id: 3,
                connection_state: dv4_state,
                peer_role: Role::Secondary,
                peer_devices: vec![crate::drbd::status::PeerDevice {
                    volume: 0,
                    replication_state: "Established".to_string(),
                    peer_disk_state: DiskState::Diskless,
                    peer_client: true,
                    out_of_sync: 0,
                }],
            });
        }
        ResourceStatus {
            name: "spike".to_string(),
            node_id: 0,
            role,
            suspended: false,
            devices: vec![crate::drbd::status::Device {
                volume: 0,
                minor: 1000,
                disk_state: DiskState::UpToDate,
                client: false,
                open: false,
                quorum: true,
                size: 1048576,
            }],
            connections,
        }
    };

    // 1. All conditions met: this node Secondary, Replica Connected+UpToDate (even if peer is Primary!),
    //    Witness Connected, Client Connected -> no reasons.
    let ok_status = build_status(
        Role::Secondary,
        ConnectionState::Connected,
        DiskState::UpToDate,
        ConnectionState::Connected,
        Some(ConnectionState::Connected),
    );
    assert_eq!(retire_reasons(&spec, "dv-1", &ok_status), Vec::<String>::new());

    // 2. This node is Primary -> reason.
    let mut primary_status = ok_status.clone();
    primary_status.role = Role::Primary;
    assert_eq!(retire_reasons(&spec, "dv-1", &primary_status), ["this node is Primary"]);

    // 3. Witness in Connecting -> reason.
    let witness_connecting = build_status(
        Role::Secondary,
        ConnectionState::Connected,
        DiskState::UpToDate,
        ConnectionState::Connecting,
        Some(ConnectionState::Connected),
    );
    assert_eq!(
        retire_reasons(&spec, "dv-1", &witness_connecting),
        ["peer dv-3 is not connected"]
    );

    // 4. Replica with peer-disk Inconsistent -> reason.
    let replica_inconsistent = build_status(
        Role::Secondary,
        ConnectionState::Connected,
        DiskState::Inconsistent,
        ConnectionState::Connected,
        Some(ConnectionState::Connected),
    );
    assert_eq!(
        retire_reasons(&spec, "dv-1", &replica_inconsistent),
        ["peer dv-2 disk is Inconsistent"]
    );

    // 5. Peer Primary, Connected, UpToDate -> NO reason (already checked in ok_status, peer_role is Primary).
    assert!(ok_status.connections[0].peer_role == Role::Primary);
    assert_eq!(retire_reasons(&spec, "dv-1", &ok_status), Vec::<String>::new());

    // 6. Client Connected, diskless -> NO reason (checked in ok_status).

    // 7. Member of spec.nodes without a connection in status -> reason.
    let missing_conn = build_status(
        Role::Secondary,
        ConnectionState::Connected,
        DiskState::UpToDate,
        ConnectionState::Connected,
        None, // dv-4 has no connection
    );
    assert_eq!(
        retire_reasons(&spec, "dv-1", &missing_conn),
        ["peer dv-4 is not connected"]
    );
}

// ---- retire_replica execution tests (Task 6) ----

fn spike_retiring(retiring_node: &str) -> ReplicatedVolume {
    let mut spec = spec_json("Done", 1000, 7000);
    for n in &mut spec.nodes {
        if n.name == retiring_node {
            n.kind = NodeKind::Retiring;
        }
    }
    let mut vol = ReplicatedVolume::new(VOLUME, spec);
    vol.metadata.finalizers = Some(vec![replica_finalizer(DV1), replica_finalizer(DV2)]);
    vol
}

#[tokio::test]
async fn retire_replica_settled_resource_proceeds_in_order() {
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let runner = node_with_status(kernel.clone(), true);
    let e = env(DV1, runner);
    prepare_copy(&e.paths, true, true);
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1), reported(DV2)],
    });
    e.volumes.put(vol);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    // 1. Files deleted
    assert!(!e.paths.image(VOLUME).exists());
    assert!(!e.paths.ready_marker(VOLUME).exists());

    // 2. Status entry removed
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());

    // 3. Finalizer dropped
    assert_eq!(e.volumes.finalizers(VOLUME), vec![replica_finalizer(DV2)]);

    // 4. Removed from spec.nodes
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert_eq!(current_nodes.len(), 2);
    assert!(!current_nodes.iter().any(|n| n.name == DV1));

    // Verify order of calls
    assert!(e.runner.calls().iter().any(|c| c.starts_with("drbdadm down")));
    let calls = e.volumes.calls();
    let status_pos = calls
        .iter()
        .position(|c| c.contains("status") && c.contains("removed"))
        .expect("status removed");
    let fin_patch_pos = calls.iter().position(|c| c == "patch spike").expect("patch finalizer");
    let members_patch_pos = calls.iter().rposition(|c| c == "patch spike").expect("patch members");
    assert!(status_pos < fin_patch_pos);
    assert!(fin_patch_pos < members_patch_pos);
}

#[tokio::test]
async fn retire_replica_applies_when_member_is_missing_before_retire_reasons() {
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let kernel_for_adjust = kernel.clone();
    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["adjust", _]) => {
            let mut all: Value = serde_json::from_str(&kernel_for_adjust.lock().unwrap()).unwrap();
            let mut conn = all[0]["connections"][0].clone();
            conn["name"] = json!("dv-4");
            conn["peer-node-id"] = json!(3);
            conn["connection-state"] = json!("Connected");
            all[0]["connections"].as_array_mut().unwrap().push(conn);
            *kernel_for_adjust.lock().unwrap() = serde_json::to_string(&all).unwrap();
            out(0, "", "")
        }
        ("drbdsetup", _) => out(0, &kernel_for_adjust.lock().unwrap(), ""),
        _ => out(0, "", ""),
    });
    let e = env(DV1, runner);
    prepare_copy(&e.paths, true, true);
    let mut vol = spike_retiring(DV1);
    vol.spec.nodes.push(MemberNode {
        name: "dv-4".to_string(),
        node_id: 3,
        address: "192.168.104.5".to_string(),
        kind: NodeKind::Client,
    });
    e.volumes.put(vol);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV1)));
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert!(current_nodes.iter().any(|n| n.name == DV1));
    assert!(e.runner.calls().iter().any(|c| c.starts_with("drbdadm adjust")));
}

#[tokio::test]
async fn retire_replica_kept_drbd_when_reasons_present() {
    let kernel = Arc::new(Mutex::new(kernel_of(
        DV1,
        "Secondary",
        &[
            (DV2, "Connecting", "Secondary"),
            ("lima-dv-3", "Connected", "Secondary"),
        ],
    )));
    let runner = node_with_status(kernel.clone(), true);
    let e = env(DV1, runner);
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_retiring(DV1));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV1)));
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("entry written");
    assert!(
        entry.message.contains("peer lima-dv-2 is not connected"),
        "{}",
        entry.message
    );
}

#[tokio::test]
async fn retire_replica_guard_refuses_when_reasons_appear_under_lock() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let calm = kernel_json(DV1, |_| {});
    let mut busy: Value = serde_json::from_str(&calm).unwrap();
    busy[0]["role"] = json!("Primary");
    let busy = serde_json::to_string(&busy).unwrap();

    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdsetup", _) => out(
            0,
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                &calm
            } else {
                &busy
            },
            "",
        ),
        _ => out(0, "", ""),
    });
    let e = env(DV1, runner);
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_retiring(DV1));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::KeptDrbd);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV1)));
    let entry = e.volumes.entry_of(VOLUME, DV1).expect("entry written");
    assert!(entry.message.contains("this node is Primary"), "{}", entry.message);
}

#[tokio::test]
async fn retire_replica_brings_up_ready_file_when_unconfigured() {
    let runner = node_coming_up(crate::tests::THREE_INCONSISTENT.to_string(), || {});
    let e = env(DV1, runner);
    prepare_copy(&e.paths, true, true);
    e.volumes.put(spike_retiring(DV1));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(e.paths.image(VOLUME).exists());
    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV1)));
    assert!(e.runner.calls().iter().any(|c| c.starts_with("drbdadm up")));
}

#[tokio::test]
async fn retire_replica_removes_unconfigured_when_no_data() {
    let kernel = Arc::new(Mutex::new(EMPTY_KERNEL.to_string()));
    let runner = node_with_status(kernel.clone(), false);
    let e = env(DV1, runner);
    fs::create_dir_all(&e.paths.res_dir).unwrap();
    fs::write(e.paths.res_file(VOLUME), "resource spike {}").unwrap();
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1)],
    });
    e.volumes.put(vol);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
    assert_eq!(e.volumes.finalizers(VOLUME), vec![replica_finalizer(DV2)]);
    assert!(
        !e.volumes
            .object(VOLUME)
            .unwrap()
            .spec
            .nodes
            .iter()
            .any(|n| n.name == DV1)
    );
}

#[tokio::test]
async fn retire_replica_removes_unconfigured_pending_file_when_no_data() {
    let script = Script::new(EMPTY_KERNEL, Script::no_metadata());
    let e = env(DV1, script.runner());
    prepare_copy(&e.paths, true, false);
    fs::remove_file(e.paths.ready_marker(VOLUME)).unwrap();
    let _ = fs::remove_file(e.paths.device_link(VOLUME));
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1)],
    });
    e.volumes.put(vol);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.image(VOLUME).exists());
    assert!(!e.paths.res_file(VOLUME).exists());
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
    assert_eq!(e.volumes.finalizers(VOLUME), vec![replica_finalizer(DV2)]);
}

#[tokio::test]
async fn retire_replica_conflict_on_replace_members_yields_progress() {
    let volumes = Arc::new(FakeVolumes::default());
    let hook_volumes = volumes.clone();
    let kernel = Arc::new(Mutex::new(kernel_json(DV1, |_| {})));
    let kernel_for_runner = kernel.clone();
    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["down", _]) => {
            *kernel_for_runner.lock().unwrap() = EMPTY_KERNEL.to_string();
            let mut obj = hook_volumes.object(VOLUME).unwrap();
            obj.spec.nodes[0].node_id = 99;
            hook_volumes.put(obj);
            out(0, "", "")
        }
        ("drbdsetup", _) => out(0, &kernel_for_runner.lock().unwrap(), ""),
        _ => out(0, "", ""),
    });
    let e = env_sharing(DV1, runner, true, volumes.clone());
    prepare_copy(&e.paths, true, true);
    volumes.put(spike_retiring(DV1));

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert!(!e.paths.image(VOLUME).exists());
    assert_eq!(volumes.finalizers(VOLUME), vec![replica_finalizer(DV2)]);
    assert!(!volumes.object(VOLUME).unwrap().spec.nodes.iter().any(|n| n.name == DV1));
}

#[tokio::test]
async fn finish_retire_cleans_status_drops_finalizer_and_removes_member() {
    let e = env(DV1, fake_node());
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1), reported(DV2)],
    });
    e.volumes.put(vol);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);

    assert!(e.runner.calls().is_empty());
    assert!(e.volumes.entry_of(VOLUME, DV1).is_none());
    assert_eq!(e.volumes.finalizers(VOLUME), vec![replica_finalizer(DV2)]);
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert_eq!(current_nodes.len(), 2);
    assert!(!current_nodes.iter().any(|n| n.name == DV1));
}

#[tokio::test]
async fn finish_retire_keeps_when_status_clear_fails() {
    let e = env(DV1, fake_node());
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1)],
    });
    e.volumes.put(vol);
    e.volumes.co_owned.store(true, Ordering::SeqCst);
    e.volumes.fail_status_patch.store(true, Ordering::SeqCst);

    assert_eq!(e.agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);

    assert!(e.volumes.finalizers(VOLUME).contains(&replica_finalizer(DV1)));
    assert!(
        e.volumes
            .object(VOLUME)
            .unwrap()
            .spec
            .nodes
            .iter()
            .any(|n| n.name == DV1)
    );
}

#[tokio::test]
async fn stale_client_without_pods_is_cleaned_up_and_removed_from_spec() {
    let e = env(DV3, fake_node());
    let mut vol = spike_with_third("Client", &[DV1, DV2]);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV3)],
    });
    e.volumes.put(vol.clone());

    let res = crate::agent::cleanup::check_and_cleanup_client(&e.agent, VOLUME, &vol)
        .await
        .unwrap();
    assert_eq!(res, Some(Outcome::Progress));

    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert!(!current_nodes.iter().any(|n| n.name == DV3));
}

#[tokio::test]
async fn client_with_pods_is_not_cleaned_up() {
    let e = env(DV3, fake_node());
    let vol = spike_with_third("Client", &[DV1, DV2]);
    e.volumes.put(vol.clone());

    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: kube::core::ObjectMeta {
            name: Some("client-pod".to_string()),
            namespace: Some("default".to_string()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some(DV3.to_string()),
            volumes: Some(vec![k8s_openapi::api::core::v1::Volume {
                name: VOLUME.to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };
    e.volumes.pods.lock().unwrap().push(pod);

    let res = crate::agent::cleanup::check_and_cleanup_client(&e.agent, VOLUME, &vol)
        .await
        .unwrap();
    assert_eq!(res, None);
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert!(current_nodes.iter().any(|n| n.name == DV3));
    assert!(!e.runner.calls().iter().any(|c| c.contains("down")));
}

#[tokio::test]
async fn check_and_cleanup_client_fails_closed_when_drbd_status_fails() {
    let (e, status) = configured_env(
        DV3,
        kernel_json(DV3, |s| {
            s["role"] = json!("Secondary");
            s["devices"][0]["client"] = json!(true);
            s["devices"][0]["open"] = json!(false);
        }),
        true,
    );
    let mut vol = spike_with_third("Client", &[DV1, DV2]);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV3)],
    });
    e.volumes.put(vol.clone());

    // Corrupt status so read_status fails:
    *status.lock().unwrap() = "invalid json {".to_string();

    let res = crate::agent::cleanup::check_and_cleanup_client(&e.agent, VOLUME, &vol).await;
    // Must return Err (fail-closed), not delete client!
    assert!(res.is_err());
    let current_nodes = e.volumes.object(VOLUME).unwrap().spec.nodes;
    assert!(current_nodes.iter().any(|n| n.name == DV3));
}

#[tokio::test]
async fn tick_failures_taints_and_untaints_node_based_on_mounted_volume_quorum() {
    let (e, status) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["may-promote"] = json!(false);
            s["devices"][0]["open"] = json!(true);
            s["devices"][0]["quorum"] = json!(false);
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let now = std::time::Instant::now();

    // 1. Quorum lost on mounted volume -> node is tainted
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            now,
        )
        .await
        .unwrap();

    assert!(e.agent.is_tainted.load(Ordering::SeqCst));
    {
        let taints = e.volumes.node_taints.lock().unwrap();
        let dv1_taints = taints.get(DV1).expect("dv1 taints present");
        assert!(
            dv1_taints
                .iter()
                .any(|(k, ef)| k == "ambar.balookrd.github.io/no-quorum" && ef == "NoSchedule")
        );
    }

    let metrics = e.agent.metrics.get();
    assert!(
        metrics.contains("ambar_node_tainted{node=\"lima-dv-1\"} 1"),
        "{metrics}"
    );
    assert!(
        metrics.contains("ambar_volume_quorum{node=\"lima-dv-1\",volume=\"spike\"} 0"),
        "{metrics}"
    );

    // 2. Quorum restored -> taint is removed
    *status.lock().unwrap() = kernel_json(DV1, |s| {
        s["may-promote"] = json!(true);
        s["devices"][0]["open"] = json!(true);
        s["devices"][0]["quorum"] = json!(true);
    });

    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            now,
        )
        .await
        .unwrap();

    assert!(!e.agent.is_tainted.load(Ordering::SeqCst));
    {
        let taints = e.volumes.node_taints.lock().unwrap();
        let dv1_taints = taints.get(DV1).expect("dv1 taints present");
        assert!(
            !dv1_taints
                .iter()
                .any(|(k, _)| k == "ambar.balookrd.github.io/no-quorum")
        );
    }

    let metrics2 = e.agent.metrics.get();
    assert!(
        metrics2.contains("ambar_node_tainted{node=\"lima-dv-1\"} 0"),
        "{metrics2}"
    );
    assert!(
        metrics2.contains("ambar_volume_quorum{node=\"lima-dv-1\",volume=\"spike\"} 1"),
        "{metrics2}"
    );
}

#[tokio::test]
async fn tick_failures_fails_closed_when_read_all_statuses_fails() {
    let (e, status) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["may-promote"] = json!(false);
            s["devices"][0]["open"] = json!(true);
            s["devices"][0]["quorum"] = json!(false);
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let now = std::time::Instant::now();

    // 1. Quorum lost -> node is tainted
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            now,
        )
        .await
        .unwrap();
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));

    // 2. Reading DRBD status fails (e.g. invalid json / command failure)
    *status.lock().unwrap() = "not valid json".to_string();

    let res = e
        .agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            now,
        )
        .await;
    // Must return Err (fail-closed)
    assert!(res.is_err());

    // Taint must NOT be removed!
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));
    {
        let taints = e.volumes.node_taints.lock().unwrap();
        let dv1_taints = taints.get(DV1).expect("dv1 taints present");
        assert!(
            dv1_taints
                .iter()
                .any(|(k, ef)| k == "ambar.balookrd.github.io/no-quorum" && ef == "NoSchedule")
        );
    }

    // Metrics must NOT be wiped
    let metrics = e.agent.metrics.get();
    assert!(metrics.contains("ambar_node_tainted{node=\"lima-dv-1\"} 1"));
    assert!(metrics.contains("ambar_volume_quorum{node=\"lima-dv-1\",volume=\"spike\"} 0"));
}

#[tokio::test]
async fn tick_failures_evicts_pods_after_grace_period() {
    let (e, _) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["may-promote"] = json!(false);
            s["devices"][0]["open"] = json!(true);
            s["devices"][0]["quorum"] = json!(false);
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let t0 = std::time::Instant::now();

    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: kube::core::ObjectMeta {
            name: Some("test-workload".to_string()),
            namespace: Some("default".to_string()),
            uid: Some("uid-123".to_string()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some(DV1.to_string()),
            volumes: Some(vec![k8s_openapi::api::core::v1::Volume {
                name: VOLUME.to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };
    e.volumes.pods.lock().unwrap().push(pod);

    // Initial check (t0): grace period started, pod is not evicted yet
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0,
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.pods.lock().unwrap().len(), 1);

    // After 10s: still in grace period
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.pods.lock().unwrap().len(), 1);

    // After 31s: grace period expired, pod is evicted
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(31),
        )
        .await
        .unwrap();
    assert!(e.volumes.pods.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tick_failures_auto_adjusts_standalone_and_sets_split_brain_metric() {
    let (e, status) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["role"] = json!("Secondary");
            s["connections"][0]["connection-state"] = json!("StandAlone");
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let t0 = std::time::Instant::now();

    // 1st attempt
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0,
        )
        .await
        .unwrap();
    assert_eq!(
        commands(&e.runner, &["drbdadm adjust spike"]),
        vec!["drbdadm adjust spike"]
    );
    assert!(
        e.agent
            .metrics
            .get()
            .contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 0")
    );

    // 2nd attempt at t0 + 5s
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(commands(&e.runner, &["drbdadm adjust spike"]).len(), 2);
    assert!(
        e.agent
            .metrics
            .get()
            .contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 0")
    );

    // 3rd attempt at t0 + 10s
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(commands(&e.runner, &["drbdadm adjust spike"]).len(), 3);

    // After 3rd attempt: attempts exhausted, split-brain detected
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(15),
        )
        .await
        .unwrap();
    assert!(standalone_tracker.is_manual_check_needed(VOLUME));
    assert!(
        e.agent
            .metrics
            .get()
            .contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 1")
    );

    // Connection restored to Connected -> auto-resets metric
    *status.lock().unwrap() = kernel_json(DV1, |s| {
        s["role"] = json!("Secondary");
        s["connections"][0]["connection-state"] = json!("Connected");
    });
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(20),
        )
        .await
        .unwrap();
    assert!(!standalone_tracker.is_manual_check_needed(VOLUME));
    assert!(
        e.agent
            .metrics
            .get()
            .contains("ambar_volume_manual_check_needed{node=\"lima-dv-1\",volume=\"spike\"} 0")
    );
}

#[tokio::test]
async fn tick_failures_cleans_up_stale_client() {
    let kernel = Arc::new(Mutex::new(EMPTY_KERNEL.to_string()));
    let runner = node_with_status(kernel.clone(), false);
    let e = env(DV3, runner);
    e.agent.ready.store(true, Ordering::SeqCst);

    let mut vol = spike_with_third("Client", &[DV1, DV2]);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV3)],
    });
    e.volumes.put(vol);

    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let now = std::time::Instant::now();

    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            now,
        )
        .await
        .unwrap();

    let obj = e.volumes.object(VOLUME).unwrap();
    assert!(!obj.spec.nodes.iter().any(|n| n.name == DV3));
    assert!(e.volumes.entry_of(VOLUME, DV3).is_none());
}

#[tokio::test]
async fn run_failure_loop_stops_on_cancellation() {
    let e = env(DV1, fake_node());
    let shutdown = CancellationToken::new();
    let shutdown_task = shutdown.clone();
    let agent = e.agent.clone();

    let handle = tokio::spawn(async move {
        agent.run_failure_loop(shutdown_task).await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown.cancel();

    let res = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(res.is_ok(), "run_failure_loop should exit on cancellation");
}

#[tokio::test]
async fn agent_serve_metrics_listener_serves_http() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let e = env(DV1, fake_node());
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            std::time::Instant::now(),
        )
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let shutdown_server = shutdown.clone();
    let agent = e.agent.clone();

    let server_handle = tokio::spawn(async move { agent.serve_metrics_listener(listener, shutdown_server).await });

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8(buf).unwrap();

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("ambar_node_allocated_bytes{node=\"lima-dv-1\"}"));

    shutdown.cancel();
    let _ = server_handle.await;
}

#[tokio::test]
async fn tick_failures_throttles_node_taint_checks_and_retries_on_error() {
    let (e, status) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["may-promote"] = json!(true);
            s["devices"][0]["open"] = json!(true);
            s["devices"][0]["quorum"] = json!(true);
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let t0 = std::time::Instant::now();

    // 1. Initial tick reconciles once.
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0,
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 1);
    assert!(!e.agent.is_tainted.load(Ordering::SeqCst));

    // 2. Same state within resync interval skips the API call.
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 1);

    // 3. Re-sync happens once resync interval elapses.
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 2);

    // 4. Quorum loss triggers immediate reconcile and sets taint.
    *status.lock().unwrap() = kernel_json(DV1, |s| {
        s["may-promote"] = json!(false);
        s["devices"][0]["open"] = json!(true);
        s["devices"][0]["quorum"] = json!(false);
    });
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(61),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 3);
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));

    // 5. Failed reconcile does not mark synced, so next tick retries immediately.
    *status.lock().unwrap() = kernel_json(DV1, |s| {
        s["may-promote"] = json!(true);
        s["devices"][0]["open"] = json!(true);
        s["devices"][0]["quorum"] = json!(true);
    });
    e.volumes.fail.store(true, Ordering::SeqCst);
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(62),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 4);
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));

    e.volumes.fail.store(false, Ordering::SeqCst);
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(63),
        )
        .await
        .unwrap();
    assert_eq!(e.volumes.taint_checks.load(Ordering::SeqCst), 5);
    assert!(!e.agent.is_tainted.load(Ordering::SeqCst));
}

#[tokio::test]
async fn tick_failures_forgets_vanished_volumes_and_stays_fail_closed_on_drbd_error() {
    let (e, status) = configured_env(
        DV1,
        kernel_json(DV1, |s| {
            s["role"] = json!("Secondary");
            s["devices"][0]["open"] = json!(true);
            s["devices"][0]["quorum"] = json!(false);
            s["connections"][0]["connection-state"] = json!("StandAlone");
        }),
        true,
    );
    let mut quorum_tracker = crate::agent::quorum::QuorumTracker::new(Duration::from_secs(30));
    let mut standalone_tracker = crate::agent::standalone::StandaloneTracker::new(3, Duration::from_secs(5));
    let mut taint_tracker = crate::agent::taint::TaintTracker::new(Duration::from_secs(60));
    let mut cleanup_tracker = crate::agent::cleanup::CleanupTracker::new(Duration::from_secs(30));
    let t0 = std::time::Instant::now();

    // 1. Первый тик: том spike в StandAlone и без кворума (open=true) -> node tainted, attempt 1
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0,
        )
        .await
        .unwrap();
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));
    assert_eq!(
        commands(&e.runner, &["drbdadm adjust spike"]),
        vec!["drbdadm adjust spike"]
    );

    // 2. Ошибка drbdsetup -> tick_failures возвращает Err (fail-closed), трекеры и taint сохранены
    let saved_json = status.lock().unwrap().clone();
    *status.lock().unwrap() = "invalid json".to_string();
    let err = e
        .agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(5),
        )
        .await;
    assert!(err.is_err());
    assert!(e.agent.is_tainted.load(Ordering::SeqCst));

    // Восстанавливаем статус: том spike не был забыт во время ошибки drbdsetup, поэтому следующий adjust — attempt 2
    *status.lock().unwrap() = saved_json;
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(commands(&e.runner, &["drbdadm adjust spike"]).len(), 2);

    // 3. Том spike исчезает из statuses -> трекеры очищаются
    *status.lock().unwrap() = EMPTY_KERNEL.to_string();
    e.agent
        .tick_failures(
            &mut quorum_tracker,
            &mut standalone_tracker,
            &mut taint_tracker,
            &mut cleanup_tracker,
            t0 + Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert!(!e.agent.is_tainted.load(Ordering::SeqCst));

    // Проверяем, что QuorumTracker и StandaloneTracker действительно забыли spike:
    assert_eq!(
        quorum_tracker.update(VOLUME, true, false, t0 + Duration::from_secs(35)),
        crate::agent::quorum::QuorumAction::WithinGracePeriod(Duration::ZERO)
    );
    let parsed: Vec<crate::drbd::status::ResourceStatus> = serde_json::from_str(&kernel_json(DV1, |s| {
        s["role"] = json!("Secondary");
        s["connections"][0]["connection-state"] = json!("StandAlone");
    }))
    .unwrap();
    assert_eq!(
        standalone_tracker.check(&parsed[0], t0 + Duration::from_secs(15)),
        crate::agent::standalone::StandaloneAction::RunAdjust { attempt: 1 }
    );
}

#[tokio::test]
async fn first_pass_lists_volumes_once() {
    let e = env("lima-dv-4", fake_node());
    for (idx, (minor, port)) in [(1001, 7001), (1002, 7002), (1003, 7003), (1004, 7004)]
        .into_iter()
        .enumerate()
    {
        let name = format!("vol-{}", idx + 1);
        e.volumes.put(object(&name, "Done", minor, port, &[DV1, DV2]));
    }

    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });
    wait_for("ready", || e.agent.ready.load(Ordering::SeqCst)).await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("run returns after the shutdown")
        .unwrap()
        .unwrap();

    let calls = e.volumes.lists.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&calls),
        "expected 1 list call from serve (+ at most 1 from failure loop), got {calls}"
    );
}

#[tokio::test]
async fn serve_first_pass_lists_volumes_once() {
    let e = env("lima-dv-4", fake_node());
    for (idx, (minor, port)) in [(1001, 7001), (1002, 7002), (1003, 7003), (1004, 7004)]
        .into_iter()
        .enumerate()
    {
        let name = format!("vol-{}", idx + 1);
        e.volumes.put(object(&name, "Done", minor, port, &[DV1, DV2]));
    }

    let queue = Arc::new(Queue::default());
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let queue = queue.clone();
        let shutdown = shutdown.clone();
        async move { agent.serve(&queue, &shutdown).await }
    });
    wait_for("ready", || e.agent.ready.load(Ordering::SeqCst)).await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("serve returns after the shutdown")
        .unwrap()
        .unwrap();

    assert_eq!(e.volumes.lists.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn watch_event_invalidates_full_pass_peers_snapshot() {
    let e = env("lima-dv-4", fake_node());
    for (idx, (minor, port)) in [(1001, 7001), (1002, 7002), (1003, 7003), (1004, 7004)]
        .into_iter()
        .enumerate()
    {
        let name = format!("vol-{}", idx + 1);
        e.volumes.put(object(&name, "Done", minor, port, &[DV1, DV2]));
    }

    let events = e.volumes.events();
    let queue = Arc::new(Queue::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    *e.volumes.on_get.lock().unwrap() = Some(Box::new({
        let release = release.clone();
        move |name| {
            let name = name.to_string();
            Box::pin(async move {
                assert_eq!(name, "vol-1");
                let _ = entered_tx.send(());
                release.notified().await;
            })
        }
    }));

    let stop_watch = CancellationToken::new();
    let watch = tokio::spawn(feed(e.volumes.watch(), queue.clone(), stop_watch.clone()));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let queue = queue.clone();
        let shutdown = shutdown.clone();
        async move { agent.serve(&queue, &shutdown).await }
    });

    // Pause inside get("vol-1") while full_pass_peers is still active for vol-1..vol-4.
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .expect("get(vol-1) reached")
        .unwrap();
    assert!(!e.agent.ready.load(Ordering::SeqCst));
    assert_eq!(e.volumes.lists.load(Ordering::SeqCst), 1);

    // Send a watch event and wait for `feed` to set `watch_dirty` before resuming vol-1.
    events.send("vol-1".to_string()).unwrap();
    wait_for("watch_dirty", || queue.watch_dirty.load(Ordering::SeqCst)).await;
    release.notify_one();

    // Once vol-1 finishes, `serve` consumes `watch_dirty` and drops `full_pass_peers`,
    // so vol-2..vol-4 must call `list()` again during the same first pass.
    wait_for("ready", || e.agent.ready.load(Ordering::SeqCst)).await;
    let lists = e.volumes.lists.load(Ordering::SeqCst);
    assert!(
        lists >= 4,
        "expected list() to be called again for vol-2..vol-4 after watch_dirty invalidated the active snapshot, got {lists}"
    );

    shutdown.cancel();
    stop_watch.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("serve returns after the shutdown")
        .unwrap()
        .unwrap();
    let _ = watch.await;
}

#[tokio::test(start_paused = true)]
async fn serve_skips_locked_volume_and_reconciles_others_without_marking_ready_early() {
    let e = env(DV1, fake_node());
    e.volumes.put(object("vol-a", "Skip", 1001, 7001, &[DV1, DV2]));
    e.volumes.put(object("vol-b", "Skip", 1002, 7002, &[DV1, DV2]));

    // Имитируем долгий mkfs/операцию CSI на vol-a.
    let guard = e.agent.locks.lock("vol-a").await;

    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let agent = e.agent.clone();
        let shutdown = shutdown.clone();
        async move { agent.run(shutdown).await }
    });

    // 1. vol-b должен пройти реконсиляцию, не дожидаясь освобождения vol-a.
    wait_for_virtual("vol-b reconciled", || e.volumes.entry_of("vol-b", DV1).is_some()).await;

    // 2. ready остаётся false, пока guard удерживается (даже спустя > BUSY_VOLUME_RETRY_DELAY).
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !e.agent.ready.load(Ordering::SeqCst),
        "ready must stay false while vol-a is still locked and skipped"
    );
    assert!(e.volumes.entry_of("vol-a", DV1).is_none());

    // 3. Отпускаем lock — после BUSY_VOLUME_RETRY_DELAY vol-a проходит реконсиляцию и ready становится true.
    drop(guard);
    wait_for_virtual("vol-a reconciled", || e.volumes.entry_of("vol-a", DV1).is_some()).await;
    wait_for_virtual("readiness", || e.agent.ready.load(Ordering::SeqCst)).await;

    shutdown.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn settled_volume_calls_drbdsetup_status_only_once_per_pass() {
    let (e, _) = configured_env(DV1, kernel_json(DV1, |_| {}), true);
    e.volumes.put(spike("Done"));
    e.agent.reconcile_one(VOLUME).await.unwrap();
    let before = e.runner.calls().len();
    let outcome = e.agent.reconcile_one(VOLUME).await.unwrap();
    assert_eq!(outcome, Outcome::Settled);
    let after = e.runner.calls();
    let drbdsetup_calls = after[before..]
        .iter()
        .filter(|c| c.starts_with("drbdsetup status"))
        .count();
    assert_eq!(drbdsetup_calls, 1, "calls in settled pass: {:?}", &after[before..]);
}

#[tokio::test]
async fn prove_pending_metadata_uses_real_minor_and_runs_under_flock() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    prepare_copy(&paths, true, false);
    fs::remove_file(paths.ready_marker(VOLUME)).unwrap();
    let _ = fs::remove_file(paths.device_link(VOLUME));
    let _ = fs::remove_file(paths.res_file(VOLUME));

    let observed_res = Arc::new(Mutex::new(None::<String>));
    let observed_busy = Arc::new(Mutex::new(None::<bool>));
    let runner = Arc::new(FakeRunner::new({
        let paths = paths.clone();
        let observed_res = observed_res.clone();
        let observed_busy = observed_busy.clone();
        move |program, args| match (program, args) {
            ("drbdsetup", _) => out(0, EMPTY_KERNEL, ""),
            ("losetup", ["--list", ..]) => out(0, "", ""),
            ("losetup", ["--find", ..]) => out(0, "/dev/loop3\n", ""),
            ("losetup", ["-d", _]) => out(0, "", ""),
            ("drbdadm", ["dump-md", _]) => {
                *observed_res.lock().unwrap() = Some(fs::read_to_string(paths.res_file(VOLUME)).unwrap());
                let no_op = FakeRunner::new(|_, _| out(0, "", ""));
                let busy = matches!(
                    Reconciler::new(&paths, &no_op).lock_volume(VOLUME),
                    Err(ReconcileError::Busy { .. })
                );
                *observed_busy.lock().unwrap() = Some(busy);
                out(0, USED_DUMP, "")
            }
            _ => out(0, "", ""),
        }
    }));

    let volumes = Arc::new(FakeVolumes::default());
    let mut vol = spike("Done");
    vol.spec.minor = 1000;
    volumes.put(vol);
    volumes.delete(VOLUME).await.unwrap();

    let agent = Arc::new(Agent {
        node: DV1.to_string(),
        paths: paths.clone(),
        runner,
        volumes,
        locks: VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(false)),
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
    });

    assert_eq!(agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Kept);
    let res_text = observed_res.lock().unwrap().clone().expect("dump-md must be called");
    assert!(
        res_text.contains("device /dev/drbd1000;"),
        "dummy .res must use spec.minor 1000, got:\n{res_text}"
    );
    assert_eq!(
        *observed_busy.lock().unwrap(),
        Some(true),
        "prove_pending_metadata must run under volume flock"
    );
    assert!(!paths.res_file(VOLUME).exists(), "temporary .res must be cleaned up");
    assert!(
        !paths.res_file(VOLUME).with_file_name(".spike.res.tmp").exists(),
        "temporary .res.tmp must not remain"
    );
}

#[tokio::test]
async fn finish_retire_retries_replace_members_on_conflict() {
    struct ConflictOnceVolumes {
        inner: Arc<FakeVolumes>,
        replace_calls: AtomicUsize,
    }

    #[async_trait]
    impl Volumes for ConflictOnceVolumes {
        async fn get(&self, name: &str) -> Result<Option<ReplicatedVolume>, ApiError> {
            self.inner.get(name).await
        }
        async fn list(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
            self.inner.list().await
        }
        async fn list_fresh(&self) -> Result<Vec<ReplicatedVolume>, ApiError> {
            self.inner.list_fresh().await
        }
        async fn create(&self, volume: &ReplicatedVolume) -> Result<crate::agent::kube::Created, ApiError> {
            self.inner.create(volume).await
        }
        async fn delete(&self, name: &str) -> Result<(), ApiError> {
            self.inner.delete(name).await
        }
        async fn delete_exact(&self, name: &str, uid: &str) -> Result<(), ApiError> {
            self.inner.delete_exact(name, uid).await
        }
        async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError> {
            self.inner.remove_finalizer(name, uid, finalizer).await
        }
        async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
            self.inner.mark_initial_sync_done(name, uid).await
        }
        async fn mark_formatted(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
            self.inner.mark_formatted(name, uid).await
        }
        async fn replace_members(
            &self,
            name: &str,
            uid: &str,
            expected: &[MemberNode],
            members: &[MemberNode],
        ) -> Result<bool, ApiError> {
            let call = self.replace_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                // Имитируем параллельное обновление spec.nodes другим узлом до патча.
                let mut obj = self.inner.object(name).unwrap();
                obj.spec.nodes[0].node_id = 77;
                self.inner.put(obj);
            }
            self.inner.replace_members(name, uid, expected, members).await
        }
        async fn edit_members(
            &self,
            name: &str,
            uid: &str,
            expected: &[MemberNode],
            edit: &crate::replicas::Edit,
        ) -> Result<bool, ApiError> {
            self.inner.edit_members(name, uid, expected, edit).await
        }
        async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError> {
            self.inner.apply_node_status(name, node, entry).await
        }
        async fn remove_node_entry(&self, name: &str, uid: &str, node: &str) -> Result<bool, ApiError> {
            self.inner.remove_node_entry(name, uid, node).await
        }
        async fn ensure_node_taint(&self, node: &str, key: &str, effect: &str) -> Result<bool, ApiError> {
            self.inner.ensure_node_taint(node, key, effect).await
        }
        async fn remove_node_taint(&self, node: &str, key: &str, effect: &str) -> Result<bool, ApiError> {
            self.inner.remove_node_taint(node, key, effect).await
        }
        async fn delete_node_pods_for_volume(
            &self,
            node: &str,
            volume: &str,
            mounted_uids: &[String],
        ) -> Result<Vec<String>, ApiError> {
            self.inner.delete_node_pods_for_volume(node, volume, mounted_uids).await
        }
        async fn has_node_pods_for_volume(
            &self,
            node: &str,
            volume: &str,
            mounted_uids: &[String],
        ) -> Result<bool, ApiError> {
            self.inner.has_node_pods_for_volume(node, volume, mounted_uids).await
        }
        fn watch(&self) -> futures_util::stream::BoxStream<'static, Result<String, ApiError>> {
            self.inner.watch()
        }
    }

    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let inner = Arc::new(FakeVolumes::default());
    let mut vol = spike_retiring(DV1);
    vol.status = Some(crate::crd::ReplicatedVolumeStatus {
        nodes: vec![reported(DV1), reported(DV2)],
    });
    inner.put(vol);

    let volumes = Arc::new(ConflictOnceVolumes {
        inner: inner.clone(),
        replace_calls: AtomicUsize::new(0),
    });

    let agent = Arc::new(Agent {
        node: DV1.to_string(),
        paths,
        runner: Arc::new(fake_node()),
        volumes: volumes.clone(),
        locks: VolumeLocks::default(),
        ready: Arc::new(AtomicBool::new(false)),
        options: Default::default(),
        metrics: Default::default(),
        is_tainted: Arc::new(AtomicBool::new(false)),
        staging: Default::default(),
    });

    assert_eq!(agent.reconcile_one(VOLUME).await.unwrap(), Outcome::Progress);
    assert_eq!(volumes.replace_calls.load(Ordering::SeqCst), 2);
    let current_nodes = inner.object(VOLUME).unwrap().spec.nodes;
    assert!(!current_nodes.iter().any(|n| n.name == DV1));
}
