use kube::CustomResourceExt;
use serde_json::{Value, json};

use super::*;
use crate::model::{InitialSync, Protocol, SpecError};

fn spec_json() -> Value {
    json!({
        "sizeBytes": 1073741824u64,
        "protocol": "C",
        "minor": 1000,
        "port": 7900,
        "maxPeers": 2,
        "initialSync": "Skip",
        "nodes": [
            {"name": "n1", "nodeId": 0, "address": "192.0.2.1", "kind": "Replica"},
            {"name": "n2", "nodeId": 1, "address": "192.0.2.2", "kind": "Replica"},
            {"name": "n3", "nodeId": 2, "address": "192.0.2.3", "kind": "Witness"},
        ],
    })
}

fn spec_with(mutate: impl FnOnce(&mut Value)) -> ReplicatedVolumeSpec {
    let mut value = spec_json();
    mutate(&mut value);
    serde_json::from_value(value).unwrap()
}

#[test]
fn three_nodes_convert_with_diskful_by_kind() {
    let spec = spec_with(|_| {});
    let volume = to_volume_spec("pvc-1", &spec).unwrap();
    assert_eq!(volume.name, "pvc-1");
    assert_eq!(volume.size_bytes, 1073741824);
    assert_eq!(volume.protocol, Protocol::C);
    assert_eq!(volume.minor, 1000);
    assert_eq!(volume.port, 7900);
    assert_eq!(volume.max_peers, 2);
    assert_eq!(volume.initial_sync, InitialSync::Skip);
    let diskful: Vec<bool> = volume.nodes.iter().map(|n| n.diskful).collect();
    assert_eq!(diskful, [true, true, false]);
    assert_eq!(volume.nodes[2].name, "n3");
    assert_eq!(volume.nodes[2].node_id, 2);
    assert_eq!(volume.nodes[2].address.to_string(), "192.0.2.3");
}

#[test]
fn client_is_diskless() {
    let spec = spec_with(|v| {
        v["nodes"].as_array_mut().unwrap().push(json!(
            {"name": "n4", "nodeId": 3, "address": "192.0.2.4", "kind": "Client"}
        ));
        v["maxPeers"] = json!(3);
    });
    let volume = to_volume_spec("pvc-1", &spec).unwrap();
    assert!(!volume.nodes[3].diskful);
}

#[test]
fn bad_address_is_reported_with_node() {
    let spec = spec_with(|v| v["nodes"][1]["address"] = json!("not-an-ip"));
    assert_eq!(
        to_volume_spec("pvc-1", &spec),
        Err(CrdError::Address {
            node: "n2".into(),
            address: "not-an-ip".into()
        })
    );
}

#[test]
fn single_replica_fails_validation() {
    let spec = spec_with(|v| v["nodes"][1]["kind"] = json!("Client"));
    assert_eq!(
        to_volume_spec("pvc-1", &spec),
        Err(CrdError::Spec(SpecError::Replicas(1)))
    );
}

#[test]
fn missing_initial_sync_reads_as_done() {
    let spec = spec_with(|v| {
        v.as_object_mut().unwrap().remove("initialSync");
    });
    assert_eq!(spec.initial_sync, InitialSync::Done);
}

fn generated() -> String {
    format!("{}\n", serde_json::to_string_pretty(&ReplicatedVolume::crd()).unwrap())
}

#[test]
fn committed_crd_equals_generator_output() {
    assert_eq!(include_str!("../../deploy/crd.json"), generated());
}

fn crd_value() -> Value {
    serde_json::to_value(ReplicatedVolume::crd()).unwrap()
}

fn spec_schema(crd: &Value) -> &Value {
    &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]
}

#[test]
fn crd_is_cluster_scoped_with_status_subresource() {
    let crd = crd_value();
    assert_eq!(crd["spec"]["scope"], "Cluster");
    assert_eq!(crd["spec"]["group"], GROUP);
    assert_eq!(crd["spec"]["names"]["shortNames"], json!(["rv"]));
    assert!(crd["spec"]["versions"][0]["subresources"]["status"].is_object());
}

#[test]
fn node_lists_are_maps_keyed_by_name() {
    let crd = crd_value();
    let spec_nodes = &spec_schema(&crd)["properties"]["nodes"];
    let status = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["status"];
    for list in [spec_nodes, &status["properties"]["nodes"]] {
        assert_eq!(list["x-kubernetes-list-type"], "map");
        assert_eq!(list["x-kubernetes-list-map-keys"], json!(["name"]));
    }
}

#[test]
fn spec_carries_four_transition_rules_and_default() {
    let crd = crd_value();
    let spec = spec_schema(&crd);
    let messages: Vec<&str> = spec["x-kubernetes-validations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        [
            "sizeBytes, minor, port, maxPeers and protocol are immutable",
            "the set of Replica and Witness nodes is immutable",
            "a member node cannot change nodeId, address or kind",
            "initialSync cannot go back to Skip",
        ]
    );
    assert_eq!(spec["properties"]["initialSync"]["default"], "Done");
}
