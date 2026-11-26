//! Pure membership rules of a `ReplicatedVolume`: a node joins as a diskless
//! `Client` when a pod lands on it and leaves when the pod goes; names of the
//! per-replica finalizers; volumes that collide on DRBD minor or port.
//! No Kubernetes API calls and no I/O.

use std::net::IpAddr;

use crate::config::ClusterNodes;
use crate::crd::{MemberNode, NodeKind, ReplicatedVolumeSpec};

/// Finalizer name of a Replica node is this prefix plus the node name.
/// The part after `/` is `model::REPLICA_FINALIZER_NAME_PREFIX` + node.
pub const FINALIZER_PREFIX: &str = "ambar.balookrd.github.io/replica-";

pub fn replica_finalizer(node: &str) -> String {
    format!("{FINALIZER_PREFIX}{node}")
}

/// Finalizers a new object must carry: one per Replica node.
pub fn initial_finalizers(spec: &ReplicatedVolumeSpec) -> Vec<String> {
    spec.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Replica)
        .map(|n| replica_finalizer(&n.name))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    AlreadyMember(NodeKind),
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MembershipError {
    #[error("node {0:?} is not in the ambar-nodes ConfigMap")]
    UnknownNode(String),
    #[error("the volume has no room for another node: maxPeers is {max_peers}")]
    NoRoom { max_peers: u32 },
    #[error("nodeId {node_id} of node {node:?} is already used by {by:?}")]
    NodeIdTaken { node: String, node_id: u32, by: String },
    #[error("address {address} of node {node:?} is already used by {by:?}")]
    AddressTaken { node: String, address: String, by: String },
}

/// Adds `node` as a Client. Errors: the node is not in the ConfigMap; no room
/// within `maxPeers`; its `nodeId` or address is used by another member. The
/// list stays sorted by name; on error the spec is unchanged.
pub fn join_as_client(
    spec: &mut ReplicatedVolumeSpec,
    node: &str,
    nodes: &ClusterNodes,
) -> Result<Join, MembershipError> {
    if let Some(existing) = spec.nodes.iter().find(|n| n.name == node) {
        return Ok(Join::AlreadyMember(existing.kind));
    }
    let cluster = nodes
        .get(node)
        .ok_or_else(|| MembershipError::UnknownNode(node.to_string()))?;
    if spec.nodes.len() > spec.max_peers as usize {
        return Err(MembershipError::NoRoom {
            max_peers: spec.max_peers,
        });
    }
    if let Some(other) = spec.nodes.iter().find(|n| n.node_id == cluster.node_id) {
        return Err(MembershipError::NodeIdTaken {
            node: node.to_string(),
            node_id: cluster.node_id,
            by: other.name.clone(),
        });
    }
    let address = cluster.address.to_string();
    if let Some(other) = spec.nodes.iter().find(|n| same_address(&n.address, cluster.address)) {
        return Err(MembershipError::AddressTaken {
            node: node.to_string(),
            address,
            by: other.name.clone(),
        });
    }
    spec.nodes.push(MemberNode {
        name: node.to_string(),
        node_id: cluster.node_id,
        address,
        kind: NodeKind::Client,
    });
    spec.nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Join::Added)
}

/// A member address is free text in the CRD; compare it as an IP address when
/// it parses, as text otherwise.
pub(crate) fn same_address(member: &str, address: IpAddr) -> bool {
    match member.parse::<IpAddr>() {
        Ok(parsed) => parsed == address,
        Err(_) => member == address.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leave {
    NotAMember,
    Kept(NodeKind),
    Removed,
}

/// Removes `node` only when it is a Client.
pub fn leave_as_client(spec: &mut ReplicatedVolumeSpec, node: &str) -> Leave {
    let Some(pos) = spec.nodes.iter().position(|n| n.name == node) else {
        return Leave::NotAMember;
    };
    match spec.nodes[pos].kind {
        NodeKind::Client => {
            spec.nodes.remove(pos);
            Leave::Removed
        }
        kind => Leave::Kept(kind),
    }
}

/// Names of other volumes that use the same minor or the same port. Sorted,
/// without duplicates; `name` itself is never reported.
pub fn collisions(name: &str, minor: u32, port: u16, all: &[(String, u32, u16)]) -> Vec<String> {
    let mut found: Vec<String> = all
        .iter()
        .filter(|(other, m, p)| other != name && (*m == minor || *p == port))
        .map(|(other, _, _)| other.clone())
        .collect();
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
#[path = "tests/membership.rs"]
mod tests;
