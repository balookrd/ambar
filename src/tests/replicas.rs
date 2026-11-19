use super::*;
use crate::config::ClusterNodes;
use crate::crd::{MemberNode, NodeKind, NodeStatus, PeerStatus, ReplicatedVolumeSpec, ReplicatedVolumeStatus};
use crate::membership::replica_finalizer;
use crate::model::InitialSync;
use crate::placement::Existing;

const GIB: u64 = 1024 * 1024 * 1024;

fn test_cluster() -> ClusterNodes {
    ClusterNodes::from_json(
        r#"[
        {"name": "n1", "address": "192.168.104.1", "nodeId": 0, "capacityBytes": 10737418240},
        {"name": "n2", "address": "192.168.104.2", "nodeId": 1, "capacityBytes": 10737418240},
        {"name": "n3", "address": "192.168.104.3", "nodeId": 2, "capacityBytes": 10737418240},
        {"name": "n4", "address": "192.168.104.4", "nodeId": 3, "capacityBytes": 10737418240},
        {"name": "n5", "address": "192.168.104.5", "nodeId": 4, "capacityBytes": 10737418240}
    ]"#,
    )
    .unwrap()
}

fn member(name: &str, node_id: u32, address: &str, kind: NodeKind) -> MemberNode {
    MemberNode {
        name: name.to_string(),
        node_id,
        address: address.to_string(),
        kind,
    }
}

fn healthy_status(spec: &ReplicatedVolumeSpec) -> ReplicatedVolumeStatus {
    let mut nodes = Vec::new();
    for member in &spec.nodes {
        let is_diskful = matches!(member.kind, NodeKind::Replica | NodeKind::Retiring);
        let mut connections = Vec::new();
        for peer in &spec.nodes {
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
        nodes.push(NodeStatus {
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
    ReplicatedVolumeStatus { nodes }
}

fn sample_spec(members: Vec<MemberNode>) -> ReplicatedVolumeSpec {
    ReplicatedVolumeSpec {
        size_bytes: GIB,
        protocol: crate::model::Protocol::C,
        minor: 1000,
        port: 7000,
        max_peers: 7,
        initial_sync: InitialSync::Done,
        nodes: members,
    }
}

// ==========================================
// Step 1: next_add tests
// ==========================================

#[test]
fn add_refuses_when_deleting_or_unknown_node() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];

    // Deleting
    let view_del = View {
        spec: &spec,
        status: Some(&status),
        deleting: true,
        finalizers: &finalizers,
    };
    assert_eq!(next_add(&view_del, "n3", &cluster, &[]), Err(ChangeError::Deleting));

    // Unknown node
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view, "unknown", &cluster, &[]),
        Err(ChangeError::UnknownNode("unknown".to_string()))
    );
}

#[test]
fn add_refuses_before_first_patch_on_unhealthy_state() {
    let cluster = test_cluster();
    let mut spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];

    // initialSync != Done
    spec.initial_sync = InitialSync::Skip;
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view, "n3", &cluster, &[]),
        Err(ChangeError::InitialSyncNotDone("Skip".to_string()))
    );
    spec.initial_sync = InitialSync::Done;

    // Retiring already present
    let mut retiring_spec = spec.clone();
    retiring_spec
        .nodes
        .push(member("n4", 3, "192.168.104.4", NodeKind::Retiring));
    let view_ret = View {
        spec: &retiring_spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_ret, "n3", &cluster, &[]),
        Err(ChangeError::RetiringAlreadyPresent("n4".to_string()))
    );

    // Replica missing status
    let mut missing_status = status.clone();
    missing_status.nodes.retain(|n| n.name != "n2");
    let view_miss = View {
        spec: &spec,
        status: Some(&missing_status),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_miss, "n3", &cluster, &[]),
        Err(ChangeError::ReplicaMissingStatus("n2".to_string()))
    );

    // Replica not UpToDate
    let mut not_uptodate = status.clone();
    not_uptodate.nodes.iter_mut().find(|n| n.name == "n2").unwrap().disk = "Inconsistent".to_string();
    let view_inc = View {
        spec: &spec,
        status: Some(&not_uptodate),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_inc, "n3", &cluster, &[]),
        Err(ChangeError::ReplicaNotUpToDate {
            node: "n2".to_string(),
            disk: "Inconsistent".to_string(),
        })
    );

    // Member (Witness) missing status
    let mut with_witness_spec = spec.clone();
    with_witness_spec
        .nodes
        .push(member("n4", 3, "192.168.104.4", NodeKind::Witness));
    let view_w_miss = View {
        spec: &with_witness_spec,
        status: Some(&status), // only n1 and n2 status
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_w_miss, "n3", &cluster, &[]),
        Err(ChangeError::MemberMissingStatus("n4".to_string()))
    );

    // Peer not Connected
    let mut peer_disc = status.clone();
    peer_disc.nodes[0].connections[0].state = "Connecting".to_string();
    let view_conn = View {
        spec: &spec,
        status: Some(&peer_disc),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_conn, "n3", &cluster, &[]),
        Err(ChangeError::PeerNotConnected {
            from: "n1".to_string(),
            to: "n2".to_string(),
            state: "Connecting".to_string(),
        })
    );

    // Peer disk not UpToDate
    let mut peer_inc = status.clone();
    peer_inc.nodes[0].connections[0].peer_disk = "Inconsistent".to_string();
    let view_p_inc = View {
        spec: &spec,
        status: Some(&peer_inc),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(
        next_add(&view_p_inc, "n3", &cluster, &[]),
        Err(ChangeError::PeerDiskNotUpToDate {
            from: "n1".to_string(),
            to: "n2".to_string(),
            disk: "Inconsistent".to_string(),
        })
    );
}

#[test]
fn add_refuses_when_powered_off_node_has_stale_status() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    // n3 is powered off: n3's own entry says UpToDate and Connected,
    // but n1 and n2 see n3 as Connecting.
    for n in &mut status.nodes {
        if n.name == "n1" || n.name == "n2" {
            for c in &mut n.connections {
                if c.peer == "n3" {
                    c.state = "Connecting".to_string();
                }
            }
        }
    }
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let err = next_add(&view, "n4", &cluster, &[]).unwrap_err();
    match err {
        ChangeError::PeerNotConnected { to, state, .. } => {
            assert_eq!(to, "n3");
            assert_eq!(state, "Connecting");
        }
        other => panic!("expected PeerNotConnected, got {other:?}"),
    }
}

#[test]
fn add_adds_replica_and_keeps_witness_when_n2_with_w() {
    let cluster = test_cluster();
    // N=2 with W: n1, n2, witness n3 -> adding n4 makes N=3.
    // N becomes odd (3), but W is kept on the first patch because N=2 had W!
    // (W will be removed on the second patch when n4 is UpToDate).
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n4", &cluster, &[]).unwrap();
    let expected_nodes = vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
    ];
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: expected_nodes,
            add_finalizer: Some(replica_finalizer("n4")),
        })
    );
}

#[test]
fn add_adds_replica_and_new_witness_when_n3_without_w() {
    let cluster = test_cluster();
    // N=3 without W: n1, n2, n3. Adding n4 makes N=4 (even) -> W needed!
    // Client node n5 exists -> picked as W first.
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n5", 4, "192.168.104.5", NodeKind::Client),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n4", &cluster, &[]).unwrap();
    let expected_nodes = vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
        member("n5", 4, "192.168.104.5", NodeKind::Witness), // Client became Witness!
    ];
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: expected_nodes,
            add_finalizer: Some(replica_finalizer("n4")),
        })
    );
}

#[test]
fn add_respects_max_peers_and_omits_witness_or_errors() {
    let cluster = test_cluster();
    let mut spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];

    // max_peers = 3 -> total members limit = max_peers + 1 = 4.
    // Adding n4 reaches 4. Witness n5 would exceed 4 -> Edit without witness.
    spec.max_peers = 3;
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next = next_add(&view, "n4", &cluster, &[]).unwrap();
    let expected_nodes = vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
    ];
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: expected_nodes,
            add_finalizer: Some(replica_finalizer("n4")),
        })
    );

    // max_peers = 2 -> total members limit = 3. Already 3 members -> NoRoom!
    spec.max_peers = 2;
    let view_noroom = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(next_add(&view_noroom, "n4", &cluster, &[]), Err(ChangeError::NoRoom));
}

#[test]
fn add_converts_witness_to_replica_and_picks_new_witness() {
    let cluster = test_cluster();
    // N=2 with W: n1, n2, witness n3. Target is n3!
    // n3: Witness -> Replica (N becomes 3).
    // R22: witness was present before edit -> pick new witness!
    // Client n4 is available -> picked as new Witness.
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
        member("n4", 3, "192.168.104.4", NodeKind::Client),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n3", &cluster, &[]).unwrap();
    let expected_nodes = vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Witness),
    ];
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: expected_nodes,
            add_finalizer: Some(replica_finalizer("n3")),
        })
    );
}

#[test]
fn add_checks_primary_on_client_node() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Client),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().role = "Primary".to_string();
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // Client in Primary -> error
    assert_eq!(
        next_add(&view, "n3", &cluster, &[]),
        Err(ChangeError::Primary("n3".to_string()))
    );

    // Client in Secondary -> Edit Client -> Replica
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().role = "Secondary".to_string();
    let view_sec = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next = next_add(&view_sec, "n3", &cluster, &[]).unwrap();
    assert!(
        matches!(next, Next::Edit(edit) if edit.nodes.iter().find(|n| n.name == "n3").unwrap().kind == NodeKind::Replica)
    );
}

#[test]
fn add_checks_primary_on_witness_node() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().role = "Primary".to_string();
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // Witness in Primary -> error
    assert_eq!(
        next_add(&view, "n3", &cluster, &[]),
        Err(ChangeError::Primary("n3".to_string()))
    );
}

#[test]
fn add_checks_budget_capacity() {
    let cluster = test_cluster(); // each has 10 GiB
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // Existing volume on n3 consumes 10 GiB -> free on n3 is 0 < 1 GiB needed
    let existing = vec![Existing {
        minor: 1001,
        port: 7001,
        replica_nodes: vec!["n3".to_string()],
        size_bytes: 10 * GIB,
    }];
    assert_eq!(
        next_add(&view, "n3", &cluster, &existing),
        Err(ChangeError::NoCapacity {
            node: "n3".to_string(),
            needed: GIB,
            free: 0,
        })
    );
}

#[test]
fn add_omits_finalizer_when_already_present() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    // Finalizer for n3 is already on the object
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n3", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
            ],
            add_finalizer: None,
        })
    );
}

#[test]
fn add_waits_when_already_replica_and_not_uptodate() {
    let cluster = test_cluster();
    // n3 is already a Replica in spec.nodes, but its status is Inconsistent
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().disk = "Inconsistent".to_string();
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n3", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Wait("node \"n3\" is Inconsistent; waiting for UpToDate".to_string())
    );
}

#[test]
fn add_removes_witness_on_second_patch_when_n_is_odd() {
    let cluster = test_cluster();
    // n3 is Replica and UpToDate. N=3 (odd), but Witness n4 is still present.
    // Must generate Edit to remove witness!
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Witness),
    ]);
    let mut status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_add(&view, "n3", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
            ],
            add_finalizer: None,
        })
    );

    // If witness n4 is Primary, it must become Client!
    status.nodes.iter_mut().find(|n| n.name == "n4").unwrap().role = "Primary".to_string();
    let view_pri = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_pri = next_add(&view_pri, "n3", &cluster, &[]).unwrap();
    assert_eq!(
        next_pri,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
                member("n4", 3, "192.168.104.4", NodeKind::Client),
            ],
            add_finalizer: None,
        })
    );
}

#[test]
fn add_done_when_already_replica_uptodate_and_witness_rule_satisfied() {
    let cluster = test_cluster();
    // N=3 without witness -> witness rule satisfied -> Done!
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    assert_eq!(next_add(&view, "n3", &cluster, &[]), Ok(Next::Done));
}

// ==========================================
// Step 2: next_remove tests
// ==========================================

#[test]
fn remove_refuses_when_not_a_member_and_no_finalizer() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    assert_eq!(
        next_remove(&view, "n4", false, &cluster, &[]),
        Err(ChangeError::NotAMember("n4".to_string()))
    );
}

#[test]
fn remove_refuses_when_too_few_replicas() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    assert_eq!(
        next_remove(&view, "n2", false, &cluster, &[]),
        Err(ChangeError::TooFewReplicas)
    );
}

#[test]
fn remove_refuses_when_removing_node_is_primary() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().role = "Primary".to_string();
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    assert_eq!(
        next_remove(&view, "n3", false, &cluster, &[]),
        Err(ChangeError::Primary("n3".to_string()))
    );
}

#[test]
fn remove_allows_removing_unsynced_replica() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    // n3 is Inconsistent, but it's the node being removed!
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().disk = "Inconsistent".to_string();
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // N=3 -> N=2 (even) -> witness needed, n4 picked!
    let next = next_remove(&view, "n3", false, &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Retiring),
                member("n4", 3, "192.168.104.4", NodeKind::Witness),
            ],
            add_finalizer: None,
        })
    );
}

#[test]
fn remove_sets_retiring_and_picks_witness_when_n3_to_2() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_remove(&view, "n3", false, &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Retiring),
                member("n4", 3, "192.168.104.4", NodeKind::Witness),
            ],
            add_finalizer: None,
        })
    );

    // If only 3 nodes exist in cluster (no witness candidate): Edit without witness
    let cluster_3 = ClusterNodes::from_json(
        r#"[
        {"name": "n1", "address": "192.168.104.1", "nodeId": 0, "capacityBytes": 10737418240},
        {"name": "n2", "address": "192.168.104.2", "nodeId": 1, "capacityBytes": 10737418240},
        {"name": "n3", "address": "192.168.104.3", "nodeId": 2, "capacityBytes": 10737418240}
    ]"#,
    )
    .unwrap();
    let next_3 = next_remove(&view, "n3", false, &cluster_3, &[]).unwrap();
    assert_eq!(
        next_3,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Retiring),
            ],
            add_finalizer: None,
        })
    );
}

#[test]
fn remove_sets_retiring_and_keeps_witness_when_n4_with_w_to_3() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
        member("n5", 4, "192.168.104.5", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
        replica_finalizer("n4"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_remove(&view, "n4", false, &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
                member("n4", 3, "192.168.104.4", NodeKind::Retiring),
                member("n5", 4, "192.168.104.5", NodeKind::Witness),
            ],
            add_finalizer: None,
        })
    );
}

#[test]
fn remove_waits_while_retiring_or_finalizer_present() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Retiring),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().message = "peer n1 is not connected".to_string();
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n3"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // Node is Retiring -> Wait with message
    let next = next_remove(&view, "n3", false, &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Wait("waiting for node \"n3\" to remove its copy: peer n1 is not connected".to_string())
    );

    // Node gone from spec.nodes, but finalizer still on object -> Wait
    let spec_gone = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let view_gone = View {
        spec: &spec_gone,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_gone = next_remove(&view_gone, "n3", false, &cluster, &[]).unwrap();
    assert_eq!(
        next_gone,
        Next::Wait("waiting for node \"n3\" to remove its copy".to_string())
    );
}

#[test]
fn remove_adjusts_witness_or_completes_when_node_and_finalizer_are_gone() {
    let cluster = test_cluster();
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];

    // N=3 with W -> N is odd, remove witness!
    let spec_w = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
        member("n5", 4, "192.168.104.5", NodeKind::Witness),
    ]);
    let status_w = healthy_status(&spec_w);
    let view_w = View {
        spec: &spec_w,
        status: Some(&status_w),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_w = next_remove(&view_w, "n4", true, &cluster, &[]).unwrap();
    assert_eq!(
        next_w,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
            ],
            add_finalizer: None,
        })
    );

    // N=2 without W -> N is even, assign witness (removed node n3 is candidate now!)
    let spec_2 = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let cluster_3 = ClusterNodes::from_json(
        r#"[
        {"name": "n1", "address": "192.168.104.1", "nodeId": 0, "capacityBytes": 10737418240},
        {"name": "n2", "address": "192.168.104.2", "nodeId": 1, "capacityBytes": 10737418240},
        {"name": "n3", "address": "192.168.104.3", "nodeId": 2, "capacityBytes": 10737418240}
    ]"#,
    )
    .unwrap();
    let status_2 = healthy_status(&spec_2);
    let view_2 = View {
        spec: &spec_2,
        status: Some(&status_2),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_2 = next_remove(&view_2, "n3", true, &cluster_3, &[]).unwrap();
    assert_eq!(
        next_2,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Witness),
            ],
            add_finalizer: None,
        })
    );

    // Witness rule already satisfied (N=2 with witness n3) -> Done!
    let spec_done = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
    ]);
    let status_done = healthy_status(&spec_done);
    let view_done = View {
        spec: &spec_done,
        status: Some(&status_done),
        deleting: false,
        finalizers: &finalizers,
    };
    assert_eq!(next_remove(&view_done, "n4", true, &cluster, &[]), Ok(Next::Done));
}

// ==========================================
// Step 3: next_move tests
// ==========================================

#[test]
fn move_checks_same_node_and_not_a_member() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n4", 3, "192.168.104.4", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    // from == to -> SameNode
    assert_eq!(next_move(&view, "n1", "n1", &cluster, &[]), Err(ChangeError::SameNode));

    // unknown node -> UnknownNode
    assert_eq!(
        next_move(&view, "n1", "n6", &cluster, &[]),
        Err(ChangeError::UnknownNode("n6".to_string()))
    );

    // from not a member and no finalizer, to not replica -> NotAMember
    assert_eq!(
        next_move(&view, "n3", "n4", &cluster, &[]),
        Err(ChangeError::NotAMember("n3".to_string()))
    );

    // from not a member, but to is Replica and UpToDate, and witness present -> Done (already finished)
    assert_eq!(next_move(&view, "n3", "n1", &cluster, &[]), Ok(Next::Done));
}

#[test]
fn move_refuses_when_source_is_primary_before_first_patch() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n1").unwrap().role = "Primary".to_string();
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    assert_eq!(
        next_move(&view, "n1", "n3", &cluster, &[]),
        Err(ChangeError::Primary("n1".to_string()))
    );
}

#[test]
fn move_adds_destination_replica_and_keeps_witness() {
    let cluster = test_cluster();
    // N=2 with W: n1, n2, witness n3. Move n2 -> n4.
    // Destination n4 added as Replica, witness n3 stays!
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_move(&view, "n2", "n4", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Witness),
                member("n4", 3, "192.168.104.4", NodeKind::Replica),
            ],
            add_finalizer: Some(replica_finalizer("n4")),
        })
    );
}

#[test]
fn move_when_destination_is_witness_picks_new_witness() {
    let cluster = test_cluster();
    // Move n2 -> n3, where n3 is Witness!
    // n3: Witness -> Replica. New witness picked from remaining (n4, excluding n2 and n3)!
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_move(&view, "n2", "n3", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Replica),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
                member("n4", 3, "192.168.104.4", NodeKind::Witness),
            ],
            add_finalizer: Some(replica_finalizer("n3")),
        })
    );
}

#[test]
fn move_waits_for_destination_uptodate_then_retires_source() {
    let cluster = test_cluster();
    // n4 added as Replica, but not UpToDate yet -> Wait
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n4").unwrap().disk = "Inconsistent".to_string();
    let finalizers = vec![
        replica_finalizer("n1"),
        replica_finalizer("n2"),
        replica_finalizer("n4"),
    ];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };

    let next = next_move(&view, "n2", "n4", &cluster, &[]).unwrap();
    assert_eq!(
        next,
        Next::Wait("node \"n4\" is Inconsistent; waiting for UpToDate".to_string())
    );

    // Once n4 is UpToDate -> second patch: source n2 becomes Retiring! Witness n3 unchanged.
    status.nodes.iter_mut().find(|n| n.name == "n4").unwrap().disk = "UpToDate".to_string();
    let view_up = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_up = next_move(&view_up, "n2", "n4", &cluster, &[]).unwrap();
    assert_eq!(
        next_up,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Retiring),
                member("n3", 2, "192.168.104.3", NodeKind::Witness),
                member("n4", 3, "192.168.104.4", NodeKind::Replica),
            ],
            add_finalizer: None,
        })
    );

    // Source n2 is now Retiring -> Wait
    let spec_ret = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Retiring),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
    ]);
    let view_ret = View {
        spec: &spec_ret,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    let next_ret = next_move(&view_ret, "n2", "n4", &cluster, &[]).unwrap();
    assert!(matches!(next_ret, Next::Wait(msg) if msg.contains("waiting for node \"n2\" to remove its copy")));

    // Source n2 gone from spec.nodes, finalizer removed -> Done!
    let spec_done = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Witness),
        member("n4", 3, "192.168.104.4", NodeKind::Replica),
    ]);
    let finalizers_done = vec![replica_finalizer("n1"), replica_finalizer("n4")];
    let view_done = View {
        spec: &spec_done,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers_done,
    };
    assert_eq!(next_move(&view_done, "n2", "n4", &cluster, &[]), Ok(Next::Done));
}

#[test]
fn move_refuses_when_volume_is_deleting() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: true,
        finalizers: &finalizers,
    };
    assert_eq!(next_move(&view, "n1", "n3", &cluster, &[]), Err(ChangeError::Deleting));
}

#[test]
fn move_refuses_when_from_is_not_replica() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Witness),
    ]);
    let status = healthy_status(&spec);
    let finalizers = vec![replica_finalizer("n1")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    // n2 is a Witness, cannot move replica from a witness node!
    assert_eq!(
        next_move(&view, "n2", "n3", &cluster, &[]),
        Err(ChangeError::NotAMember("n2".to_string()))
    );
}

#[test]
fn move_refuses_when_to_is_primary() {
    let cluster = test_cluster();
    let spec = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n2", 1, "192.168.104.2", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Client),
    ]);
    let mut status = healthy_status(&spec);
    status.nodes.iter_mut().find(|n| n.name == "n3").unwrap().role = "Primary".to_string();
    let finalizers = vec![replica_finalizer("n1"), replica_finalizer("n2")];
    let view = View {
        spec: &spec,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers,
    };
    // Destination n3 is Primary -> error
    assert_eq!(
        next_move(&view, "n2", "n3", &cluster, &[]),
        Err(ChangeError::Primary("n3".to_string()))
    );
}

#[test]
fn move_in_three_node_cluster_assigns_source_as_witness_when_finished() {
    // 3-node cluster: n1, n2, n3
    let cluster_3 = ClusterNodes::from_json(
        r#"[
        {"name":"n1","address":"192.168.104.1","nodeId":0,"capacityBytes":10737418240},
        {"name":"n2","address":"192.168.104.2","nodeId":1,"capacityBytes":10737418240},
        {"name":"n3","address":"192.168.104.3","nodeId":2,"capacityBytes":10737418240}
    ]"#,
    )
    .unwrap();

    // Volume has n1 (Replica) and n3 (Replica). n2 was removed.
    // N=2 replicas in a 3-node cluster: even N requires Witness!
    // n2 must be assigned as Witness before returning Done!
    let spec_done = sample_spec(vec![
        member("n1", 0, "192.168.104.1", NodeKind::Replica),
        member("n3", 2, "192.168.104.3", NodeKind::Replica),
    ]);
    let status = healthy_status(&spec_done);
    let finalizers_done = vec![replica_finalizer("n1"), replica_finalizer("n3")];
    let view_done = View {
        spec: &spec_done,
        status: Some(&status),
        deleting: false,
        finalizers: &finalizers_done,
    };

    let next = next_move(&view_done, "n2", "n3", &cluster_3, &[]).unwrap();
    assert_eq!(
        next,
        Next::Edit(Edit {
            nodes: vec![
                member("n1", 0, "192.168.104.1", NodeKind::Replica),
                member("n2", 1, "192.168.104.2", NodeKind::Witness),
                member("n3", 2, "192.168.104.3", NodeKind::Replica),
            ],
            add_finalizer: None,
        })
    );
}
