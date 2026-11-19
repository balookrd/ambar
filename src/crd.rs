//! Kubernetes view of a replicated volume: the `ReplicatedVolume` custom
//! resource and its conversion to the description the node agent works with.

use std::net::IpAddr;

use kube::{CustomResource, CustomResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{
    InitialSync, MAX_MINOR, MAX_NODE_ID, MAX_NODE_NAME_LEN, MAX_SIZE_BYTES, Protocol, SpecError, VolumeNode, VolumeSpec,
};

pub const GROUP: &str = "ambar.balookrd.github.io";
pub const DRIVER_NAME: &str = "ambar.balookrd.github.io";

/// Longest textual IPv6 address (`ffff:ffff:ffff:ffff:ffff:ffff:255.255.255.255`).
/// Cap for `address`: CEL cost estimation needs bounded strings.
pub const MAX_ADDRESS_LEN: usize = 45;

/// The generated CRD as committed in `deploy/crd.json`: pretty JSON, trailing newline.
pub fn crd_json() -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&ReplicatedVolume::crd()).expect("CRD serializes")
    )
}

/// The API server does not know schemars' `uint16`/`uint32`/`uint64` formats and
/// prints "unrecognized format" on apply; the `minimum`/`maximum` bounds carry the range.
fn drop_format(schema: &mut schemars::Schema) {
    schema.remove("format");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NodeKind {
    /// Holds a copy of the data.
    Replica,
    /// Holds a copy that is being removed; the node's agent deletes it and then
    /// removes this entry.
    Retiring,
    /// Diskless; only takes part in the quorum.
    Witness,
    /// Diskless; attaches the volume to run a workload.
    Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemberNode {
    /// Kubernetes node name, equal to `uname -n` of the node.
    // maxLength: the CEL cost budget needs bounded strings; mirrors the limit of
    // `model::valid_node_name`.
    #[schemars(length(max = MAX_NODE_NAME_LEN))]
    pub name: String,
    #[schemars(range(min = 0, max = MAX_NODE_ID), transform = drop_format)]
    pub node_id: u32,
    /// Address on the replication network; parsed as an IP address by the agent.
    // maxLength: same reason; the longest textual IPv6 address.
    #[schemars(length(max = MAX_ADDRESS_LEN))]
    pub address: String,
    pub kind: NodeKind,
}

// The first seven rules are transition rules (`oldSelf`): they run on update only.
// The last six run on create and update, so a bad object is refused up front
// instead of becoming permanent under the transition rules.
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
        "rule": "self.nodes.all(n, oldSelf.nodes.all(o, o.name != n.name || (o.nodeId == n.nodeId && o.address == n.address)))",
        "message": "a member node cannot change nodeId or address"
    },
    {
        "rule": "self.nodes.all(n, oldSelf.nodes.all(o, o.name != n.name || o.kind == n.kind || (o.kind == 'Witness' && n.kind == 'Replica') || (o.kind == 'Client' && n.kind == 'Replica') || (o.kind == 'Replica' && n.kind == 'Retiring') || (o.kind == 'Client' && n.kind == 'Witness') || (o.kind == 'Witness' && n.kind == 'Client')))",
        "message": "this change of a node's kind is not allowed"
    },
    {
        "rule": "self.nodes.all(n, n.kind != 'Retiring' || oldSelf.nodes.exists(o, o.name == n.name))",
        "message": "a node can become Retiring only from Replica"
    },
    {
        "rule": "oldSelf.nodes.all(o, o.kind == 'Client' || o.kind == 'Witness' || o.kind == 'Retiring' || self.nodes.exists(n, n.name == o.name))",
        "message": "a Replica node is removed only through Retiring"
    },
    {
        "rule": "(self.initialSync == 'Done' && oldSelf.initialSync == 'Done') || (self.nodes.all(n, n.kind == 'Client' || oldSelf.nodes.exists(o, o.name == n.name && o.kind == n.kind)) && oldSelf.nodes.all(o, o.kind == 'Client' || self.nodes.exists(n, n.name == o.name && n.kind == o.kind)))",
        "message": "the set of Replica, Retiring and Witness nodes changes only when initialSync is Done"
    },
    {
        "rule": "!(oldSelf.initialSync == 'Done' && self.initialSync == 'Skip')",
        "message": "initialSync cannot go back to Skip"
    },
    {
        "rule": "self.nodes.filter(n, n.kind == 'Replica').size() >= 2",
        "message": "at least two Replica nodes are required"
    },
    {
        "rule": "self.nodes.filter(n, n.kind == 'Retiring').size() <= 1",
        "message": "at most one node can be Retiring"
    },
    {
        "rule": "self.nodes.all(a, self.nodes.exists_one(b, b.nodeId == a.nodeId))",
        "message": "nodeId must be unique among nodes"
    },
    {
        "rule": "self.nodes.all(a, self.nodes.exists_one(b, b.address == a.address))",
        "message": "address must be unique among nodes"
    },
    {
        "rule": "self.sizeBytes % 4096 == 0",
        "message": "sizeBytes must be a multiple of 4096"
    },
    {
        "rule": "self.maxPeers >= size(self.nodes) - 1",
        "message": "maxPeers must be at least the number of other nodes"
    }
]))]
pub struct ReplicatedVolumeSpec {
    #[schemars(range(min = 4096, max = MAX_SIZE_BYTES), transform = drop_format)]
    pub size_bytes: u64,
    pub protocol: Protocol,
    #[schemars(range(min = 0, max = MAX_MINOR), transform = drop_format)]
    pub minor: u32,
    #[schemars(range(min = 1024, max = 65535), transform = drop_format)]
    pub port: u16,
    #[schemars(range(min = 1, max = MAX_NODE_ID), transform = drop_format)]
    pub max_peers: u32,
    #[serde(default)]
    pub initial_sync: InitialSync,
    // maxItems: the CEL cost budget needs bounded lists; mirrors the DRBD node
    // limit, `MAX_NODE_ID + 1` nodes.
    #[schemars(
        length(min = 2, max = MAX_NODE_ID + 1),
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
                diskful: matches!(node.kind, NodeKind::Replica | NodeKind::Retiring),
                retiring: node.kind == NodeKind::Retiring,
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
