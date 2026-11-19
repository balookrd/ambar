//! Pure rules of replica add, remove and move.
//!
//! Evaluates the current state of a volume and plans the next step for
//! `ambar replica add`, `ambar replica remove`, and `ambar replica move`.
//!
//! Implements rules R22–R25, R27, and R31:
//! - R22 (add): first edit adds `Replica` with finalizer and witness if N becomes even;
//!   waits for UpToDate; then removes witness if N is odd.
//! - R23 (remove): first edit sets `Retiring` and adds witness if N becomes even;
//!   waits for node to disappear; then adjusts witness if needed.
//! - R24 (move): adds destination `Replica` with finalizer; waits for UpToDate;
//!   then sets source to `Retiring`; waits for source to disappear.
//! - R25 (safety checks before first patch): object not deleting, initialSync is Done,
//!   no Retiring nodes present, all live members connected each-to-each, replica disks
//!   UpToDate, nodes known in `ambar-nodes`.
//! - R27 (witness selection and retirement): pick witness from Clients or highest free budget;
//!   convert witness to Client if Primary on retirement.
//! - R31: N in witness rules counts only `Replica` nodes (excluding `Retiring`).

use std::collections::HashMap;

use crate::config::ClusterNodes;
use crate::crd::{MemberNode, NodeKind, ReplicatedVolumeSpec, ReplicatedVolumeStatus};
use crate::membership::replica_finalizer;
use crate::model::InitialSync;
use crate::placement::{Existing, free_budgets, pick_witness};

/// What the object looks like to the command: spec, status, deletion, finalizers.
#[derive(Debug, Clone)]
pub struct View<'a> {
    pub spec: &'a ReplicatedVolumeSpec,
    pub status: Option<&'a ReplicatedVolumeStatus>,
    pub deleting: bool,
    pub finalizers: &'a [String],
}

/// One edit of the object: the new `spec.nodes` (sorted by name) and the
/// finalizer to add, if any. Applied only while `spec.nodes` is still the
/// list it was computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub nodes: Vec<MemberNode>,
    pub add_finalizer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Apply this edit, then call the planner again.
    Edit(Edit),
    /// Wait for the agents, then call the planner again; the text says what for.
    Wait(String),
    /// The requested change is complete.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChangeError {
    #[error("the volume is being deleted")]
    Deleting,
    #[error("node {0:?} is not known in ambar-nodes")]
    UnknownNode(String),
    #[error("initialSync is not Done (it is {0:?})")]
    InitialSyncNotDone(String),
    #[error("a replica is already being retired on node {0:?}")]
    RetiringAlreadyPresent(String),
    #[error("node {0:?} is not a member of the volume")]
    NotAMember(String),
    #[error("cannot remove replica: at least 2 replicas are required")]
    TooFewReplicas,
    #[error("node {0:?} is Primary; move the pod first")]
    Primary(String),
    #[error("replica {0:?} has not reported its status")]
    ReplicaMissingStatus(String),
    #[error("replica {node:?} disk is {disk}; must be UpToDate")]
    ReplicaNotUpToDate { node: String, disk: String },
    #[error("member {0:?} has not reported its status")]
    MemberMissingStatus(String),
    #[error("node {from:?} sees peer {to:?} as {state}; all peers must be Connected")]
    PeerNotConnected { from: String, to: String, state: String },
    #[error("node {from:?} sees peer {to:?} disk as {disk}; all replica disks must be UpToDate")]
    PeerDiskNotUpToDate { from: String, to: String, disk: String },
    #[error("cannot add member: maxPeers limit reached")]
    NoRoom,
    #[error("node {node:?} has {free} bytes free, but {needed} bytes are required")]
    NoCapacity { node: String, needed: u64, free: u64 },
    #[error("source and destination nodes must be different")]
    SameNode,
}

/// N in witness rules is the count of Replica nodes, excluding Retiring (R31).
fn replica_count(nodes: &[MemberNode]) -> usize {
    nodes.iter().filter(|n| n.kind == NodeKind::Replica).count()
}

/// A witness is needed when the count of Replica nodes is even.
fn witness_needed(n_replicas: usize) -> bool {
    n_replicas.is_multiple_of(2)
}

fn find_witness(nodes: &[MemberNode]) -> Option<&MemberNode> {
    nodes.iter().find(|n| n.kind == NodeKind::Witness)
}

/// R25 safety checks before the first patch.
fn check_r25(view: &View, _cluster: &ClusterNodes, ignore_node: Option<&str>) -> Result<(), ChangeError> {
    if view.deleting {
        return Err(ChangeError::Deleting);
    }
    if view.spec.initial_sync != InitialSync::Done {
        return Err(ChangeError::InitialSyncNotDone(format!("{:?}", view.spec.initial_sync)));
    }
    if let Some(r) = view.spec.nodes.iter().find(|n| n.kind == NodeKind::Retiring) {
        return Err(ChangeError::RetiringAlreadyPresent(r.name.clone()));
    }

    let status = match view.status {
        Some(s) => s,
        None => {
            // If status is completely absent, any replica in spec is missing status
            if let Some(rep) = view
                .spec
                .nodes
                .iter()
                .find(|n| n.kind == NodeKind::Replica && Some(n.name.as_str()) != ignore_node)
            {
                return Err(ChangeError::ReplicaMissingStatus(rep.name.clone()));
            }
            if let Some(m) = view.spec.nodes.iter().find(|n| Some(n.name.as_str()) != ignore_node) {
                return Err(ChangeError::MemberMissingStatus(m.name.clone()));
            }
            return Ok(());
        }
    };

    // 1. Every Replica member (except ignore_node) must have status with disk == "UpToDate"
    for member in &view.spec.nodes {
        if member.kind == NodeKind::Replica && Some(member.name.as_str()) != ignore_node {
            let Some(entry) = status.nodes.iter().find(|n| n.name == member.name) else {
                return Err(ChangeError::ReplicaMissingStatus(member.name.clone()));
            };
            if entry.disk != "UpToDate" {
                return Err(ChangeError::ReplicaNotUpToDate {
                    node: member.name.clone(),
                    disk: entry.disk.clone(),
                });
            }
        }
    }

    // 2. Every member (except ignore_node) must have an entry in status
    for member in &view.spec.nodes {
        if Some(member.name.as_str()) == ignore_node {
            continue;
        }
        if !status.nodes.iter().any(|n| n.name == member.name) {
            return Err(ChangeError::MemberMissingStatus(member.name.clone()));
        }
    }

    // 3. And in its status, connections to every OTHER member (except ignore_node) must be Connected
    for member in &view.spec.nodes {
        if Some(member.name.as_str()) == ignore_node {
            continue;
        }
        let entry = status.nodes.iter().find(|n| n.name == member.name).unwrap();

        for peer in &view.spec.nodes {
            if peer.name == member.name || Some(peer.name.as_str()) == ignore_node {
                continue;
            }
            let Some(conn) = entry.connections.iter().find(|c| c.peer == peer.name) else {
                return Err(ChangeError::PeerNotConnected {
                    from: member.name.clone(),
                    to: peer.name.clone(),
                    state: "missing".to_string(),
                });
            };
            if conn.state != "Connected" {
                return Err(ChangeError::PeerNotConnected {
                    from: member.name.clone(),
                    to: peer.name.clone(),
                    state: conn.state.clone(),
                });
            }
            // And if peer is a Replica, peerDisk must be UpToDate
            if peer.kind == NodeKind::Replica && conn.peer_disk != "UpToDate" {
                return Err(ChangeError::PeerDiskNotUpToDate {
                    from: member.name.clone(),
                    to: peer.name.clone(),
                    disk: conn.peer_disk.clone(),
                });
            }
        }
    }

    Ok(())
}

fn check_capacity(node: &str, view: &View, cluster: &ClusterNodes, existing: &[Existing]) -> Result<(), ChangeError> {
    if view
        .spec
        .nodes
        .iter()
        .any(|n| n.name == node && matches!(n.kind, NodeKind::Replica | NodeKind::Retiring))
    {
        return Ok(());
    }
    let free_map = free_budgets(cluster, existing);
    let free = free_map.get(node).copied().unwrap_or(0);
    if free < view.spec.size_bytes {
        return Err(ChangeError::NoCapacity {
            node: node.to_string(),
            needed: view.spec.size_bytes,
            free,
        });
    }
    Ok(())
}

/// R27: Remove witness. If the witness is Primary in status, convert it to Client.
fn remove_witness(nodes: &mut Vec<MemberNode>, status: Option<&ReplicatedVolumeStatus>) {
    let Some(pos) = nodes.iter().position(|n| n.kind == NodeKind::Witness) else {
        return;
    };
    let w_name = &nodes[pos].name;
    let is_primary = status
        .and_then(|s| s.nodes.iter().find(|n| n.name == *w_name))
        .is_some_and(|n| n.role == "Primary");
    if is_primary {
        nodes[pos].kind = NodeKind::Client;
    } else {
        nodes.remove(pos);
    }
}

/// R27: Assign a witness if candidates exist.
fn assign_witness(
    nodes: &mut Vec<MemberNode>,
    cluster: &ClusterNodes,
    free: &HashMap<&str, u64>,
    extra_exclude: &[&str],
    max_peers: u32,
) {
    let clients: Vec<String> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Client && !extra_exclude.contains(&n.name.as_str()))
        .map(|n| n.name.clone())
        .collect();
    let mut exclude: Vec<String> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Replica | NodeKind::Retiring))
        .map(|n| n.name.clone())
        .collect();
    for extra in extra_exclude {
        if !exclude.iter().any(|e| e == extra) {
            exclude.push(extra.to_string());
        }
    }
    let clients_ref: Vec<&str> = clients.iter().map(|s| s.as_str()).collect();
    let exclude_ref: Vec<&str> = exclude.iter().map(|s| s.as_str()).collect();
    if let Some(cand) = pick_witness(cluster, free, &exclude_ref, &clients_ref) {
        if let Some(m) = nodes.iter_mut().find(|n| n.name == cand) {
            m.kind = NodeKind::Witness;
        } else if nodes.len() < (max_peers as usize) + 1 {
            let cnode = cluster.get(&cand).unwrap();
            nodes.push(MemberNode {
                name: cand,
                node_id: cnode.node_id,
                address: cnode.address.to_string(),
                kind: NodeKind::Witness,
            });
        }
    }
}

pub fn next_add(view: &View, node: &str, cluster: &ClusterNodes, existing: &[Existing]) -> Result<Next, ChangeError> {
    if view.deleting {
        return Err(ChangeError::Deleting);
    }
    if cluster.get(node).is_none() {
        return Err(ChangeError::UnknownNode(node.to_string()));
    }

    let existing_member = view.spec.nodes.iter().find(|n| n.name == node);

    // Case 1: Node is already a Replica
    if let Some(m) = existing_member
        && m.kind == NodeKind::Replica
    {
        let disk = view
            .status
            .and_then(|s| s.nodes.iter().find(|n| n.name == node))
            .map(|n| n.disk.as_str());
        if disk != Some("UpToDate") {
            let d = disk.unwrap_or("missing");
            return Ok(Next::Wait(format!("node {node:?} is {d}; waiting for UpToDate")));
        }

        // Replica is UpToDate! Check witness rule.
        let n_replicas = replica_count(&view.spec.nodes);
        if !witness_needed(n_replicas) && find_witness(&view.spec.nodes).is_some() {
            let mut new_nodes = view.spec.nodes.clone();
            remove_witness(&mut new_nodes, view.status);
            new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(Next::Edit(Edit {
                nodes: new_nodes,
                add_finalizer: None,
            }));
        }

        return Ok(Next::Done);
    }

    // Case 2: Node is not a Replica (new node, Client, or Witness)
    check_r25(view, cluster, None)?;

    if let Some(m) = existing_member
        && (m.kind == NodeKind::Client || m.kind == NodeKind::Witness)
    {
        let is_primary = view
            .status
            .and_then(|s| s.nodes.iter().find(|n| n.name == node))
            .is_some_and(|n| n.role == "Primary");
        if is_primary {
            return Err(ChangeError::Primary(node.to_string()));
        }
    }

    check_capacity(node, view, cluster, existing)?;

    if existing_member.is_none() && view.spec.nodes.len() > (view.spec.max_peers as usize) {
        return Err(ChangeError::NoRoom);
    }

    let mut new_nodes = view.spec.nodes.clone();
    let was_witness = existing_member.is_some_and(|m| m.kind == NodeKind::Witness);

    if let Some(m) = new_nodes.iter_mut().find(|n| n.name == node) {
        m.kind = NodeKind::Replica;
    } else {
        let cnode = cluster.get(node).unwrap();
        new_nodes.push(MemberNode {
            name: node.to_string(),
            node_id: cnode.node_id,
            address: cnode.address.to_string(),
            kind: NodeKind::Replica,
        });
    }

    let n_new = replica_count(&new_nodes);
    let witness_present = find_witness(&new_nodes).is_some();
    let need_witness = !witness_present && (was_witness || witness_needed(n_new));

    if need_witness {
        let free_map = free_budgets(cluster, existing);
        assign_witness(&mut new_nodes, cluster, &free_map, &[node], view.spec.max_peers);
    }

    new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let finalizer = replica_finalizer(node);
    let add_finalizer = if view.finalizers.contains(&finalizer) {
        None
    } else {
        Some(finalizer)
    };

    Ok(Next::Edit(Edit {
        nodes: new_nodes,
        add_finalizer,
    }))
}

pub fn next_remove(
    view: &View,
    node: &str,
    seen_member: bool,
    cluster: &ClusterNodes,
    existing: &[Existing],
) -> Result<Next, ChangeError> {
    if view.deleting {
        return Err(ChangeError::Deleting);
    }
    if cluster.get(node).is_none() {
        return Err(ChangeError::UnknownNode(node.to_string()));
    }

    let member = view.spec.nodes.iter().find(|n| n.name == node);
    let has_finalizer = view.finalizers.contains(&replica_finalizer(node));

    match member {
        Some(m) if m.kind == NodeKind::Retiring => {
            let msg = view
                .status
                .and_then(|s| s.nodes.iter().find(|n| n.name == node))
                .map(|n| n.message.as_str())
                .unwrap_or("");
            let text = if msg.is_empty() {
                format!("waiting for node {node:?} to remove its copy")
            } else {
                format!("waiting for node {node:?} to remove its copy: {msg}")
            };
            Ok(Next::Wait(text))
        }
        Some(m) if m.kind == NodeKind::Replica => {
            let n_replicas = replica_count(&view.spec.nodes);
            if n_replicas <= 2 {
                return Err(ChangeError::TooFewReplicas);
            }
            let is_primary = view
                .status
                .and_then(|s| s.nodes.iter().find(|n| n.name == node))
                .is_some_and(|n| n.role == "Primary");
            if is_primary {
                return Err(ChangeError::Primary(node.to_string()));
            }

            check_r25(view, cluster, Some(node))?;

            let mut new_nodes = view.spec.nodes.clone();
            new_nodes.iter_mut().find(|n| n.name == node).unwrap().kind = NodeKind::Retiring;

            let n_after = n_replicas - 1;
            let witness_present = find_witness(&new_nodes).is_some();
            if !witness_present && witness_needed(n_after) {
                let free_map = free_budgets(cluster, existing);
                assign_witness(&mut new_nodes, cluster, &free_map, &[node], view.spec.max_peers);
            }

            new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(Next::Edit(Edit {
                nodes: new_nodes,
                add_finalizer: None,
            }))
        }
        Some(_) => Err(ChangeError::NotAMember(node.to_string())),
        None => {
            if has_finalizer {
                return Ok(Next::Wait(format!("waiting for node {node:?} to remove its copy")));
            }
            if !seen_member {
                return Err(ChangeError::NotAMember(node.to_string()));
            }

            // seen_member is true: copy is removed, adjust witness if needed
            let n_replicas = replica_count(&view.spec.nodes);
            let witness_present = find_witness(&view.spec.nodes).is_some();

            if !witness_needed(n_replicas) && witness_present {
                let mut new_nodes = view.spec.nodes.clone();
                remove_witness(&mut new_nodes, view.status);
                new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
                return Ok(Next::Edit(Edit {
                    nodes: new_nodes,
                    add_finalizer: None,
                }));
            }

            if witness_needed(n_replicas) && !witness_present {
                let mut new_nodes = view.spec.nodes.clone();
                let free_map = free_budgets(cluster, existing);
                // Removed node is now a valid candidate!
                assign_witness(&mut new_nodes, cluster, &free_map, &[], view.spec.max_peers);
                if new_nodes != view.spec.nodes {
                    new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
                    return Ok(Next::Edit(Edit {
                        nodes: new_nodes,
                        add_finalizer: None,
                    }));
                }
            }

            Ok(Next::Done)
        }
    }
}

fn balance_witness_or_done(view: &View, cluster: &ClusterNodes, existing: &[Existing]) -> Result<Next, ChangeError> {
    let n_replicas = replica_count(&view.spec.nodes);
    let witness_present = find_witness(&view.spec.nodes).is_some();

    if !witness_needed(n_replicas) && witness_present {
        let mut new_nodes = view.spec.nodes.clone();
        remove_witness(&mut new_nodes, view.status);
        new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
        return Ok(Next::Edit(Edit {
            nodes: new_nodes,
            add_finalizer: None,
        }));
    }

    if witness_needed(n_replicas) && !witness_present {
        let mut new_nodes = view.spec.nodes.clone();
        let free_map = free_budgets(cluster, existing);
        assign_witness(&mut new_nodes, cluster, &free_map, &[], view.spec.max_peers);
        if new_nodes != view.spec.nodes {
            new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(Next::Edit(Edit {
                nodes: new_nodes,
                add_finalizer: None,
            }));
        }
    }

    Ok(Next::Done)
}

pub fn next_move(
    view: &View,
    from: &str,
    to: &str,
    cluster: &ClusterNodes,
    existing: &[Existing],
) -> Result<Next, ChangeError> {
    if view.deleting {
        return Err(ChangeError::Deleting);
    }
    if from == to {
        return Err(ChangeError::SameNode);
    }
    if cluster.get(from).is_none() {
        return Err(ChangeError::UnknownNode(from.to_string()));
    }
    if cluster.get(to).is_none() {
        return Err(ChangeError::UnknownNode(to.to_string()));
    }

    let from_in_nodes = view.spec.nodes.iter().find(|n| n.name == from);
    let from_has_finalizer = view.finalizers.contains(&replica_finalizer(from));

    if let Some(m) = from_in_nodes
        && m.kind != NodeKind::Replica
        && m.kind != NodeKind::Retiring
    {
        return Err(ChangeError::NotAMember(from.to_string()));
    }

    if from_in_nodes.is_none() && !from_has_finalizer {
        let to_is_replica = view
            .spec
            .nodes
            .iter()
            .any(|n| n.name == to && n.kind == NodeKind::Replica);
        let to_uptodate = view
            .status
            .and_then(|s| s.nodes.iter().find(|n| n.name == to))
            .is_some_and(|n| n.disk == "UpToDate");
        if to_is_replica && to_uptodate {
            return balance_witness_or_done(view, cluster, existing);
        }
        return Err(ChangeError::NotAMember(from.to_string()));
    }

    let to_member = view.spec.nodes.iter().find(|n| n.name == to);

    // Step 1 of move: make `to` a Replica
    if to_member.map(|m| m.kind) != Some(NodeKind::Replica) {
        if let Some(st) = view.status.and_then(|s| s.nodes.iter().find(|n| n.name == from))
            && st.role == "Primary"
        {
            return Err(ChangeError::Primary(from.to_string()));
        }

        if let Some(m) = to_member
            && (m.kind == NodeKind::Client || m.kind == NodeKind::Witness)
        {
            let to_is_primary = view
                .status
                .and_then(|s| s.nodes.iter().find(|n| n.name == to))
                .is_some_and(|n| n.role == "Primary");
            if to_is_primary {
                return Err(ChangeError::Primary(to.to_string()));
            }
        }

        check_r25(view, cluster, None)?;
        check_capacity(to, view, cluster, existing)?;

        if to_member.is_none() && view.spec.nodes.len() > (view.spec.max_peers as usize) {
            return Err(ChangeError::NoRoom);
        }

        let mut new_nodes = view.spec.nodes.clone();
        let to_was_witness = to_member.is_some_and(|m| m.kind == NodeKind::Witness);

        if let Some(m) = new_nodes.iter_mut().find(|n| n.name == to) {
            m.kind = NodeKind::Replica;
        } else {
            let cnode = cluster.get(to).unwrap();
            new_nodes.push(MemberNode {
                name: to.to_string(),
                node_id: cnode.node_id,
                address: cnode.address.to_string(),
                kind: NodeKind::Replica,
            });
        }

        // R24: If target was witness, pick new witness excluding from and to
        if to_was_witness {
            let free_map = free_budgets(cluster, existing);
            assign_witness(&mut new_nodes, cluster, &free_map, &[from, to], view.spec.max_peers);
        }

        new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
        let finalizer = replica_finalizer(to);
        let add_finalizer = if view.finalizers.contains(&finalizer) {
            None
        } else {
            Some(finalizer)
        };

        return Ok(Next::Edit(Edit {
            nodes: new_nodes,
            add_finalizer,
        }));
    }

    // Step 2 of move: `to` is Replica, check if it's UpToDate
    let to_disk = view
        .status
        .and_then(|s| s.nodes.iter().find(|n| n.name == to))
        .map(|n| n.disk.as_str());
    if to_disk != Some("UpToDate") {
        let d = to_disk.unwrap_or("missing");
        return Ok(Next::Wait(format!("node {to:?} is {d}; waiting for UpToDate")));
    }

    // `to` is UpToDate! Now retire `from`
    if let Some(m) = from_in_nodes {
        if m.kind == NodeKind::Replica {
            check_r25(view, cluster, None)?;
            if let Some(st) = view.status.and_then(|s| s.nodes.iter().find(|n| n.name == from))
                && st.role == "Primary"
            {
                return Err(ChangeError::Primary(from.to_string()));
            }
            let n_replicas = replica_count(&view.spec.nodes);
            if n_replicas.saturating_sub(1) < 2 {
                return Err(ChangeError::TooFewReplicas);
            }

            let mut new_nodes = view.spec.nodes.clone();
            new_nodes.iter_mut().find(|n| n.name == from).unwrap().kind = NodeKind::Retiring;
            new_nodes.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(Next::Edit(Edit {
                nodes: new_nodes,
                add_finalizer: None,
            }));
        } else if m.kind == NodeKind::Retiring {
            let msg = view
                .status
                .and_then(|s| s.nodes.iter().find(|n| n.name == from))
                .map(|n| n.message.as_str())
                .unwrap_or("");
            let text = if msg.is_empty() {
                format!("waiting for node {from:?} to remove its copy")
            } else {
                format!("waiting for node {from:?} to remove its copy: {msg}")
            };
            return Ok(Next::Wait(text));
        }
    }

    if from_has_finalizer {
        return Ok(Next::Wait(format!("waiting for node {from:?} to remove its copy")));
    }

    balance_witness_or_done(view, cluster, existing)
}

#[cfg(test)]
#[path = "tests/replicas.rs"]
mod tests;
