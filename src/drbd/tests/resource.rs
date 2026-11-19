use std::path::Path;

use super::*;
use crate::model::VolumeSpec;

const TWO_COPIES_ONE_DISKLESS: &str = r#"{
  "name": "spike2",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1001,
  "port": 7001,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false},
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true}
  ]
}"#;

#[test]
fn renders_two_copies_and_a_diskless_node_sorted_by_node_id() {
    let spec = VolumeSpec::from_json(TWO_COPIES_ONE_DISKLESS).unwrap();
    let expected = "\
resource spike2 {
    options { quorum majority; on-no-quorum io-error; auto-promote yes; }
    net { protocol C; }
    on lima-dv-1 {
        node-id 0;
        address ipv4 192.168.104.1:7001;
        volume 0 { device minor 1001; disk /dev/ambar/spike2; meta-disk internal; }
    }
    on lima-dv-2 {
        node-id 1;
        address ipv4 192.168.104.3:7001;
        volume 0 { device minor 1001; disk /dev/ambar/spike2; meta-disk internal; }
    }
    on lima-dv-3 {
        node-id 2;
        address ipv4 192.168.104.4:7001;
        volume 0 { device minor 1001; disk none; }
    }
    connection-mesh { hosts lima-dv-1 lima-dv-2 lima-dv-3; }
}
";
    assert_eq!(render(&spec, Path::new("/dev/ambar/spike2")), expected);
}

#[test]
fn renders_protocol_a_and_ipv6_addresses() {
    let json = TWO_COPIES_ONE_DISKLESS
        .replace("\"protocol\": \"C\"", "\"protocol\": \"A\"")
        .replace("192.168.104.1", "fd00::1");
    let spec = VolumeSpec::from_json(&json).unwrap();
    let text = render(&spec, Path::new("/dev/ambar/spike2"));
    assert!(text.contains("    net { protocol A; }\n"), "{text}");
    assert!(text.contains("        address ipv6 [fd00::1]:7001;\n"), "{text}");
}

#[test]
fn the_quorum_policy_is_always_explicit() {
    let spec = VolumeSpec::from_json(TWO_COPIES_ONE_DISKLESS).unwrap();
    let text = render(&spec, Path::new("/dev/ambar/spike2"));
    assert!(text.contains("quorum majority; on-no-quorum io-error;"), "{text}");
}

#[test]
fn safe_backing_device_path_validation() {
    assert!(is_safe_backing_device_path(Path::new("/dev/ambar/spike2")));
    assert!(is_safe_backing_device_path(Path::new("/dev/k3s-vg/pvc-123+4")));
    assert!(!is_safe_backing_device_path(Path::new("relative/path")));
    assert!(!is_safe_backing_device_path(Path::new("/dev/vg/evil; injection")));
    assert!(!is_safe_backing_device_path(Path::new("/dev/vg/evil\nnewline")));
    assert!(!is_safe_backing_device_path(Path::new("/dev/vg/evil{brace}")));
    assert!(!is_safe_backing_device_path(Path::new("/dev/vg/evil\"quote")));
}

#[test]
#[should_panic(expected = "unsafe backing device path")]
fn render_panics_on_unsafe_backing_device_path() {
    let spec = VolumeSpec::from_json(TWO_COPIES_ONE_DISKLESS).unwrap();
    render(&spec, Path::new("/dev/ambar/evil; injection"));
}
