//! Renders the DRBD resource file of a volume.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::Path;

use crate::model::{Protocol, VolumeNode, VolumeSpec};

/// Checks whether a block device path is safe to embed in a DRBD resource file
/// without risking directive injection or syntax corruption.
pub fn is_safe_backing_device_path(path: &Path) -> bool {
    let Some(s) = path.to_str() else { return false };
    if !s.starts_with('/') {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.' | b'+'))
}

/// Renders `<volume>.res`. The same text is written on every node of the
/// volume; DRBD picks the `on <host>` section that matches `uname -n`.
///
/// `backing_device` is the stable path of the loop device on a node that
/// holds a copy, e.g. `/dev/ambar/<volume>`.
pub fn render(spec: &VolumeSpec, backing_device: &Path) -> String {
    assert!(
        is_safe_backing_device_path(backing_device),
        "unsafe backing device path: {:?}",
        backing_device
    );
    let mut nodes: Vec<&VolumeNode> = spec.nodes.iter().collect();
    nodes.sort_by_key(|n| n.node_id);

    let protocol = match spec.protocol {
        Protocol::A => "A",
        Protocol::C => "C",
    };

    let mut out = String::new();
    let _ = writeln!(out, "resource {} {{", spec.name);
    // `on-no-quorum io-error` is spelled out on purpose: DRBD 9.3.4 defaults
    // to `suspend-io`, which leaves the device open on an isolated node.
    let _ = writeln!(
        out,
        "    options {{ quorum majority; on-no-quorum io-error; auto-promote yes; }}"
    );
    let _ = writeln!(
        out,
        "    net {{ protocol {protocol}; max-buffers 8192; sndbuf-size 0; rcvbuf-size 0; }}"
    );
    for node in &nodes {
        let disk = if node.diskful {
            format!("disk {}; meta-disk internal;", backing_device.display())
        } else {
            "disk none;".to_string()
        };
        let _ = writeln!(out, "    on {} {{", node.name);
        let _ = writeln!(out, "        node-id {};", node.node_id);
        let _ = writeln!(out, "        address {};", address(node.address, spec.port));
        let _ = writeln!(out, "        volume 0 {{ device minor {}; {disk} }}", spec.minor);
        let _ = writeln!(out, "    }}");
    }
    let hosts: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    let _ = writeln!(out, "    connection-mesh {{ hosts {}; }}", hosts.join(" "));
    let _ = writeln!(out, "}}");
    out
}

fn address(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("ipv4 {v4}:{port}"),
        IpAddr::V6(v6) => format!("ipv6 [{v6}]:{port}"),
    }
}

#[cfg(test)]
#[path = "tests/resource.rs"]
mod tests;
