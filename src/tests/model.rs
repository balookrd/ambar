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
