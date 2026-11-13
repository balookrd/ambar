use super::*;
use crate::drbd::status::parse;

macro_rules! sample {
    ($name:literal) => {
        include_str!(concat!("../../../stand/spike/samples/", $name, ".json"))
    };
}

fn one(json: &str) -> ResourceStatus {
    let mut all = parse(json).expect("sample parses");
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn rv_spec() -> ReplicatedVolumeSpec {
    serde_json::from_value(serde_json::json!({
        "sizeBytes": 1073741824u64, "protocol": "C", "minor": 1000, "port": 7900, "maxPeers": 3,
        "nodes": [
            {"name": "r1", "nodeId": 0, "address": "192.0.2.1", "kind": "Replica"},
            {"name": "r2", "nodeId": 1, "address": "192.0.2.2", "kind": "Replica"},
            {"name": "w", "nodeId": 2, "address": "192.0.2.3", "kind": "Witness"},
            {"name": "c", "nodeId": 3, "address": "192.0.2.4", "kind": "Client"},
        ],
    }))
    .unwrap()
}

fn entry(name: &str, disk: &str) -> NodeStatus {
    NodeStatus {
        name: name.to_string(),
        role: "Secondary".to_string(),
        disk: disk.to_string(),
        quorum: true,
        connections: vec![],
        message: String::new(),
    }
}

// --- node_status -------------------------------------------------------------

#[test]
fn node_status_of_a_diskful_node() {
    let st = one(sample!("three-uptodate"));
    let ns = node_status("n1", Some(&st), "");
    assert_eq!(ns.name, "n1");
    assert_eq!(ns.role, "Secondary");
    assert_eq!(ns.disk, "UpToDate");
    assert!(ns.quorum);
    assert_eq!(ns.message, "");
    assert_eq!(ns.connections.len(), 2);
    for c in &ns.connections {
        assert_eq!(c.state, "Connected");
        assert_eq!(c.peer_disk, "UpToDate");
    }
}

#[test]
fn node_status_connections_are_sorted_by_peer() {
    let st = one(sample!("three-uptodate"));
    let mut reversed = st.clone();
    reversed.connections.reverse();
    let a = node_status("n1", Some(&st), "");
    let b = node_status("n1", Some(&reversed), "");
    assert_eq!(a, b);
    let peers: Vec<&str> = a.connections.iter().map(|c| c.peer.as_str()).collect();
    let mut sorted = peers.clone();
    sorted.sort();
    assert_eq!(peers, sorted);
}

#[test]
fn node_status_of_a_diskless_node() {
    let st = one(sample!("diskless-node"));
    let ns = node_status("n3", Some(&st), "");
    assert_eq!(ns.disk, "Diskless");
    assert!(ns.quorum);
    assert!(ns.connections.iter().all(|c| c.peer_disk == "UpToDate"));
}

#[test]
fn node_status_uses_drbd_spelling_for_states() {
    let st = one(sample!("peer-disconnected"));
    let ns = node_status("n1", Some(&st), "boom");
    assert_eq!(ns.message, "boom");
    assert!(ns.connections.iter().any(|c| c.state != "Connected"));
    let st = one(sample!("standalone-primary"));
    let ns = node_status("n1", Some(&st), "");
    assert_eq!(ns.role, "Primary");
    assert!(ns.connections.iter().any(|c| c.state == "StandAlone"));
    let st = one(sample!("three-inconsistent"));
    assert_eq!(node_status("n1", Some(&st), "").disk, "Inconsistent");
}

#[test]
fn node_status_of_an_unconfigured_resource() {
    let ns = node_status("n1", None, "no resource");
    assert_eq!(
        ns,
        NodeStatus {
            name: "n1".to_string(),
            role: "Unknown".to_string(),
            disk: "Unknown".to_string(),
            quorum: false,
            connections: vec![],
            message: "no resource".to_string(),
        }
    );
}

// --- usable ------------------------------------------------------------------

#[test]
fn usable_local_up_to_date_with_quorum() {
    assert!(usable(Some(&one(sample!("three-uptodate")))));
}

#[test]
fn usable_local_inconsistent_but_a_connected_peer_is_up_to_date() {
    let mut st = one(sample!("three-uptodate"));
    st.devices[0].disk_state = DiskState::Inconsistent;
    assert!(usable(Some(&st)));
}

#[test]
fn usable_diskless_with_a_connected_up_to_date_peer() {
    assert!(usable(Some(&one(sample!("diskless-node")))));
}

#[test]
fn not_usable_without_quorum() {
    for name in [
        one(sample!("quorum-lost")),
        one(sample!("stale-diskful-no-quorum")),
        one(sample!("diskless-alone-no-quorum")),
    ] {
        assert!(!usable(Some(&name)), "{}", name.name);
    }
    let mut st = one(sample!("three-uptodate"));
    st.devices[0].quorum = false;
    assert!(!usable(Some(&st)));
}

#[test]
fn not_usable_when_no_disk_in_sight_is_up_to_date() {
    let mut st = one(sample!("diskless-node"));
    for c in &mut st.connections {
        for p in &mut c.peer_devices {
            p.peer_disk_state = DiskState::Inconsistent;
        }
    }
    assert!(!usable(Some(&st)));
}

#[test]
fn up_to_date_peer_that_is_not_connected_does_not_count() {
    let mut st = one(sample!("diskless-node"));
    for c in &mut st.connections {
        c.connection_state = ConnectionState::Connecting;
    }
    assert!(!usable(Some(&st)));
}

#[test]
fn not_usable_when_the_resource_is_not_configured() {
    assert!(!usable(None));
}

// --- all_replicas_up_to_date -------------------------------------------------

#[test]
fn all_replicas_up_to_date_when_every_replica_reports_it() {
    let status = ReplicatedVolumeStatus {
        nodes: vec![entry("r1", "UpToDate"), entry("r2", "UpToDate")],
    };
    assert!(all_replicas_up_to_date(&rv_spec(), &status));
}

#[test]
fn witness_and_client_are_not_counted() {
    let status = ReplicatedVolumeStatus {
        nodes: vec![
            entry("r1", "UpToDate"),
            entry("r2", "UpToDate"),
            entry("w", "Diskless"),
            entry("c", "Diskless"),
        ],
    };
    assert!(all_replicas_up_to_date(&rv_spec(), &status));
}

#[test]
fn a_replica_without_an_entry_is_not_up_to_date() {
    let status = ReplicatedVolumeStatus {
        nodes: vec![entry("r1", "UpToDate"), entry("w", "Diskless")],
    };
    assert!(!all_replicas_up_to_date(&rv_spec(), &status));
    assert!(!all_replicas_up_to_date(&rv_spec(), &ReplicatedVolumeStatus::default()));
}

#[test]
fn a_replica_that_is_still_syncing_is_not_up_to_date() {
    let status = ReplicatedVolumeStatus {
        nodes: vec![entry("r1", "UpToDate"), entry("r2", "Inconsistent")],
    };
    assert!(!all_replicas_up_to_date(&rv_spec(), &status));
}
