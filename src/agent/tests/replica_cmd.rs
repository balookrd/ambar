use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::crd::{
    MemberNode, NodeKind, NodeStatus, PeerStatus, ReplicatedVolume, ReplicatedVolumeSpec, ReplicatedVolumeStatus,
};
use crate::membership::replica_finalizer;
use crate::model::InitialSync;
use crate::tests::volumes::{FakeNodes, FakeVolumes, cluster, typed};

const GIB: u64 = 1024 * 1024 * 1024;

fn member(name: &str, node_id: u32, address: &str, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.to_string(),
        node_id,
        address: address.to_string(),
        kind,
    }
}

fn set_all_uptodate(volumes: &FakeVolumes, name: &str, members: &[MemberNode]) {
    let mut obj = volumes.object(name).unwrap();
    let mut nodes_status = Vec::new();
    for m in members {
        let is_diskful = matches!(m.kind, NodeKind::Replica | NodeKind::Retiring);
        let mut connections = Vec::new();
        for peer in members {
            if peer.name == m.name {
                continue;
            }
            let peer_is_diskful = matches!(peer.kind, NodeKind::Replica | NodeKind::Retiring);
            connections.push(PeerStatus {
                peer: peer.name.clone(),
                state: "Connected".to_string(),
                peer_disk: if peer_is_diskful {
                    "UpToDate".to_string()
                } else {
                    "Diskless".to_string()
                },
            });
        }
        nodes_status.push(NodeStatus {
            name: m.name.clone(),
            role: "Secondary".to_string(),
            disk: if is_diskful {
                "UpToDate".to_string()
            } else {
                "Diskless".to_string()
            },
            quorum: true,
            connections,
            message: String::new(),
        });
    }
    obj.status = Some(ReplicatedVolumeStatus { nodes: nodes_status });
    volumes.put(obj);
}

fn make_volume(name: &str, members: Vec<MemberNode>) -> ReplicatedVolume {
    let finalizers: Vec<String> = members
        .iter()
        .filter(|m| matches!(m.kind, NodeKind::Replica | NodeKind::Retiring))
        .map(|m| replica_finalizer(&m.name))
        .collect();

    let mut nodes_status = Vec::new();
    for member in &members {
        let is_diskful = matches!(member.kind, NodeKind::Replica | NodeKind::Retiring);
        let mut connections = Vec::new();
        for peer in &members {
            if peer.name == member.name {
                continue;
            }
            let peer_is_diskful = matches!(peer.kind, NodeKind::Replica | NodeKind::Retiring);
            connections.push(PeerStatus {
                peer: peer.name.clone(),
                state: "Connected".to_string(),
                peer_disk: if peer_is_diskful {
                    "UpToDate".to_string()
                } else {
                    "Diskless".to_string()
                },
            });
        }
        nodes_status.push(NodeStatus {
            name: member.name.clone(),
            role: "Secondary".to_string(),
            disk: if is_diskful {
                "UpToDate".to_string()
            } else {
                "Diskless".to_string()
            },
            quorum: true,
            connections,
            message: String::new(),
        });
    }

    let spec = ReplicatedVolumeSpec {
        size_bytes: GIB,
        protocol: crate::model::Protocol::C,
        minor: 1000,
        port: 7000,
        max_peers: 7,
        initial_sync: InitialSync::Done,
        nodes: members,
    };

    typed(&json!({
        "apiVersion": "ambar.balookrd.github.io/v1alpha1",
        "kind": "ReplicatedVolume",
        "metadata": {
            "name": name,
            "uid": "uid-1",
            "finalizers": finalizers,
        },
        "spec": spec,
        "status": ReplicatedVolumeStatus { nodes: nodes_status },
    }))
}

#[tokio::test(start_paused = true)]
async fn add_to_free_node_applies_edit_and_finishes_on_uptodate() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Replica),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let v_clone = volumes.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(current) = v_clone.object("v")
                && current.spec.nodes.iter().any(|m| m.name == "n4")
            {
                set_all_uptodate(&v_clone, "v", &current.spec.nodes);
                break;
            }
        }
    });

    let mut out = Vec::new();
    let action = ReplicaAction::Add {
        volume: "v".into(),
        node: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 0);
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("edit: +n4 Replica, +finalizer ambar.balookrd.github.io/replica-n4"));
    assert!(output.contains("OK: n4 holds a copy of v"));

    let patch_calls = volumes.calls().into_iter().filter(|c| c.starts_with("patch v")).count();
    assert_eq!(patch_calls, 1);
}

struct ConflictOnceVolumes<'a> {
    inner: &'a FakeVolumes,
    conflicted: AtomicBool,
}

#[async_trait]
impl Volumes for ConflictOnceVolumes<'_> {
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
    async fn delete_exact(&self, name: &str, uid: &str) -> Result<(), ApiError> {
        self.inner.delete_exact(name, uid).await
    }
    async fn delete(&self, name: &str) -> Result<(), ApiError> {
        self.inner.delete(name).await
    }
    async fn remove_finalizer(&self, name: &str, uid: &str, finalizer: &str) -> Result<bool, ApiError> {
        self.inner.remove_finalizer(name, uid, finalizer).await
    }
    async fn mark_initial_sync_done(&self, name: &str, uid: &str) -> Result<bool, ApiError> {
        self.inner.mark_initial_sync_done(name, uid).await
    }
    async fn remove_node_entry(&self, name: &str, uid: &str, node: &str) -> Result<bool, ApiError> {
        self.inner.remove_node_entry(name, uid, node).await
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
        self.inner.replace_members(name, uid, expected, members).await
    }
    async fn edit_members(
        &self,
        name: &str,
        uid: &str,
        expected: &[MemberNode],
        edit: &crate::replicas::Edit,
    ) -> Result<bool, ApiError> {
        if !self.conflicted.swap(true, Ordering::SeqCst) {
            return Ok(false);
        }
        self.inner.edit_members(name, uid, expected, edit).await
    }
    async fn apply_node_status(&self, name: &str, node: &str, entry: Option<&NodeStatus>) -> Result<(), ApiError> {
        self.inner.apply_node_status(name, node, entry).await
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

#[tokio::test(start_paused = true)]
async fn edit_members_conflict_re_reads_and_recomputes() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Replica),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let v_clone = volumes.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(current) = v_clone.object("v")
                && current.spec.nodes.iter().any(|m| m.name == "n4")
            {
                set_all_uptodate(&v_clone, "v", &current.spec.nodes);
                break;
            }
        }
    });

    let conflict_v = ConflictOnceVolumes {
        inner: volumes.as_ref(),
        conflicted: AtomicBool::new(false),
    };

    let mut out = Vec::new();
    let action = ReplicaAction::Add {
        volume: "v".into(),
        node: "n4".into(),
    };
    let code = execute(&conflict_v, &fake_nodes, &action, 0, &mut out, CancellationToken::new()).await;

    assert_eq!(code, 0);
    assert!(conflict_v.conflicted.load(Ordering::SeqCst));
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("OK: n4 holds a copy of v"));
}

#[tokio::test(start_paused = true)]
async fn change_error_exits_with_code_1_without_patch() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Witness),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let mut out = Vec::new();
    // Removing when N=2 is rejected by R23/TooFewReplicas
    let action = ReplicaAction::Remove {
        volume: "v".into(),
        node: "n1".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 1);
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("cannot remove replica: at least 2 replicas are required"));
    let patches = volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch"))
        .collect::<Vec<_>>();
    assert!(patches.is_empty(), "expected no patches, got: {patches:?}");
}

#[tokio::test(start_paused = true)]
async fn timeout_in_wait_exits_with_code_1_no_patches_after() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Replica),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let mut out = Vec::new();
    let action = ReplicaAction::Add {
        volume: "v".into(),
        node: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        2,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 1);
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("timed out; run the same command again to continue"));

    let patch_count = volumes.calls().into_iter().filter(|c| c.starts_with("patch v")).count();
    // One patch was done to add n4, but none after the timeout expired
    assert_eq!(patch_count, 1);
}

#[tokio::test(start_paused = true)]
async fn move_with_even_n_does_two_patches_and_preserves_witness() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Witness),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let v_clone = volumes.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(mut current) = v_clone.object("v") {
                let has_n4 = current.spec.nodes.iter().any(|m| m.name == "n4");
                let n1_retiring = current
                    .spec
                    .nodes
                    .iter()
                    .any(|m| m.name == "n1" && m.kind == NodeKind::Retiring);

                if has_n4 && !n1_retiring {
                    set_all_uptodate(&v_clone, "v", &current.spec.nodes);
                } else if n1_retiring {
                    // Agent removes n1 from spec.nodes and removes replica-n1 finalizer
                    current.spec.nodes.retain(|m| m.name != "n1");
                    if let Some(ref mut f) = current.metadata.finalizers {
                        let finalizer = replica_finalizer("n1");
                        f.retain(|name| name != &finalizer);
                    }
                    v_clone.put(current);
                    break;
                }
            }
        }
    });

    let mut out = Vec::new();
    let action = ReplicaAction::Move {
        volume: "v".into(),
        from: "n1".into(),
        to: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 0);
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.lines().filter(|l| l.starts_with("edit:")).count(), 2);
    assert!(output.contains("OK: the copy of v moved from n1 to n4"));

    let patch_count = volumes.calls().into_iter().filter(|c| c.starts_with("patch v")).count();
    assert_eq!(patch_count, 2);

    let final_obj = volumes.object("v").unwrap();
    let witness = final_obj
        .spec
        .nodes
        .iter()
        .find(|m| m.name == "n3")
        .expect("witness n3 must remain");
    assert_eq!(witness.kind, NodeKind::Witness);
}

#[tokio::test(start_paused = true)]
async fn resumed_move_after_interruption_waits_without_extra_patches() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    // n4 already added, n1 is already Retiring
    let members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Retiring),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Witness),
        member("n4", 3, "192.0.2.4", NodeKind::Replica),
    ];
    let vol = make_volume("v", members);
    volumes.put(vol);

    let v_clone = volumes.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut current = v_clone.object("v").unwrap();
        current.spec.nodes.retain(|m| m.name != "n1");
        if let Some(ref mut f) = current.metadata.finalizers {
            let finalizer = replica_finalizer("n1");
            f.retain(|name| name != &finalizer);
        }
        v_clone.put(current);
    });

    let mut out = Vec::new();
    let action = ReplicaAction::Move {
        volume: "v".into(),
        from: "n1".into(),
        to: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 0);
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.lines().filter(|l| l.starts_with("edit:")).count(), 0);
    assert!(output.contains("OK: the copy of v moved from n1 to n4"));
    let patches = volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch"))
        .collect::<Vec<_>>();
    assert!(patches.is_empty(), "expected no patches, got: {patches:?}");
}

#[tokio::test(start_paused = true)]
async fn remove_unknown_member_fails_with_not_a_member_without_patches() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let initial_members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Replica),
    ];
    let vol = make_volume("v", initial_members);
    volumes.put(vol);

    let mut out = Vec::new();
    let action = ReplicaAction::Remove {
        volume: "v".into(),
        node: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 1);
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("node \"n4\" is not a member of the volume"));
    let patches = volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch"))
        .collect::<Vec<_>>();
    assert!(patches.is_empty());
}

#[tokio::test(start_paused = true)]
async fn repeated_move_after_completion_returns_0_without_patches() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    // Final state: n4 is Replica, n1 is gone
    let members = vec![
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Witness),
        member("n4", 3, "192.0.2.4", NodeKind::Replica),
    ];
    let vol = make_volume("v", members);
    volumes.put(vol);

    let mut out = Vec::new();
    let action = ReplicaAction::Move {
        volume: "v".into(),
        from: "n1".into(),
        to: "n4".into(),
    };
    let code = execute(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;

    assert_eq!(code, 0);
    let output = String::from_utf8(out).unwrap();
    assert_eq!(output.lines().filter(|l| l.starts_with("edit:")).count(), 0);
    assert!(output.contains("OK: the copy of v moved from n1 to n4"));
    let patches = volumes
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("patch"))
        .collect::<Vec<_>>();
    assert!(patches.is_empty(), "expected no patches, got: {patches:?}");
}

#[tokio::test(start_paused = true)]
async fn run_replica_does_not_call_get_separately_when_list_fresh_is_used() {
    let fake_nodes = FakeNodes(Ok(cluster(&[10 * GIB, 10 * GIB, 10 * GIB, 10 * GIB])));
    let volumes = Arc::new(FakeVolumes::default());
    let members = vec![
        member("n1", 0, "192.0.2.1", NodeKind::Replica),
        member("n2", 1, "192.0.2.2", NodeKind::Replica),
        member("n3", 2, "192.0.2.3", NodeKind::Replica),
    ];
    volumes.put(make_volume("v", members));

    let mut out = Vec::new();
    let action = ReplicaAction::Add {
        volume: "v".into(),
        node: "n3".into(),
    };
    let res = run_replica(
        volumes.as_ref(),
        &fake_nodes,
        &action,
        0,
        &mut out,
        CancellationToken::new(),
    )
    .await;
    assert!(res.is_ok(), "unexpected error: {res:?}");

    let get_calls = volumes.calls().into_iter().filter(|c| c.starts_with("get ")).count();
    assert_eq!(get_calls, 0, "expected no separate get calls during run_replica");
    assert!(
        volumes.fresh_lists.load(Ordering::SeqCst) >= 1,
        "expected list_fresh to be called"
    );
}
