//! Cluster node map: the ConfigMap `ambar-nodes` carries a JSON array of
//! nodes under the key `nodes`. This module only parses and validates it.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::Deserialize;

use crate::model::{MAX_NODE_ID, valid_node_name};

/// Name of the ConfigMap that holds the node map.
pub const NODES_CONFIGMAP: &str = "ambar-nodes";
/// Key inside the ConfigMap that holds the JSON array.
pub const NODES_KEY: &str = "nodes";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClusterNode {
    pub name: String,
    pub address: IpAddr,
    pub node_id: u32,
    pub capacity_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid nodes json: {0}")]
    Json(String),
    #[error(
        "invalid node name {0:?}: use 1-55 characters A-Z, a-z, 0-9, '.', '_' and '-', starting and ending with a letter or digit"
    )]
    NodeName(String),
    #[error("node name {0:?} appears more than once")]
    DuplicateName(String),
    #[error("nodeId {0} is above the DRBD limit of 31")]
    NodeId(u32),
    #[error("nodeId {0} appears more than once")]
    DuplicateNodeId(u32),
    #[error("address {0} appears more than once")]
    DuplicateAddress(IpAddr),
    #[error("the node list is empty")]
    Empty,
}

/// Validated node map, sorted by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterNodes(Vec<ClusterNode>);

impl ClusterNodes {
    pub fn from_json(json: &str) -> Result<Self, ConfigError> {
        let mut nodes: Vec<ClusterNode> = serde_json::from_str(json).map_err(|e| ConfigError::Json(e.to_string()))?;
        if nodes.is_empty() {
            return Err(ConfigError::Empty);
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        let mut addresses = HashSet::new();
        for node in &nodes {
            if !valid_node_name(&node.name) {
                return Err(ConfigError::NodeName(node.name.clone()));
            }
            if !names.insert(node.name.as_str()) {
                return Err(ConfigError::DuplicateName(node.name.clone()));
            }
            if node.node_id > MAX_NODE_ID {
                return Err(ConfigError::NodeId(node.node_id));
            }
            if !ids.insert(node.node_id) {
                return Err(ConfigError::DuplicateNodeId(node.node_id));
            }
            if !addresses.insert(node.address) {
                return Err(ConfigError::DuplicateAddress(node.address));
            }
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self(nodes))
    }

    pub fn get(&self, name: &str) -> Option<&ClusterNode> {
        self.0.iter().find(|n| n.name == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ClusterNode> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false for a validated map; present for the `len` pairing lint.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
#[path = "tests/config.rs"]
mod tests;
