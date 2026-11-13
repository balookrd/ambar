//! Desired state of one replicated volume, as the agent reads it.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// DRBD 9 addresses at most 32 nodes per resource: node ids 0..=31.
pub const MAX_NODE_ID: u32 = 31;
/// Largest device minor number the kernel accepts (20 bits).
pub const MAX_MINOR: u32 = (1 << 20) - 1;
/// A volume keeps at least two copies; single-copy volumes stay on local-path.
pub const MIN_DISKFUL: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    A,
    C,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeNode {
    /// Node name as `uname -n` reports it; DRBD matches `on <name>` against it.
    pub name: String,
    /// Address on the replication network.
    pub address: IpAddr,
    pub node_id: u32,
    /// `true`: the node holds a copy; `false`: it attaches without a disk.
    pub diskful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeSpec {
    pub name: String,
    pub size_bytes: u64,
    pub protocol: Protocol,
    pub minor: u32,
    pub port: u16,
    /// Number of peers the DRBD metadata is sized for (`create-md --max-peers`).
    pub max_peers: u32,
    pub nodes: Vec<VolumeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("invalid volume json: {0}")]
    Json(String),
    #[error("invalid volume name {0:?}: use 1-63 characters a-z, 0-9 and '-', starting with a letter or digit")]
    Name(String),
    #[error("sizeBytes {0} must be a positive multiple of 4096")]
    Size(u64),
    #[error("minor {0} is above 1048575")]
    Minor(u32),
    #[error("port {0} is below 1024")]
    Port(u16),
    #[error("duplicate node name {0:?}")]
    DuplicateName(String),
    #[error("duplicate nodeId {0}")]
    DuplicateNodeId(u32),
    #[error("nodeId {0} is above 31")]
    NodeId(u32),
    #[error("{0} nodes hold a copy, at least 2 are required")]
    Replicas(usize),
    #[error("maxPeers {max_peers} must be between {needed} (other nodes of this volume) and 31")]
    MaxPeers { max_peers: u32, needed: u32 },
}

impl VolumeSpec {
    /// Parses and validates a volume description.
    pub fn from_json(json: &str) -> Result<Self, SpecError> {
        let spec: Self = serde_json::from_str(json).map_err(|e| SpecError::Json(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if !valid_name(&self.name) {
            return Err(SpecError::Name(self.name.clone()));
        }
        if self.size_bytes == 0 || !self.size_bytes.is_multiple_of(4096) {
            return Err(SpecError::Size(self.size_bytes));
        }
        if self.minor > MAX_MINOR {
            return Err(SpecError::Minor(self.minor));
        }
        if self.port < 1024 {
            return Err(SpecError::Port(self.port));
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !names.insert(node.name.as_str()) {
                return Err(SpecError::DuplicateName(node.name.clone()));
            }
            if node.node_id > MAX_NODE_ID {
                return Err(SpecError::NodeId(node.node_id));
            }
            if !ids.insert(node.node_id) {
                return Err(SpecError::DuplicateNodeId(node.node_id));
            }
        }
        let diskful = self.diskful_nodes().count();
        if diskful < MIN_DISKFUL {
            return Err(SpecError::Replicas(diskful));
        }
        let needed = self.nodes.len() as u32 - 1;
        if self.max_peers < needed || self.max_peers > MAX_NODE_ID {
            return Err(SpecError::MaxPeers {
                max_peers: self.max_peers,
                needed,
            });
        }
        Ok(())
    }

    pub fn node(&self, name: &str) -> Option<&VolumeNode> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn diskful_nodes(&self) -> impl Iterator<Item = &VolumeNode> {
        self.nodes.iter().filter(|n| n.diskful)
    }

    /// The diskful node with the lowest node id runs the one-time
    /// "skip initial sync" command for a freshly created volume.
    pub fn initial_sync_leader(&self) -> Option<&VolumeNode> {
        self.diskful_nodes().min_by_key(|n| n.node_id)
    }
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-';
    !bytes.is_empty() && bytes.len() <= 63 && bytes[0] != b'-' && bytes.iter().all(allowed)
}

#[cfg(test)]
#[path = "tests/model.rs"]
mod tests;
