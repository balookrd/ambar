//! Kubernetes view of a replicated volume: the `ReplicatedVolume` custom
//! resource and its conversion to the description the node agent works with.

use std::net::IpAddr;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{InitialSync, Protocol, SpecError, VolumeNode, VolumeSpec};

pub const GROUP: &str = "ambar.balookrd.github.io";
pub const DRIVER_NAME: &str = "ambar.balookrd.github.io";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NodeKind {
    /// Holds a copy of the data.
    Replica,
    /// Diskless; only takes part in the quorum.
    Witness,
    /// Diskless; attaches the volume to run a workload.
    Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberNode {
    /// Kubernetes node name, equal to `uname -n` of the node.
    #[schemars(length(max = 63))]
    pub name: String,
    pub node_id: u32,
    /// Address on the replication network; parsed as an IP address by the agent.
    #[schemars(length(max = 45))]
    pub address: String,
    pub kind: NodeKind,
}

// Transition rules (`oldSelf`) run on update only. The set of Replica and
// Witness members is compared in both directions plus by size, so that
// swapping one member for another in a single update is rejected too.
#[derive(CustomResource, Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "ambar.balookrd.github.io",
    version = "v1alpha1",
    kind = "ReplicatedVolume",
    status = "ReplicatedVolumeStatus",
    shortname = "rv"
)]
#[serde(rename_all = "camelCase")]
#[schemars(extend("x-kubernetes-validations" = [
    {
        "rule": "self.sizeBytes == oldSelf.sizeBytes && self.minor == oldSelf.minor && self.port == oldSelf.port && self.maxPeers == oldSelf.maxPeers && self.protocol == oldSelf.protocol",
        "message": "sizeBytes, minor, port, maxPeers and protocol are immutable"
    },
    {
        "rule": "self.nodes.filter(n, n.kind != 'Client').size() == oldSelf.nodes.filter(o, o.kind != 'Client').size() && self.nodes.filter(n, n.kind != 'Client').all(n, oldSelf.nodes.exists(o, o.name == n.name && o.nodeId == n.nodeId && o.address == n.address && o.kind == n.kind)) && oldSelf.nodes.filter(o, o.kind != 'Client').all(o, self.nodes.exists(n, o.name == n.name && o.nodeId == n.nodeId && o.address == n.address && o.kind == n.kind))",
        "message": "the set of Replica and Witness nodes is immutable"
    },
    {
        "rule": "self.nodes.all(n, oldSelf.nodes.all(o, o.name != n.name || (o.nodeId == n.nodeId && o.address == n.address && o.kind == n.kind)))",
        "message": "a member node cannot change nodeId, address or kind"
    },
    {
        "rule": "!(oldSelf.initialSync == 'Done' && self.initialSync == 'Skip')",
        "message": "initialSync cannot go back to Skip"
    }
]))]
pub struct ReplicatedVolumeSpec {
    pub size_bytes: u64,
    pub protocol: Protocol,
    pub minor: u32,
    pub port: u16,
    pub max_peers: u32,
    #[serde(default)]
    pub initial_sync: InitialSync,
    #[schemars(
        length(max = 32),
        extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"])
    )]
    pub nodes: Vec<MemberNode>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReplicatedVolumeStatus {
    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"]))]
    pub nodes: Vec<NodeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub name: String,
    /// `Primary`, `Secondary` or `Unknown`.
    pub role: String,
    /// DRBD disk state of the local volume 0; `Diskless` for a node without a disk.
    pub disk: String,
    pub quorum: bool,
    pub connections: Vec<PeerStatus>,
    /// Last error of the reconcile on this node; empty when the last pass succeeded.
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus {
    pub peer: String,
    pub state: String,
    pub peer_disk: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CrdError {
    #[error("node {node:?} has the address {address:?}, which is not an IP address")]
    Address { node: String, address: String },
    #[error(transparent)]
    Spec(#[from] SpecError),
}

/// Description the node agent works with. `name` is the object name.
pub fn to_volume_spec(name: &str, spec: &ReplicatedVolumeSpec) -> Result<VolumeSpec, CrdError> {
    let nodes = spec
        .nodes
        .iter()
        .map(|node| {
            let address = node.address.parse::<IpAddr>().map_err(|_| CrdError::Address {
                node: node.name.clone(),
                address: node.address.clone(),
            })?;
            Ok(VolumeNode {
                name: node.name.clone(),
                address,
                node_id: node.node_id,
                diskful: node.kind == NodeKind::Replica,
            })
        })
        .collect::<Result<Vec<_>, CrdError>>()?;
    let volume = VolumeSpec {
        name: name.to_string(),
        size_bytes: spec.size_bytes,
        protocol: spec.protocol,
        minor: spec.minor,
        port: spec.port,
        max_peers: spec.max_peers,
        initial_sync: spec.initial_sync,
        nodes,
    };
    volume.validate()?;
    Ok(volume)
}

#[cfg(test)]
#[path = "tests/crd.rs"]
mod tests;
