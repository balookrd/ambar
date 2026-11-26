//! Pure helpers that turn a DRBD status into the node's entry in
//! `ReplicatedVolume.status`.

use crate::crd::{NodeKind, NodeStatus, PeerStatus, ReplicatedVolumeSpec, ReplicatedVolumeStatus};
use crate::drbd::status::{ConnectionState, DiskState, ResourceStatus, Role};

/// Written where DRBD gives no value: the resource is not configured or has no volume 0.
const UNKNOWN: &str = "Unknown";

/// Entry of this node for status.nodes. `status == None`: the resource is not configured.
pub fn node_status(node: &str, status: Option<&ResourceStatus>, message: &str) -> NodeStatus {
    let Some(st) = status else {
        return NodeStatus {
            name: node.to_string(),
            role: UNKNOWN.to_string(),
            disk: UNKNOWN.to_string(),
            quorum: false,
            connections: Vec::new(),
            message: message.to_string(),
        };
    };
    let mut connections: Vec<PeerStatus> = st
        .connections
        .iter()
        .map(|c| PeerStatus {
            peer: c.name.clone(),
            state: c.connection_state.to_string(),
            peer_disk: c
                .peer_devices
                .iter()
                .find(|p| p.volume == 0)
                .map_or_else(|| UNKNOWN.to_string(), |p| p.peer_disk_state.to_string()),
        })
        .collect();
    connections.sort_by(|a, b| a.peer.cmp(&b.peer));
    NodeStatus {
        name: node.to_string(),
        // The CRD documents Primary, Secondary or Unknown; a role this build
        // does not know is reported as Unknown.
        role: match st.role {
            Role::Other => UNKNOWN.to_string(),
            role => role.to_string(),
        },
        disk: st
            .device()
            .map_or_else(|| UNKNOWN.to_string(), |d| d.disk_state.to_string()),
        quorum: st.quorum(),
        connections,
        message: message.to_string(),
    }
}

/// A volume is usable for mounting on this node: the resource is configured,
/// has quorum, and the local disk or a connected peer's disk is UpToDate.
pub fn usable(status: Option<&ResourceStatus>) -> bool {
    let Some(st) = status else {
        return false;
    };
    if !st.quorum() {
        return false;
    }
    let local_up_to_date = st.device().is_some_and(|d| d.disk_state == DiskState::UpToDate);
    let peer_up_to_date = st.connections.iter().any(|c| {
        c.connection_state == ConnectionState::Connected
            && c.peer_devices
                .iter()
                .any(|p| p.volume == 0 && p.peer_disk_state == DiskState::UpToDate)
    });
    local_up_to_date || peer_up_to_date
}

/// Every Replica node has reported disk UpToDate (CreateVolume waits for this).
/// A spec without a Replica never qualifies.
pub fn all_replicas_up_to_date(spec: &ReplicatedVolumeSpec, status: &ReplicatedVolumeStatus) -> bool {
    let mut replicas = spec.nodes.iter().filter(|n| n.kind == NodeKind::Replica).peekable();
    replicas.peek().is_some()
        && replicas.all(|replica| {
            status
                .nodes
                .iter()
                .any(|n| n.name == replica.name && n.disk == DiskState::UpToDate.as_str())
        })
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
