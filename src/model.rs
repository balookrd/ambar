//! Desired state of one replicated volume, as the agent reads it.

use std::collections::HashSet;
use std::net::IpAddr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// DRBD 9 addresses at most 32 nodes per resource: node ids 0..=31.
pub const MAX_NODE_ID: u32 = 31;
/// Largest device minor number the kernel accepts (20 bits).
pub const MAX_MINOR: u32 = (1 << 20) - 1;
/// Largest volume the agent accepts: 1 PiB.
pub const MAX_SIZE_BYTES: u64 = 1 << 50;
/// Longest name part of a Kubernetes qualified name (the part after `/`).
pub const MAX_QUALIFIED_NAME_PART_LEN: usize = 63;
/// Prefix of the per-replica finalizer name part (`replica-<node>`).
pub const REPLICA_FINALIZER_NAME_PREFIX: &str = "replica-";
/// Longest node name `valid_node_name` accepts: `replica-<node>` must fit
/// into a qualified-name part, 63 - 8 = 55 characters.
pub const MAX_NODE_NAME_LEN: usize = MAX_QUALIFIED_NAME_PART_LEN - REPLICA_FINALIZER_NAME_PREFIX.len();
/// A volume keeps at least two copies; single-copy volumes stay on local-path.
pub const MIN_DISKFUL: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Protocol {
    A,
    C,
}

/// Whether the leader may declare the copies identical without copying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum InitialSync {
    /// The volume was just created and holds no data: the leader may declare
    /// the copies identical without copying.
    Skip,
    /// The copies must never be declared identical without copying.
    #[default]
    Done,
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
    /// `true`: the node holds a copy that is being removed (`Retiring`).
    #[serde(default)]
    pub retiring: bool,
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
    /// Absent in the description means `Done`: nothing is ever skipped by
    /// default.
    #[serde(default)]
    pub initial_sync: InitialSync,
    pub nodes: Vec<VolumeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("invalid volume json: {0}")]
    Json(String),
    #[error(
        "invalid volume name {0:?}: use 1-63 characters a-z, 0-9 and '-', starting with a letter or digit; \"all\" is reserved by drbdadm"
    )]
    Name(String),
    #[error(
        "invalid node name {0:?}: use 1-55 characters A-Z, a-z, 0-9, '.', '_' and '-', starting and ending with a letter or digit"
    )]
    NodeName(String),
    #[error("sizeBytes {0} must be a positive multiple of 4096, at most 1125899906842624")]
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
        if !valid_volume_name(&self.name) {
            return Err(SpecError::Name(self.name.clone()));
        }
        if self.size_bytes == 0 || !self.size_bytes.is_multiple_of(4096) || self.size_bytes > MAX_SIZE_BYTES {
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
            if !valid_node_name(&node.name) {
                return Err(SpecError::NodeName(node.name.clone()));
            }
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
        // At least MIN_DISKFUL nodes exist here, so the subtraction cannot
        // underflow; saturating keeps that from depending on the check above.
        let needed = self.nodes.len().saturating_sub(1) as u32;
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

    /// The diskful, non-retiring node with the lowest node id runs the one-time
    /// "skip initial sync" command for a freshly created volume.
    pub fn initial_sync_leader(&self) -> Option<&VolumeNode> {
        self.diskful_nodes().filter(|n| !n.retiring).min_by_key(|n| n.node_id)
    }
}

/// Volume name rule: 1-63 characters a-z, 0-9 and '-', not starting with
/// '-', and not `all`. The name becomes part of file paths and DRBD resource
/// names; `all` is a drbdadm keyword (`drbdadm down all` acts on every
/// resource of the node), so a volume named `all` would make `ambar remove`
/// take down every volume.
pub fn valid_volume_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-';
    !bytes.is_empty() && bytes.len() <= 63 && bytes[0] != b'-' && bytes.iter().all(allowed) && name != "all"
}

/// Hostname-like, at most `MAX_NODE_NAME_LEN` characters, first and last
/// character a letter or digit (the name is part of a finalizer name, and a
/// qualified-name part must end alphanumeric). DRBD writes the name into the resource file verbatim, so
/// anything beyond these characters could inject config syntax.
pub fn valid_node_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-');
    !bytes.is_empty()
        && bytes.len() <= MAX_NODE_NAME_LEN
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes.iter().all(allowed)
}

/// LVM Volume Group name rule: 1-127 characters a-z, A-Z, 0-9, '.', '_', '-' and '+',
/// not starting with '-', and not '.' or '..'.
pub fn valid_volume_group_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let allowed = |b: &u8| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-' | b'+');
    !bytes.is_empty()
        && bytes.len() <= 127
        && bytes[0] != b'-'
        && name != "."
        && name != ".."
        && bytes.iter().all(allowed)
}

#[cfg(test)]
#[path = "tests/model.rs"]
mod tests;
