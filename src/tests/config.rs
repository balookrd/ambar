use super::*;

const SAMPLE: &str = r#"[
  {"name": "node-c", "address": "192.168.104.4", "nodeId": 2, "capacityBytes": 3221225472},
  {"name": "node-a", "address": "192.168.104.1", "nodeId": 0, "capacityBytes": 1073741824},
  {"name": "node-b", "address": "192.168.104.3", "nodeId": 1, "capacityBytes": 2147483648}
]"#;

fn one(name: &str, address: &str, node_id: u32) -> String {
    format!(r#"{{"name": "{name}", "address": "{address}", "nodeId": {node_id}, "capacityBytes": 4096}}"#)
}

fn list(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

#[test]
fn parses_a_valid_list_of_three_nodes() {
    let nodes = ClusterNodes::from_json(SAMPLE).expect("sample is valid");
    assert_eq!(nodes.len(), 3);
    let b = nodes.get("node-b").expect("node-b present");
    assert_eq!(b.address, "192.168.104.3".parse::<IpAddr>().unwrap());
    assert_eq!(b.node_id, 1);
    assert_eq!(b.capacity_bytes, 2147483648);
    assert_eq!(nodes.get("nosuch"), None);
}

#[test]
fn iterates_sorted_by_name_whatever_the_input_order() {
    let nodes = ClusterNodes::from_json(SAMPLE).unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["node-a", "node-b", "node-c"]);
}

#[test]
fn accepts_ipv6_addresses() {
    let json = list(&[one("a", "2001:db8::1", 0), one("b", "192.0.2.2", 1)]);
    assert_eq!(ClusterNodes::from_json(&json).unwrap().len(), 2);
}

#[test]
fn rejects_malformed_json() {
    assert!(matches!(ClusterNodes::from_json("{"), Err(ConfigError::Json(_))));
    assert!(matches!(
        ClusterNodes::from_json(r#"{"nodes": []}"#),
        Err(ConfigError::Json(_))
    ));
}

#[test]
fn rejects_a_bad_address() {
    let json = list(&[one("a", "not-an-ip", 0)]);
    assert!(matches!(ClusterNodes::from_json(&json), Err(ConfigError::Json(_))));
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"[{"name": "a", "address": "192.0.2.1", "nodeId": 0,
        "capacityBytes": 4096, "zone": "x"}]"#;
    assert!(matches!(ClusterNodes::from_json(json), Err(ConfigError::Json(_))));
}

#[test]
fn rejects_missing_fields() {
    let json = r#"[{"name": "a", "address": "192.0.2.1", "nodeId": 0}]"#;
    assert!(matches!(ClusterNodes::from_json(json), Err(ConfigError::Json(_))));
}

#[test]
fn rejects_an_empty_list() {
    assert_eq!(ClusterNodes::from_json("[]"), Err(ConfigError::Empty));
}

#[test]
fn rejects_an_invalid_node_name() {
    for bad in ["", "-a", "a b", "a;b", "a/b", &"x".repeat(64)] {
        let json = list(&[one(bad, "192.0.2.1", 0)]);
        assert_eq!(
            ClusterNodes::from_json(&json),
            Err(ConfigError::NodeName(bad.to_string())),
            "name {bad:?}"
        );
    }
}

#[test]
fn rejects_duplicate_names() {
    let json = list(&[one("a", "192.0.2.1", 0), one("a", "192.0.2.2", 1)]);
    assert_eq!(
        ClusterNodes::from_json(&json),
        Err(ConfigError::DuplicateName("a".into()))
    );
}

#[test]
fn rejects_a_node_id_above_the_limit() {
    let json = list(&[one("a", "192.0.2.1", MAX_NODE_ID + 1)]);
    assert_eq!(
        ClusterNodes::from_json(&json),
        Err(ConfigError::NodeId(MAX_NODE_ID + 1))
    );
    let json = list(&[one("a", "192.0.2.1", MAX_NODE_ID)]);
    assert!(ClusterNodes::from_json(&json).is_ok());
}

#[test]
fn rejects_duplicate_node_ids() {
    let json = list(&[one("a", "192.0.2.1", 5), one("b", "192.0.2.2", 5)]);
    assert_eq!(ClusterNodes::from_json(&json), Err(ConfigError::DuplicateNodeId(5)));
}

#[test]
fn rejects_duplicate_addresses() {
    let json = list(&[one("a", "192.0.2.1", 0), one("b", "192.0.2.1", 1)]);
    assert_eq!(
        ClusterNodes::from_json(&json),
        Err(ConfigError::DuplicateAddress("192.0.2.1".parse().unwrap()))
    );
}

#[test]
fn compares_addresses_after_parsing() {
    let json = list(&[one("a", "2001:db8::1", 0), one("b", "2001:0DB8:0:0:0:0:0:1", 1)]);
    assert_eq!(
        ClusterNodes::from_json(&json),
        Err(ConfigError::DuplicateAddress("2001:db8::1".parse().unwrap()))
    );
}
