use super::*;

const SAMPLE: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

fn sample() -> VolumeSpec {
    VolumeSpec::from_json(SAMPLE).expect("sample is valid")
}

#[test]
fn parses_a_valid_volume() {
    let spec = sample();
    assert_eq!(spec.name, "spike");
    assert_eq!(spec.protocol, Protocol::C);
    assert_eq!(spec.nodes.len(), 3);
    assert_eq!(spec.diskful_nodes().count(), 2);
    assert_eq!(spec.node("lima-dv-3").map(|n| n.diskful), Some(false));
    assert_eq!(spec.node("nosuch"), None);
}

#[test]
fn the_lowest_diskful_node_id_leads_the_initial_sync() {
    let mut spec = sample();
    assert_eq!(spec.initial_sync_leader().map(|n| n.name.as_str()), Some("lima-dv-1"));
    spec.nodes[0].diskful = false;
    spec.nodes[2].diskful = true;
    assert_eq!(spec.initial_sync_leader().map(|n| n.name.as_str()), Some("lima-dv-2"));
}

#[test]
fn rejects_a_single_copy() {
    let mut spec = sample();
    spec.nodes[1].diskful = false;
    assert_eq!(spec.validate(), Err(SpecError::Replicas(1)));
}

#[test]
fn rejects_duplicate_node_ids_and_names() {
    let mut spec = sample();
    spec.nodes[2].node_id = 0;
    assert_eq!(spec.validate(), Err(SpecError::DuplicateNodeId(0)));

    let mut spec = sample();
    spec.nodes[2].name = "lima-dv-1".into();
    assert_eq!(spec.validate(), Err(SpecError::DuplicateName("lima-dv-1".into())));
}

#[test]
fn rejects_out_of_range_values() {
    let mut spec = sample();
    spec.name = "Spike".into();
    assert_eq!(spec.validate(), Err(SpecError::Name("Spike".into())));

    let mut spec = sample();
    spec.size_bytes = 1000;
    assert_eq!(spec.validate(), Err(SpecError::Size(1000)));

    let mut spec = sample();
    spec.minor = MAX_MINOR + 1;
    assert_eq!(spec.validate(), Err(SpecError::Minor(MAX_MINOR + 1)));

    let mut spec = sample();
    spec.port = 80;
    assert_eq!(spec.validate(), Err(SpecError::Port(80)));

    let mut spec = sample();
    spec.nodes[0].node_id = 32;
    assert_eq!(spec.validate(), Err(SpecError::NodeId(32)));
}

#[test]
fn max_peers_must_cover_the_other_nodes() {
    let mut spec = sample();
    spec.max_peers = 1;
    assert_eq!(
        spec.validate(),
        Err(SpecError::MaxPeers {
            max_peers: 1,
            needed: 2
        })
    );
    spec.max_peers = 32;
    assert_eq!(
        spec.validate(),
        Err(SpecError::MaxPeers {
            max_peers: 32,
            needed: 2
        })
    );
}

#[test]
fn rejects_unknown_fields() {
    let json = SAMPLE.replace("\"minor\": 1000,", "\"minor\": 1000, \"mirror\": 1,");
    assert!(matches!(VolumeSpec::from_json(&json), Err(SpecError::Json(_))));
}

fn with(f: impl FnOnce(&mut VolumeSpec)) -> Result<(), SpecError> {
    let mut spec = sample();
    f(&mut spec);
    spec.validate()
}

#[test]
fn size_bytes_boundaries() {
    assert_eq!(with(|s| s.size_bytes = 0), Err(SpecError::Size(0)));
    assert_eq!(with(|s| s.size_bytes = 4096), Ok(()));
    assert_eq!(with(|s| s.size_bytes = 4097), Err(SpecError::Size(4097)));
    assert_eq!(with(|s| s.size_bytes = MAX_SIZE_BYTES), Ok(()));
    let too_big = MAX_SIZE_BYTES + 4096;
    assert_eq!(with(|s| s.size_bytes = too_big), Err(SpecError::Size(too_big)));
}

#[test]
fn port_boundaries() {
    assert_eq!(with(|s| s.port = 1023), Err(SpecError::Port(1023)));
    assert_eq!(with(|s| s.port = 1024), Ok(()));
    assert_eq!(with(|s| s.port = 65535), Ok(()));
}

#[test]
fn minor_boundaries() {
    assert_eq!(with(|s| s.minor = MAX_MINOR), Ok(()));
    assert_eq!(with(|s| s.minor = MAX_MINOR + 1), Err(SpecError::Minor(MAX_MINOR + 1)));
}

#[test]
fn node_id_boundaries() {
    assert_eq!(with(|s| s.nodes[2].node_id = 31), Ok(()));
    assert_eq!(with(|s| s.nodes[2].node_id = 32), Err(SpecError::NodeId(32)));
}

#[test]
fn max_peers_boundaries() {
    let too_low = SpecError::MaxPeers {
        max_peers: 1,
        needed: 2,
    };
    let too_high = SpecError::MaxPeers {
        max_peers: 32,
        needed: 2,
    };
    assert_eq!(with(|s| s.max_peers = 2), Ok(()));
    assert_eq!(with(|s| s.max_peers = 1), Err(too_low));
    assert_eq!(with(|s| s.max_peers = 31), Ok(()));
    assert_eq!(with(|s| s.max_peers = 32), Err(too_high));
}

#[test]
fn replica_count_edges() {
    assert_eq!(with(|s| s.nodes.clear()), Err(SpecError::Replicas(0)));
    assert_eq!(with(|s| s.nodes[1].diskful = false), Err(SpecError::Replicas(1)));
    assert_eq!(
        with(|s| {
            s.nodes.truncate(1);
            s.nodes.push(VolumeNode {
                name: "d1".into(),
                address: "192.168.104.5".parse().unwrap(),
                node_id: 5,
                diskful: false,
            });
        }),
        Err(SpecError::Replicas(1))
    );
}

#[test]
fn volume_name_rules() {
    let long = "a".repeat(64);
    for bad in ["", "-x", long.as_str(), "Spike", "a_b"] {
        assert_eq!(
            with(|s| s.name = bad.into()),
            Err(SpecError::Name(bad.into())),
            "{bad:?}"
        );
    }
    let ok63 = "a".repeat(63);
    for good in ["a", "0", ok63.as_str(), "a-b-0"] {
        assert_eq!(with(|s| s.name = good.into()), Ok(()), "{good:?}");
    }
}

#[test]
fn node_name_rules() {
    let long = "a".repeat(64);
    for bad in [
        "",
        " a",
        "a b",
        "a;b",
        "a{",
        "a}",
        "a\"b",
        "a#b",
        "a\nb",
        "-a",
        ".a",
        long.as_str(),
    ] {
        assert_eq!(
            with(|s| s.nodes[2].name = bad.into()),
            Err(SpecError::NodeName(bad.into())),
            "{bad:?}"
        );
    }
    let ok63 = "a".repeat(63);
    for good in ["k3s-1", "lima-dv-1", "Node_1.example", ok63.as_str()] {
        assert_eq!(with(|s| s.nodes[0].name = good.into()), Ok(()), "{good:?}");
    }
}

#[test]
fn node_name_is_checked_before_duplicates() {
    assert_eq!(
        with(|s| {
            s.nodes[0].name = "a;b".into();
            s.nodes[1].name = "a;b".into();
        }),
        Err(SpecError::NodeName("a;b".into()))
    );
}

#[test]
fn error_messages_are_stable() {
    let cases = [
        (SpecError::Json("x".into()), "invalid volume json: x"),
        (
            SpecError::Name("A".into()),
            "invalid volume name \"A\": use 1-63 characters a-z, 0-9 and '-', starting with a letter or digit",
        ),
        (
            SpecError::NodeName("a;b".into()),
            "invalid node name \"a;b\": use 1-63 characters A-Z, a-z, 0-9, '.', '_' and '-', starting with a letter or digit",
        ),
        (
            SpecError::Size(7),
            "sizeBytes 7 must be a positive multiple of 4096, at most 1125899906842624",
        ),
        (SpecError::Minor(1048576), "minor 1048576 is above 1048575"),
        (SpecError::Port(80), "port 80 is below 1024"),
        (SpecError::DuplicateName("n".into()), "duplicate node name \"n\""),
        (SpecError::DuplicateNodeId(3), "duplicate nodeId 3"),
        (SpecError::NodeId(32), "nodeId 32 is above 31"),
        (SpecError::Replicas(1), "1 nodes hold a copy, at least 2 are required"),
        (
            SpecError::MaxPeers {
                max_peers: 1,
                needed: 2,
            },
            "maxPeers 1 must be between 2 (other nodes of this volume) and 31",
        ),
    ];
    for (err, text) in cases {
        assert_eq!(err.to_string(), text);
    }
}
