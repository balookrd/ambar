use super::*;
use crate::model::MAX_NODE_NAME_LEN;

fn cluster() -> ClusterNodes {
    ClusterNodes::from_json(
        r#"[
  {"name": "n1", "address": "192.0.2.1", "nodeId": 0, "capacityBytes": 4096},
  {"name": "n2", "address": "192.0.2.2", "nodeId": 1, "capacityBytes": 4096},
  {"name": "n3", "address": "192.0.2.3", "nodeId": 2, "capacityBytes": 4096},
  {"name": "n4", "address": "192.0.2.4", "nodeId": 3, "capacityBytes": 4096}
]"#,
    )
    .unwrap()
}

fn member(name: &str, node_id: u32, address: &str, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.into(),
        node_id,
        address: address.into(),
        kind,
    }
}

fn spec(max_peers: u32, nodes: Vec<MemberNode>) -> ReplicatedVolumeSpec {
    ReplicatedVolumeSpec {
        size_bytes: 1 << 30,
        protocol: crate::model::Protocol::C,
        minor: 1,
        port: 7001,
        max_peers,
        initial_sync: Default::default(),
        nodes,
    }
}

/// Two replicas, n1 and n2.
fn base(max_peers: u32) -> ReplicatedVolumeSpec {
    spec(
        max_peers,
        vec![
            member("n1", 0, "192.0.2.1", NodeKind::Replica),
            member("n2", 1, "192.0.2.2", NodeKind::Replica),
        ],
    )
}

fn names(s: &ReplicatedVolumeSpec) -> Vec<&str> {
    s.nodes.iter().map(|n| n.name.as_str()).collect()
}

#[test]
fn finalizer_name_is_prefix_plus_node() {
    assert_eq!(replica_finalizer("n1"), "ambar.balookrd.github.io/replica-n1");
}

#[test]
fn finalizer_of_a_longest_node_name_fits_a_qualified_name() {
    let node = "a".repeat(MAX_NODE_NAME_LEN);
    assert!(crate::model::valid_node_name(&node));
    let finalizer = replica_finalizer(&node);
    let (prefix, part) = finalizer.split_once('/').unwrap();
    assert_eq!(prefix, "ambar.balookrd.github.io");
    assert!(part.len() <= 63, "{} characters", part.len());
    assert_eq!(part.len(), 63);
}

#[test]
fn initial_finalizers_cover_only_replicas() {
    let s = spec(
        3,
        vec![
            member("n1", 0, "192.0.2.1", NodeKind::Replica),
            member("n2", 1, "192.0.2.2", NodeKind::Replica),
            member("n3", 2, "192.0.2.3", NodeKind::Witness),
            member("n4", 3, "192.0.2.4", NodeKind::Client),
        ],
    );
    assert_eq!(
        initial_finalizers(&s),
        vec![
            "ambar.balookrd.github.io/replica-n1".to_string(),
            "ambar.balookrd.github.io/replica-n2".to_string()
        ]
    );
}

#[test]
fn join_adds_a_client_with_id_and_address_from_the_config() {
    let mut s = base(3);
    assert_eq!(join_as_client(&mut s, "n3", &cluster()), Ok(Join::Added));
    assert_eq!(s.nodes[2], member("n3", 2, "192.0.2.3", NodeKind::Client));
}

#[test]
fn join_keeps_the_list_sorted_by_name() {
    let mut s = spec(
        3,
        vec![
            member("n2", 1, "192.0.2.2", NodeKind::Replica),
            member("n4", 3, "192.0.2.4", NodeKind::Replica),
        ],
    );
    join_as_client(&mut s, "n3", &cluster()).unwrap();
    assert_eq!(names(&s), ["n2", "n3", "n4"]);
    join_as_client(&mut s, "n1", &cluster()).unwrap();
    assert_eq!(names(&s), ["n1", "n2", "n3", "n4"]);
}

#[test]
fn join_of_an_existing_member_changes_nothing() {
    for kind in [NodeKind::Replica, NodeKind::Witness, NodeKind::Client] {
        let mut s = base(3);
        s.nodes.push(member("n3", 2, "192.0.2.3", kind));
        let before = s.clone();
        assert_eq!(join_as_client(&mut s, "n3", &cluster()), Ok(Join::AlreadyMember(kind)));
        assert_eq!(s, before);
    }
}

#[test]
fn join_of_an_existing_member_wins_over_a_full_list() {
    let mut s = base(1);
    assert_eq!(
        join_as_client(&mut s, "n1", &cluster()),
        Ok(Join::AlreadyMember(NodeKind::Replica))
    );
}

#[test]
fn join_of_an_unknown_node_fails() {
    let mut s = base(3);
    let before = s.clone();
    assert_eq!(
        join_as_client(&mut s, "nosuch", &cluster()),
        Err(MembershipError::UnknownNode("nosuch".into()))
    );
    assert_eq!(s, before);
}

#[test]
fn join_needs_room_within_max_peers() {
    let mut s = base(2);
    s.nodes.push(member("n3", 2, "192.0.2.3", NodeKind::Client));
    let before = s.clone();
    assert_eq!(
        join_as_client(&mut s, "n4", &cluster()),
        Err(MembershipError::NoRoom { max_peers: 2 })
    );
    assert_eq!(s, before);
}

#[test]
fn join_fills_the_list_up_to_max_peers_plus_one() {
    let mut s = base(2);
    assert_eq!(join_as_client(&mut s, "n3", &cluster()), Ok(Join::Added));
    assert_eq!(s.nodes.len(), 3);
}

#[test]
fn join_refuses_a_node_id_used_by_another_member() {
    let mut s = spec(
        3,
        vec![
            member("n1", 0, "192.0.2.1", NodeKind::Replica),
            member("n2", 1, "192.0.2.2", NodeKind::Replica),
            member("old", 2, "192.0.2.50", NodeKind::Client),
        ],
    );
    let before = s.clone();
    assert_eq!(
        join_as_client(&mut s, "n3", &cluster()),
        Err(MembershipError::NodeIdTaken {
            node: "n3".into(),
            node_id: 2,
            by: "old".into()
        })
    );
    assert_eq!(s, before);
}

#[test]
fn join_refuses_an_address_used_by_another_member() {
    let mut s = spec(
        3,
        vec![
            member("n1", 0, "192.0.2.1", NodeKind::Replica),
            member("n2", 1, "192.0.2.2", NodeKind::Replica),
            member("old", 9, "192.0.2.3", NodeKind::Client),
        ],
    );
    let before = s.clone();
    assert_eq!(
        join_as_client(&mut s, "n3", &cluster()),
        Err(MembershipError::AddressTaken {
            node: "n3".into(),
            address: "192.0.2.3".into(),
            by: "old".into()
        })
    );
    assert_eq!(s, before);
}

#[test]
fn join_compares_addresses_as_ip_addresses_not_text() {
    let nodes = ClusterNodes::from_json(
        r#"[
  {"name": "n1", "address": "2001:db8::1", "nodeId": 0, "capacityBytes": 4096},
  {"name": "n3", "address": "2001:db8::3", "nodeId": 2, "capacityBytes": 4096}
]"#,
    )
    .unwrap();
    let mut s = spec(
        3,
        vec![
            member("n1", 0, "2001:db8::1", NodeKind::Replica),
            member("n2", 1, "2001:db8:0:0:0:0:0:3", NodeKind::Replica),
        ],
    );
    assert!(matches!(
        join_as_client(&mut s, "n3", &nodes),
        Err(MembershipError::AddressTaken { .. })
    ));
}

#[test]
fn join_writes_the_canonical_text_of_the_address() {
    let nodes = ClusterNodes::from_json(
        r#"[{"name": "n3", "address": "2001:DB8:0:0:0:0:0:3", "nodeId": 2, "capacityBytes": 4096}]"#,
    )
    .unwrap();
    let mut s = base(3);
    join_as_client(&mut s, "n3", &nodes).unwrap();
    assert_eq!(s.nodes[2].address, "2001:db8::3");
}

#[test]
fn leave_removes_a_client() {
    let mut s = base(3);
    s.nodes.push(member("n3", 2, "192.0.2.3", NodeKind::Client));
    assert_eq!(leave_as_client(&mut s, "n3"), Leave::Removed);
    assert_eq!(names(&s), ["n1", "n2"]);
}

#[test]
fn leave_keeps_replicas_and_witnesses() {
    for kind in [NodeKind::Replica, NodeKind::Witness, NodeKind::Retiring] {
        let mut s = base(3);
        s.nodes.push(member("n3", 2, "192.0.2.3", kind));
        let before = s.clone();
        assert_eq!(leave_as_client(&mut s, "n3"), Leave::Kept(kind));
        assert_eq!(s, before);
    }
}

#[test]
fn leave_of_a_non_member_changes_nothing() {
    let mut s = base(3);
    let before = s.clone();
    assert_eq!(leave_as_client(&mut s, "n3"), Leave::NotAMember);
    assert_eq!(s, before);
}

fn all() -> Vec<(String, u32, u16)> {
    vec![
        ("a".into(), 1, 7001),
        ("b".into(), 2, 7001),
        ("c".into(), 1, 7002),
        ("d".into(), 3, 7003),
    ]
}

#[test]
fn collisions_by_minor() {
    assert_eq!(collisions("x", 3, 9000, &all()), ["d"]);
}

#[test]
fn collisions_by_port() {
    assert_eq!(collisions("x", 9, 7002, &all()), ["c"]);
}

#[test]
fn collisions_by_both_are_sorted_and_not_duplicated() {
    // "a" matches on both minor and port, "b" on port, "c" on minor.
    assert_eq!(collisions("x", 1, 7001, &all()), ["a", "b", "c"]);
    let shuffled = vec![("z".into(), 1, 5), ("m".into(), 1, 5), ("z".into(), 1, 5)];
    assert_eq!(collisions("x", 1, 5, &shuffled), ["m", "z"]);
}

#[test]
fn collisions_none() {
    assert!(collisions("x", 9, 9000, &all()).is_empty());
}

#[test]
fn a_volume_never_collides_with_itself() {
    assert!(collisions("a", 1, 7001, &[("a".into(), 1, 7001)]).is_empty());
    assert_eq!(collisions("a", 1, 7001, &all()), ["b", "c"]);
}
