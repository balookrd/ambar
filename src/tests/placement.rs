use std::collections::BTreeMap;

use super::*;
use crate::crd::{MemberNode, NodeKind, ReplicatedVolume, ReplicatedVolumeSpec, to_volume_spec};
use crate::model::InitialSync;

/// Nodes `n1..` with the given capacities; node ids 0.., addresses 192.0.2.1...
fn cluster(caps: &[u64]) -> ClusterNodes {
    let list: Vec<String> = caps
        .iter()
        .enumerate()
        .map(|(i, cap)| {
            format!(
                r#"{{"name":"n{}","address":"192.0.2.{}","nodeId":{},"capacityBytes":{}}}"#,
                i + 1,
                i + 1,
                i,
                cap
            )
        })
        .collect();
    ClusterNodes::from_json(&format!("[{}]", list.join(","))).unwrap()
}

const GIB: u64 = 1 << 30;

fn request(replicas: usize) -> Request {
    Request {
        size_bytes: GIB,
        replicas,
        protocol: Protocol::C,
        max_peers: DEFAULT_MAX_PEERS,
        selected_node: None,
    }
}

fn volume(minor: u32, port: u16, nodes: &[&str], size_bytes: u64) -> Existing {
    Existing {
        minor,
        port,
        replica_nodes: nodes.iter().map(|n| n.to_string()).collect(),
        size_bytes,
    }
}

fn kinds(spec: &ReplicatedVolumeSpec) -> Vec<(&str, NodeKind)> {
    spec.nodes.iter().map(|n| (n.name.as_str(), n.kind)).collect()
}

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn three_nodes_three_replicas_no_witness() {
    let spec = place(&request(3), &cluster(&[100 * GIB; 3]), &[]).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n1", NodeKind::Replica),
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Replica)
        ]
    );
    assert_eq!(spec.minor, 1000);
    assert_eq!(spec.port, 7000);
    assert_eq!(spec.initial_sync, InitialSync::Skip);
    assert_eq!(spec.nodes[1].address, "192.0.2.2");
    assert_eq!(spec.nodes[1].node_id, 1);
}

#[test]
fn selected_node_first_then_largest_budget_and_witness_last() {
    let nodes = cluster(&[10 * GIB, 50 * GIB, 90 * GIB]);
    let mut req = request(2);
    req.selected_node = Some("n2".into());
    let spec = place(&req, &nodes, &[]).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n1", NodeKind::Witness),
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Replica)
        ]
    );
}

#[test]
fn four_nodes_two_replicas_one_witness_one_node_left_out() {
    let nodes = cluster(&[40 * GIB, 30 * GIB, 20 * GIB, 10 * GIB]);
    let spec = place(&request(2), &nodes, &[]).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n1", NodeKind::Replica),
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Witness)
        ]
    );
}

#[test]
fn two_nodes_two_replicas_no_witness() {
    let spec = place(&request(2), &cluster(&[10 * GIB; 2]), &[]).unwrap();
    assert_eq!(kinds(&spec), [("n1", NodeKind::Replica), ("n2", NodeKind::Replica)]);
}

#[test]
fn four_nodes_four_replicas_no_witness() {
    let spec = place(&request(4), &cluster(&[10 * GIB; 4]), &[]).unwrap();
    assert_eq!(spec.nodes.len(), 4);
    assert!(spec.nodes.iter().all(|n| n.kind == NodeKind::Replica));
}

#[test]
fn one_replica_is_refused() {
    assert_eq!(
        place(&request(1), &cluster(&[10 * GIB; 3]), &[]),
        Err(PlaceError::Replicas(1))
    );
}

#[test]
fn more_replicas_than_nodes_is_refused() {
    assert_eq!(
        place(&request(4), &cluster(&[10 * GIB; 3]), &[]),
        Err(PlaceError::NotEnoughNodes { replicas: 4, nodes: 3 })
    );
}

#[test]
fn unknown_selected_node_is_refused() {
    let mut req = request(2);
    req.selected_node = Some("zz".into());
    assert_eq!(
        place(&req, &cluster(&[10 * GIB; 3]), &[]),
        Err(PlaceError::UnknownSelectedNode("zz".into()))
    );
}

#[test]
fn selected_node_without_budget_is_named() {
    let nodes = cluster(&[10 * GIB, 10 * GIB, 10 * GIB]);
    let existing = [volume(1000, 7000, &["n2"], 10 * GIB - GIB / 2)];
    let mut req = request(2);
    req.selected_node = Some("n2".into());
    assert_eq!(
        place(&req, &nodes, &existing),
        Err(PlaceError::NoCapacity {
            node: "n2".into(),
            free: GIB / 2,
            needed: GIB
        })
    );
}

#[test]
fn other_node_without_budget_is_skipped() {
    let nodes = cluster(&[10 * GIB; 4]);
    // n1 is full, the other three have room.
    let existing = [volume(1000, 7000, &["n1"], 10 * GIB)];
    let spec = place(&request(3), &nodes, &existing).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Replica),
            ("n4", NodeKind::Replica)
        ]
    );
}

#[test]
fn too_few_nodes_with_budget_names_the_best_skipped() {
    let nodes = cluster(&[10 * GIB; 3]);
    let existing = [
        volume(1000, 7000, &["n1"], 10 * GIB),
        volume(1001, 7001, &["n2"], 10 * GIB - GIB / 2),
    ];
    assert_eq!(
        place(&request(3), &nodes, &existing),
        Err(PlaceError::NoCapacity {
            node: "n2".into(),
            free: GIB / 2,
            needed: GIB
        })
    );
}

#[test]
fn witness_needs_no_budget() {
    let nodes = cluster(&[10 * GIB; 3]);
    // n3 is full but still the only node left for the witness.
    let existing = [volume(1000, 7000, &["n3"], 10 * GIB)];
    let spec = place(&request(2), &nodes, &existing).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n1", NodeKind::Replica),
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Witness)
        ]
    );
}

#[test]
fn indexes_zero_and_two_taken_picks_one() {
    let nodes = cluster(&[10 * GIB; 3]);
    let existing = [volume(1000, 7000, &[], GIB), volume(1002, 7002, &[], GIB)];
    let spec = place(&request(3), &nodes, &existing).unwrap();
    assert_eq!((spec.minor, spec.port), (1001, 7001));
}

#[test]
fn minor_and_port_taken_by_different_volumes_picks_two() {
    let nodes = cluster(&[10 * GIB; 3]);
    let existing = [volume(1001, 9000, &[], GIB), volume(2000, 7000, &[], GIB)];
    let spec = place(&request(3), &nodes, &existing).unwrap();
    assert_eq!((spec.minor, spec.port), (1002, 7002));
}

#[test]
fn port_range_exhausted_is_no_free_index() {
    let nodes = cluster(&[10 * GIB; 3]);
    let existing: Vec<Existing> = (0..=(65535 - 7000))
        .map(|i| volume(5000 + i as u32, 7000 + i as u16, &[], 0))
        .collect();
    assert_eq!(place(&request(3), &nodes, &existing), Err(PlaceError::NoFreeIndex));
}

#[test]
fn max_peers_below_cluster_size_is_refused() {
    let mut req = request(3);
    req.max_peers = 1;
    assert_eq!(
        place(&req, &cluster(&[10 * GIB; 3]), &[]),
        Err(PlaceError::MaxPeers {
            max_peers: 1,
            needed: 2
        })
    );
}

#[test]
fn max_peers_above_limit_is_refused() {
    let mut req = request(3);
    req.max_peers = 32;
    assert_eq!(
        place(&req, &cluster(&[10 * GIB; 3]), &[]),
        Err(PlaceError::MaxPeers {
            max_peers: 32,
            needed: 2
        })
    );
}

#[test]
fn equal_budgets_choose_by_name() {
    let nodes = cluster(&[10 * GIB; 4]);
    let spec = place(&request(3), &nodes, &[]).unwrap();
    assert_eq!(
        kinds(&spec),
        [
            ("n1", NodeKind::Replica),
            ("n2", NodeKind::Replica),
            ("n3", NodeKind::Replica)
        ]
    );
}

#[test]
fn parse_empty_gives_defaults() {
    assert_eq!(parse_parameters(&params(&[])), Ok((3, Protocol::C, 7)));
}

#[test]
fn parse_all_parameters() {
    let p = params(&[("replicas", "2"), ("protocol", "A"), ("maxPeers", "5")]);
    assert_eq!(parse_parameters(&p), Ok((2, Protocol::A, 5)));
}

#[test]
fn parse_errors_name_the_parameter() {
    for (key, value) in [("replicas", "two"), ("protocol", "B"), ("foo", "1")] {
        let err = parse_parameters(&params(&[(key, value)])).unwrap_err();
        assert!(err.to_string().contains(key), "{err}");
    }
}

#[test]
fn parse_ignores_csi_keys() {
    let p = params(&[("csi.storage.k8s.io/fstype", "ext4")]);
    assert_eq!(parse_parameters(&p), Ok((3, Protocol::C, 7)));
}

#[test]
fn round_up_size_cases() {
    assert_eq!(round_up_size(0), Ok(GIB));
    assert_eq!(round_up_size(1), Ok(4096));
    assert_eq!(round_up_size(4096), Ok(4096));
    assert_eq!(round_up_size(4097), Ok(8192));
    assert!(round_up_size(-1).is_err());
    assert!(round_up_size(i64::MAX).is_err());
    assert_eq!(round_up_size(MAX_SIZE_BYTES as i64), Ok(MAX_SIZE_BYTES));
    assert!(round_up_size(MAX_SIZE_BYTES as i64 + 1).is_err());
}

#[test]
fn huge_budgets_and_sizes_do_not_overflow() {
    let nodes = cluster(&[u64::MAX, u64::MAX, u64::MAX]);
    let existing = [
        volume(1000, 7000, &["n1", "n2"], u64::MAX),
        volume(1001, 7001, &["n1"], u64::MAX),
    ];
    let mut req = request(2);
    req.size_bytes = u64::MAX;
    let _ = place(&req, &nodes, &existing);
}

/// Deterministic generator: no extra dependencies.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn every_successful_placement_passes_the_crd_validation() {
    let mut rng = Lcg(42);
    let mut succeeded = 0;
    for _ in 0..3000 {
        let count = 2 + (rng.next() % 8) as usize;
        let caps: Vec<u64> = (0..count).map(|_| (rng.next() % 64) * GIB).collect();
        let nodes = cluster(&caps);
        let existing: Vec<Existing> = (0..rng.next() % 6)
            .map(|_| {
                let members: Vec<String> = (0..count)
                    .filter(|_| rng.next().is_multiple_of(2))
                    .map(|i| format!("n{}", i + 1))
                    .collect();
                Existing {
                    minor: 1000 + (rng.next() % 6) as u32,
                    port: 7000 + (rng.next() % 6) as u16,
                    replica_nodes: members,
                    size_bytes: (rng.next() % 32) * GIB,
                }
            })
            .collect();
        let req = Request {
            size_bytes: (1 + rng.next() % 40) * 4096 * 1024,
            replicas: 1 + (rng.next() % 9) as usize,
            protocol: if rng.next().is_multiple_of(2) {
                Protocol::A
            } else {
                Protocol::C
            },
            max_peers: (rng.next() % 40) as u32,
            selected_node: match rng.next() % 3 {
                0 => None,
                1 => Some(format!("n{}", 1 + rng.next() % count as u64)),
                _ => Some("zz".into()),
            },
        };
        if let Ok(spec) = place(&req, &nodes, &existing) {
            succeeded += 1;
            to_volume_spec("pvc-x", &spec).unwrap_or_else(|e| panic!("{e}: {req:?} {spec:?}"));
        }
    }
    assert!(succeeded > 100, "generator produced too few successes: {succeeded}");
}

#[test]
fn existing_of_counts_both_replica_and_retiring() {
    let spec = ReplicatedVolumeSpec {
        size_bytes: 10 * GIB,
        protocol: Protocol::C,
        minor: 1005,
        port: 7005,
        max_peers: 3,
        initial_sync: InitialSync::Done,
        nodes: vec![
            MemberNode {
                name: "a".into(),
                node_id: 0,
                address: "192.0.2.1".into(),
                kind: NodeKind::Replica,
            },
            MemberNode {
                name: "b".into(),
                node_id: 1,
                address: "192.0.2.2".into(),
                kind: NodeKind::Retiring,
            },
            MemberNode {
                name: "c".into(),
                node_id: 2,
                address: "192.0.2.3".into(),
                kind: NodeKind::Witness,
            },
        ],
    };
    let rv = ReplicatedVolume::new("vol-1", spec);
    let existing = existing_of(&rv);
    assert_eq!(existing.minor, 1005);
    assert_eq!(existing.port, 7005);
    assert_eq!(existing.size_bytes, 10 * GIB);
    assert_eq!(existing.replica_nodes, ["a", "b"]);
}

#[test]
fn pick_witness_rules() {
    let nodes = cluster(&[10 * GIB, 20 * GIB, 50 * GIB, 5 * GIB]);
    // n1 (10G), n2 (20G), n3 (50G), n4 (5G)
    let free = free_budgets(&nodes, &[]);

    // 1. Client is picked before a node with a larger budget:
    assert_eq!(pick_witness(&nodes, &free, &["n1"], &["n4"]), Some("n4".into()));

    // Multiple clients: picked in alphabetical order:
    assert_eq!(pick_witness(&nodes, &free, &[], &["n4", "n2"]), Some("n2".into()));

    // 2. Client in exclude is not picked:
    assert_eq!(pick_witness(&nodes, &free, &["n1", "n4"], &["n4"]), Some("n3".into()));

    // 3. Without clients: highest free budget:
    assert_eq!(pick_witness(&nodes, &free, &["n1"], &[]), Some("n3".into()));

    // Breaking ties by name ascending:
    let tied_nodes = cluster(&[20 * GIB, 20 * GIB]);
    let tied_free = free_budgets(&tied_nodes, &[]);
    assert_eq!(pick_witness(&tied_nodes, &tied_free, &[], &[]), Some("n1".into()));

    // 4. All nodes excluded -> None:
    assert_eq!(pick_witness(&nodes, &free, &["n1", "n2", "n3", "n4"], &[]), None);
}
