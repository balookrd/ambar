use std::collections::BTreeMap;

use super::*;

fn map_with(key: &str, value: &str) -> ConfigMap {
    ConfigMap {
        data: Some(BTreeMap::from([(key.to_string(), value.to_string())])),
        ..Default::default()
    }
}

const TWO_NODES: &str = r#"[
    {"name":"n1","address":"192.0.2.1","nodeId":0,"capacityBytes":1073741824},
    {"name":"n2","address":"192.0.2.2","nodeId":1,"capacityBytes":1073741824}
]"#;

#[test]
fn the_nodes_key_is_parsed() {
    let nodes = nodes_from_configmap(Some(&map_with("nodes", TWO_NODES))).unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.get("n2").is_some());
}

#[test]
fn a_missing_configmap_is_an_error_naming_it() {
    let err = nodes_from_configmap(None).unwrap_err().to_string();
    assert!(err.contains("ambar-nodes"), "{err}");
}

#[test]
fn a_configmap_without_the_nodes_key_is_an_error_naming_both() {
    let err = nodes_from_configmap(Some(&map_with("other", TWO_NODES)))
        .unwrap_err()
        .to_string();
    assert!(err.contains("ambar-nodes") && err.contains("nodes"), "{err}");
    let err = nodes_from_configmap(Some(&ConfigMap::default()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("ambar-nodes") && err.contains("nodes"), "{err}");
}

#[test]
fn an_invalid_map_is_an_error_naming_the_configmap() {
    let err = nodes_from_configmap(Some(&map_with("nodes", "[]")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("ambar-nodes") && err.contains("empty"), "{err}");
    let err = nodes_from_configmap(Some(&map_with("nodes", "not json")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("ambar-nodes"), "{err}");
}
