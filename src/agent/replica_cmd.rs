//! Execution of `ambar replica add`, `remove`, and `move`.
//!
//! Repeatedly reads the volume, cluster node map, and other volumes, then queries
//! pure rules (`replicas::{next_add, next_remove, next_move}`) to apply edits,
//! wait for state changes, or finish.

use std::io::Write;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::agent::kube::{ApiError, Volumes};
use crate::agent::nodes::NodeMap;
use crate::crd::MemberNode;
use crate::placement::existing_of;
use crate::replicas::{ChangeError, Edit, Next, View, next_add, next_move, next_remove};

/// Target action for the replica set management commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaAction {
    Add { volume: String, node: String },
    Remove { volume: String, node: String },
    Move { volume: String, from: String, to: String },
}

impl ReplicaAction {
    pub fn volume(&self) -> &str {
        match self {
            ReplicaAction::Add { volume, .. } => volume,
            ReplicaAction::Remove { volume, .. } => volume,
            ReplicaAction::Move { volume, .. } => volume,
        }
    }
}

/// Errors that can occur during replica command execution.
#[derive(Debug, thiserror::Error)]
pub enum ReplicaError {
    #[error("{0}")]
    Change(#[from] ChangeError),
    #[error("the volume {0:?} does not exist")]
    NotFound(String),
    #[error("the API server is unavailable: {0}")]
    Api(ApiError),
    #[error("timed out; run the same command again to continue")]
    TimedOut,
    #[error("interrupted")]
    Interrupted,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Formats the changes in `edit` relative to the previous `before` list of members.
pub fn describe_edit(before: &[MemberNode], edit: &Edit) -> String {
    let mut parts = Vec::new();
    for node in &edit.nodes {
        if let Some(old) = before.iter().find(|n| n.name == node.name) {
            if old.kind != node.kind {
                parts.push(format!("{} {:?} -> {:?}", node.name, old.kind, node.kind));
            }
        } else {
            parts.push(format!("+{} {:?}", node.name, node.kind));
        }
    }
    for old in before {
        if !edit.nodes.iter().any(|n| n.name == old.name) {
            parts.push(format!("-{} {:?}", old.name, old.kind));
        }
    }
    if let Some(finalizer) = &edit.add_finalizer {
        parts.push(format!("+finalizer {finalizer}"));
    }
    parts.join(", ")
}

async fn sleep_step(deadline: Option<Instant>, shutdown: &CancellationToken) -> Result<(), ReplicaError> {
    let now = Instant::now();
    let mut duration = Duration::from_secs(5);
    if let Some(dl) = deadline {
        if now >= dl {
            return Err(ReplicaError::TimedOut);
        }
        duration = duration.min(dl - now);
    }
    tokio::select! {
        _ = tokio::time::sleep(duration) => Ok(()),
        _ = shutdown.cancelled() => Err(ReplicaError::Interrupted),
    }
}

/// Executes the replica management loop, writing progress and success output to `out`.
pub async fn run_replica<W: Write>(
    volumes: &dyn Volumes,
    nodes: &dyn NodeMap,
    action: &ReplicaAction,
    timeout_secs: u64,
    out: &mut W,
    shutdown: CancellationToken,
) -> Result<(), ReplicaError> {
    let volume_name = action.volume();
    let mut seen_member = false;
    let mut consecutive_api_errors = 0;
    let mut last_wait: Option<(String, Instant)> = None;
    let deadline = if timeout_secs > 0 {
        Some(Instant::now() + Duration::from_secs(timeout_secs))
    } else {
        None
    };

    loop {
        if shutdown.is_cancelled() {
            return Err(ReplicaError::Interrupted);
        }
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            return Err(ReplicaError::TimedOut);
        }

        let get_res = volumes.get(volume_name).await;
        let fresh_res = volumes.list_fresh().await;
        let nodes_res = nodes.load().await;

        let (volume_opt, all_volumes, cluster_nodes) = match (get_res, fresh_res, nodes_res) {
            (Ok(v), Ok(f), Ok(n)) => {
                consecutive_api_errors = 0;
                (v, f, n)
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                consecutive_api_errors += 1;
                if consecutive_api_errors >= 3 {
                    return Err(ReplicaError::Api(e));
                }
                sleep_step(deadline, &shutdown).await?;
                continue;
            }
        };

        let Some(volume) = volume_opt else {
            return Err(ReplicaError::NotFound(volume_name.to_string()));
        };

        let existing: Vec<_> = all_volumes.iter().map(existing_of).collect();

        let view = View {
            spec: &volume.spec,
            status: volume.status.as_ref(),
            deleting: volume.metadata.deletion_timestamp.is_some(),
            finalizers: volume.metadata.finalizers.as_deref().unwrap_or(&[]),
        };

        if let ReplicaAction::Remove { node, .. } = action {
            let finalizer = crate::membership::replica_finalizer(node);
            if view.spec.nodes.iter().any(|m| &m.name == node) || view.finalizers.iter().any(|f| f == &finalizer) {
                seen_member = true;
            }
        }

        let next = match action {
            ReplicaAction::Add { node, .. } => next_add(&view, node, &cluster_nodes, &existing),
            ReplicaAction::Remove { node, .. } => next_remove(&view, node, seen_member, &cluster_nodes, &existing),
            ReplicaAction::Move { from, to, .. } => next_move(&view, from, to, &cluster_nodes, &existing),
        }?;

        match next {
            Next::Done => {
                match action {
                    ReplicaAction::Add { volume, node } => {
                        writeln!(out, "OK: {node} holds a copy of {volume}")?;
                    }
                    ReplicaAction::Remove { volume, node } => {
                        writeln!(out, "OK: {node} holds no copy of {volume}")?;
                    }
                    ReplicaAction::Move { volume, from, to } => {
                        writeln!(out, "OK: the copy of {volume} moved from {from} to {to}")?;
                    }
                }
                out.flush()?;
                return Ok(());
            }
            Next::Edit(edit) => {
                last_wait = None;
                let uid = volume.metadata.uid.as_deref().unwrap_or_default();
                let before = &view.spec.nodes;
                let edit_desc = describe_edit(before, &edit);
                let edit_res = volumes.edit_members(volume_name, uid, before, &edit).await;
                match edit_res {
                    Ok(true) => {
                        writeln!(out, "edit: {edit_desc}")?;
                        out.flush()?;
                    }
                    Ok(false) => {
                        // Conflict; next iteration re-reads fresh state and re-computes.
                    }
                    Err(e) => {
                        consecutive_api_errors += 1;
                        if consecutive_api_errors >= 3 {
                            return Err(ReplicaError::Api(e));
                        }
                        sleep_step(deadline, &shutdown).await?;
                    }
                }
            }
            Next::Wait(message) => {
                let now = Instant::now();
                let should_print = match &last_wait {
                    Some((prev_msg, prev_time)) => {
                        prev_msg != &message || now.duration_since(*prev_time) >= Duration::from_secs(30)
                    }
                    None => true,
                };
                if should_print {
                    writeln!(out, "{message}")?;
                    out.flush()?;
                    last_wait = Some((message, now));
                }
                sleep_step(deadline, &shutdown).await?;
            }
        }
    }
}

/// Runs the command and translates errors into an exit code: 0 on success, 1 on failure.
pub async fn execute<W: Write>(
    volumes: &dyn Volumes,
    nodes: &dyn NodeMap,
    action: &ReplicaAction,
    timeout_secs: u64,
    out: &mut W,
    shutdown: CancellationToken,
) -> u8 {
    match run_replica(volumes, nodes, action, timeout_secs, out, shutdown).await {
        Ok(()) => 0,
        Err(err) => {
            let _ = writeln!(out, "{err}");
            1
        }
    }
}

#[cfg(test)]
#[path = "tests/replica_cmd.rs"]
mod tests;
