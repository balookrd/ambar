//! Volume placement: which nodes hold the copies, whether a witness joins,
//! and which DRBD minor and port the volume takes. Pure functions over the
//! node map and the list of existing volumes; no Kubernetes calls, no I/O.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config::{ClusterNode, ClusterNodes};
use crate::crd::{MemberNode, NodeKind, ReplicatedVolume, ReplicatedVolumeSpec};
use crate::model::{InitialSync, MAX_MINOR, MAX_NODE_ID, MAX_SIZE_BYTES, Protocol};

pub const FIRST_MINOR: u32 = 1000;
pub const FIRST_PORT: u16 = 7000;
pub const DEFAULT_REPLICAS: usize = 3;
pub const DEFAULT_MAX_PEERS: u32 = 7;
pub const DEFAULT_SIZE_BYTES: u64 = 1 << 30;
/// DRBD size granularity; `sizeBytes` is a multiple of it.
pub const SIZE_ALIGN: u64 = 4096;

/// Prefix of the parameters the external-provisioner adds to every request.
const CSI_PARAM_PREFIX: &str = "csi.storage.k8s.io/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Already rounded up to a multiple of 4096.
    pub size_bytes: u64,
    pub replicas: usize,
    pub protocol: Protocol,
    pub max_peers: u32,
    /// Node the scheduler picked for the first pod, when known.
    pub selected_node: Option<String>,
}

/// What an existing volume already occupies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub minor: u32,
    pub port: u16,
    pub replica_nodes: Vec<String>,
    pub size_bytes: u64,
}

pub fn existing_of(object: &ReplicatedVolume) -> Existing {
    Existing {
        minor: object.spec.minor,
        port: object.spec.port,
        replica_nodes: object
            .spec
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Replica | NodeKind::Retiring))
            .map(|n| n.name.clone())
            .collect(),
        size_bytes: object.spec.size_bytes,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceError {
    #[error("replicas is {0}, at least 2 are required")]
    Replicas(usize),
    #[error("replicas is {replicas}, but the cluster has only {nodes} nodes")]
    NotEnoughNodes { replicas: usize, nodes: usize },
    #[error("selected node {0:?} is not in the node map")]
    UnknownSelectedNode(String),
    #[error("maxPeers {max_peers} must be between {needed} (the other nodes of the cluster) and 31")]
    MaxPeers { max_peers: u32, needed: u32 },
    #[error("node {node:?} has {free} bytes of free budget, {needed} are needed")]
    NoCapacity { node: String, free: u64, needed: u64 },
    #[error("no free minor and port are left")]
    NoFreeIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParamError {
    #[error("parameter {name:?}: invalid value {value:?}: {reason}")]
    Value {
        name: String,
        value: String,
        reason: &'static str,
    },
    #[error("unknown parameter {0:?}")]
    Unknown(String),
    #[error("requested size {0} is negative")]
    NegativeSize(i64),
    #[error("requested size {0} is above the limit of {MAX_SIZE_BYTES} bytes")]
    SizeTooLarge(i64),
}

fn bad_value(name: &str, value: &str, reason: &'static str) -> ParamError {
    ParamError::Value {
        name: name.to_string(),
        value: value.to_string(),
        reason,
    }
}

/// StorageClass parameters: `replicas`, `protocol`, `maxPeers`. Unknown keys
/// are an error, except keys starting with `csi.storage.k8s.io/`.
pub fn parse_parameters(params: &BTreeMap<String, String>) -> Result<(usize, Protocol, u32), ParamError> {
    let mut replicas = DEFAULT_REPLICAS;
    let mut protocol = Protocol::C;
    let mut max_peers = DEFAULT_MAX_PEERS;
    for (key, value) in params {
        match key.as_str() {
            "replicas" => {
                replicas = value
                    .parse()
                    .map_err(|_| bad_value(key, value, "expected a non-negative integer"))?;
            }
            "protocol" => {
                protocol = match value.as_str() {
                    "A" => Protocol::A,
                    "C" => Protocol::C,
                    _ => return Err(bad_value(key, value, "expected A or C")),
                };
            }
            "maxPeers" => {
                max_peers = value
                    .parse()
                    .map_err(|_| bad_value(key, value, "expected a non-negative integer"))?;
            }
            _ if key.starts_with(CSI_PARAM_PREFIX) => {}
            _ => return Err(ParamError::Unknown(key.clone())),
        }
    }
    Ok((replicas, protocol, max_peers))
}

/// Rounds the requested capacity up to a multiple of 4096. Zero means the
/// default size; negative and above-limit values are errors.
pub fn round_up_size(required_bytes: i64) -> Result<u64, ParamError> {
    let required = u64::try_from(required_bytes).map_err(|_| ParamError::NegativeSize(required_bytes))?;
    if required == 0 {
        return Ok(DEFAULT_SIZE_BYTES);
    }
    // `required` is at most i64::MAX, so adding the alignment cannot overflow u64.
    let rounded = required.div_ceil(SIZE_ALIGN) * SIZE_ALIGN;
    if rounded > MAX_SIZE_BYTES {
        return Err(ParamError::SizeTooLarge(required_bytes));
    }
    Ok(rounded)
}

/// Standard LVM Physical Extent (PE) size: 4 MiB (`vgcreate -s 4m`, LVM2 default).
/// Node capacity accounting assumes the backing Volume Group uses 4 MiB extents.
pub const LVM_PE_SIZE: u64 = 4 * 1024 * 1024;

/// Calculates the actual storage capacity occupied by a replicated volume copy
/// taking into account DRBD internal metadata overhead and LVM Physical Extent (4 MiB) alignment.
pub fn lvm_allocated_bytes(size_bytes: u64, max_peers: u32) -> u64 {
    let raw = crate::backing::backing_file_bytes(size_bytes, max_peers);
    raw.div_ceil(LVM_PE_SIZE) * LVM_PE_SIZE
}

fn budgets_by<'a>(
    nodes: &'a ClusterNodes,
    existing: &[Existing],
    size_of: impl Fn(&Existing) -> u64,
) -> HashMap<&'a str, u64> {
    let mut used: HashMap<&str, u64> = HashMap::new();
    for volume in existing {
        let bytes = size_of(volume);
        for name in &volume.replica_nodes {
            let slot = used.entry(name.as_str()).or_insert(0);
            *slot = slot.saturating_add(bytes);
        }
    }
    nodes
        .iter()
        .map(|n| {
            let taken = used.get(n.name.as_str()).copied().unwrap_or(0);
            (n.name.as_str(), n.capacity_bytes.saturating_sub(taken))
        })
        .collect()
}

/// Free budget of every node taking into account LVM PE alignment and DRBD metadata overhead.
pub fn free_budgets_aligned<'a>(
    nodes: &'a ClusterNodes,
    existing: &[Existing],
    default_max_peers: u32,
) -> HashMap<&'a str, u64> {
    budgets_by(nodes, existing, |v| {
        lvm_allocated_bytes(v.size_bytes, default_max_peers)
    })
}

/// Free budget of every node: capacity minus the sizes of the volumes that
/// keep a copy on it, saturating at zero.
pub fn free_budgets<'a>(nodes: &'a ClusterNodes, existing: &[Existing]) -> HashMap<&'a str, u64> {
    budgets_by(nodes, existing, |v| v.size_bytes)
}

/// Smallest `i` such that neither minor `FIRST_MINOR + i` nor port
/// `FIRST_PORT + i` is taken.
fn free_index(existing: &[Existing]) -> Result<(u32, u16), PlaceError> {
    let minors: HashSet<u32> = existing.iter().map(|e| e.minor).collect();
    let ports: HashSet<u16> = existing.iter().map(|e| e.port).collect();
    let mut i: u32 = 0;
    loop {
        let minor = FIRST_MINOR.checked_add(i).filter(|m| *m <= MAX_MINOR);
        let port = u16::try_from(u32::from(FIRST_PORT) + i).ok();
        let (Some(minor), Some(port)) = (minor, port) else {
            return Err(PlaceError::NoFreeIndex);
        };
        if !minors.contains(&minor) && !ports.contains(&port) {
            return Ok((minor, port));
        }
        i += 1;
    }
}

/// Orders by free budget descending, then by name.
fn by_budget(free: &HashMap<&str, u64>, list: &mut [&ClusterNode]) {
    list.sort_by(|a, b| {
        free[b.name.as_str()]
            .cmp(&free[a.name.as_str()])
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Picks a node for a witness:
/// 1. First by name from `clients` that is not in `exclude`.
/// 2. Otherwise a node from `nodes` not in `exclude` and not in `clients` with the
///    highest free budget, breaking ties by name ascending.
/// 3. `None` if no such node exists.
pub fn pick_witness(
    nodes: &ClusterNodes,
    free: &HashMap<&str, u64>,
    exclude: &[&str],
    clients: &[&str],
) -> Option<String> {
    let mut eligible_clients: Vec<&str> = clients.iter().copied().filter(|c| !exclude.contains(c)).collect();
    if !eligible_clients.is_empty() {
        eligible_clients.sort_unstable();
        return Some(eligible_clients[0].to_string());
    }

    let mut eligible_nodes: Vec<&ClusterNode> = nodes
        .iter()
        .filter(|n| !exclude.contains(&n.name.as_str()) && !clients.contains(&n.name.as_str()))
        .collect();
    if eligible_nodes.is_empty() {
        return None;
    }
    by_budget(free, &mut eligible_nodes);
    eligible_nodes.first().map(|n| n.name.clone())
}

pub fn place(req: &Request, nodes: &ClusterNodes, existing: &[Existing]) -> Result<ReplicatedVolumeSpec, PlaceError> {
    if req.replicas < 2 {
        return Err(PlaceError::Replicas(req.replicas));
    }
    if req.replicas > nodes.len() {
        return Err(PlaceError::NotEnoughNodes {
            replicas: req.replicas,
            nodes: nodes.len(),
        });
    }
    // The headroom lets any node of the cluster join later as a Client.
    let needed_peers = nodes.len().saturating_sub(1) as u32;
    if req.max_peers < needed_peers || req.max_peers > MAX_NODE_ID {
        return Err(PlaceError::MaxPeers {
            max_peers: req.max_peers,
            needed: needed_peers,
        });
    }

    let free = free_budgets(nodes, existing);
    let mut chosen: Vec<&ClusterNode> = Vec::with_capacity(req.replicas);
    if let Some(name) = &req.selected_node {
        let node = nodes
            .get(name)
            .ok_or_else(|| PlaceError::UnknownSelectedNode(name.clone()))?;
        let available = free[node.name.as_str()];
        if available < req.size_bytes {
            return Err(PlaceError::NoCapacity {
                node: node.name.clone(),
                free: available,
                needed: req.size_bytes,
            });
        }
        chosen.push(node);
    }

    let mut others: Vec<&ClusterNode> = nodes
        .iter()
        .filter(|n| !chosen.iter().any(|c| c.name == n.name))
        .collect();
    by_budget(&free, &mut others);
    let wanted = req.replicas - chosen.len();
    let (fits, skipped): (Vec<&ClusterNode>, Vec<&ClusterNode>) = others
        .into_iter()
        .partition(|n| free[n.name.as_str()] >= req.size_bytes);
    if fits.len() < wanted {
        // `others` was sorted, so the first skipped node has the highest budget.
        let best = skipped
            .first()
            .expect("fewer fitting nodes than wanted leaves a skipped node, since replicas <= nodes");
        return Err(PlaceError::NoCapacity {
            node: best.name.clone(),
            free: free[best.name.as_str()],
            needed: req.size_bytes,
        });
    }
    chosen.extend(fits.into_iter().take(wanted));

    // A witness holds no data, so it needs no budget.
    let witness = if req.replicas.is_multiple_of(2) {
        let exclude: Vec<&str> = chosen.iter().map(|c| c.name.as_str()).collect();
        pick_witness(nodes, &free, &exclude, &[]).and_then(|name| nodes.get(&name))
    } else {
        None
    };

    let (minor, port) = free_index(existing)?;

    let mut members: Vec<MemberNode> = chosen
        .iter()
        .map(|n| (n, NodeKind::Replica))
        .chain(witness.iter().map(|n| (n, NodeKind::Witness)))
        .map(|(n, kind)| MemberNode {
            name: n.name.clone(),
            node_id: n.node_id,
            address: n.address.to_string(),
            kind,
        })
        .collect();
    members.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ReplicatedVolumeSpec {
        size_bytes: req.size_bytes,
        protocol: req.protocol,
        minor,
        port,
        max_peers: req.max_peers,
        initial_sync: InitialSync::Skip,
        nodes: members,
    })
}

#[cfg(test)]
#[path = "tests/placement.rs"]
mod tests;
