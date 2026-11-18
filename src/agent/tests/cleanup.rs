use super::*;
use crate::crd::{MemberNode, NodeKind, ReplicatedVolume, ReplicatedVolumeSpec};
use crate::drbd::status::{Device, DiskState, ResourceStatus, Role};
use crate::tests::volumes::FakeVolumes;
use std::sync::Arc;

fn member(name: &str, node_id: u32, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.to_string(),
        node_id,
        address: format!("192.168.104.{node_id}"),
        kind,
    }
}

fn spec_with_nodes(nodes: Vec<MemberNode>) -> ReplicatedVolumeSpec {
    ReplicatedVolumeSpec {
        size_bytes: 1024,
        protocol: crate::model::Protocol::C,
        max_peers: 7,
        minor: 1000,
        port: 7000,
        nodes,
        initial_sync: Default::default(),
    }
}

fn res_status(role: Role, open: bool) -> ResourceStatus {
    ResourceStatus {
        name: "vol1".to_string(),
        node_id: 1,
        role,
        suspended: false,
        devices: vec![Device {
            volume: 0,
            minor: 1000,
            disk_state: DiskState::Diskless,
            client: true,
            open,
            quorum: true,
            size: 1024,
        }],
        connections: Vec::new(),
    }
}

#[test]
fn should_cleanup_returns_true_for_secondary_client_without_pods() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_has_pods() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), true));
}

#[test]
fn should_cleanup_returns_false_for_replica_or_witness() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Witness),
    ]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n1", &spec, Some(&status), false));
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_primary() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Primary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_false_when_open() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let status = res_status(Role::Secondary, true);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[test]
fn should_cleanup_returns_true_when_status_is_none() {
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    assert!(should_cleanup_client("n2", &spec, None, false));
}

#[test]
fn should_cleanup_returns_false_when_not_a_member() {
    let spec = spec_with_nodes(vec![member("n1", 1, NodeKind::Replica)]);
    let status = res_status(Role::Secondary, false);
    assert!(!should_cleanup_client("n2", &spec, Some(&status), false));
}

#[tokio::test]
async fn leave_client_removes_client_member_from_spec() {
    let fake = Arc::new(FakeVolumes::default());
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Client),
    ]);
    let mut rv = ReplicatedVolume::new("vol1", spec);
    rv.metadata.uid = Some("uid-1".to_string());
    fake.create(&rv).await.unwrap();

    let removed = leave_client(&*fake, "n2", "vol1").await.unwrap();
    assert!(removed);

    let updated = fake.get("vol1").await.unwrap().unwrap();
    assert_eq!(updated.spec.nodes.len(), 1);
    assert_eq!(updated.spec.nodes[0].name, "n1");
}

#[tokio::test]
async fn leave_client_noops_when_not_client() {
    let fake = Arc::new(FakeVolumes::default());
    let spec = spec_with_nodes(vec![
        member("n1", 1, NodeKind::Replica),
        member("n2", 2, NodeKind::Witness),
    ]);
    let mut rv = ReplicatedVolume::new("vol1", spec);
    rv.metadata.uid = Some("uid-1".to_string());
    fake.create(&rv).await.unwrap();

    let result = leave_client(&*fake, "n2", "vol1").await.unwrap();
    assert!(result);

    let updated = fake.get("vol1").await.unwrap().unwrap();
    assert_eq!(updated.spec.nodes.len(), 2);
}
