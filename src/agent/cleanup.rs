//! Cleanup of leaked or stale `Client` entries (R36).
//!
//! When a node is listed as a `Client` in `spec.nodes`, but no pods on this node
//! use the volume (e.g. after a node reboot or an unexpected pod deletion),
//! the agent safely tears down the DRBD resource (`drbdadm down`), removes its
//! entry from `status.nodes`, and removes itself from `spec.nodes` via a JSON patch.

use crate::agent::kube::{ApiError, EDIT_ATTEMPTS, Volumes, conflict_backoff, mounted_pod_uids_for_volume};
use crate::agent::run::{Agent, AgentError, Outcome, uid_of};
use crate::crd::{NodeKind, ReplicatedVolume, ReplicatedVolumeSpec};
use crate::drbd::status::{ResourceStatus, Role};
use crate::membership::{Leave, leave_as_client};

/// Determines whether this node's `Client` entry should be cleaned up.
///
/// Returns `true` only if:
/// 1. This node is recorded as a `Client` in `spec.nodes`;
/// 2. No pods on this node are using the volume (`has_node_pods == false`);
/// 3. The local DRBD resource is not in use:
///    - If DRBD status is present: `role` is not `Primary` and `open` is not `Some(true)`;
///    - If DRBD status is absent: considered not in use.
pub fn should_cleanup_client(
    node: &str,
    spec: &ReplicatedVolumeSpec,
    status: Option<&ResourceStatus>,
    has_node_pods: bool,
) -> bool {
    let is_client = spec.nodes.iter().any(|n| n.name == node && n.kind == NodeKind::Client);
    if !is_client {
        return false;
    }
    if has_node_pods {
        return false;
    }
    match status {
        Some(status) => {
            if status.role == Role::Primary {
                return false;
            }
            if status.devices.iter().any(|d| d.open) {
                return false;
            }
            true
        }
        None => true,
    }
}

/// Checks if the volume on this node is a stale `Client` without pods, and cleans it up.
///
/// Returns `Ok(Some(outcome))` if cleanup was performed, `Ok(None)` if no cleanup was needed.
pub async fn check_and_cleanup_client(
    agent: &Agent,
    volume: &str,
    object: &ReplicatedVolume,
) -> Result<Option<Outcome>, AgentError> {
    if !crate::model::valid_volume_name(volume) {
        return Ok(None);
    }
    let is_client = object.metadata.deletion_timestamp.is_none()
        && object
            .spec
            .nodes
            .iter()
            .any(|n| n.name == agent.node && n.kind == NodeKind::Client);
    if !is_client {
        return Ok(None);
    }
    let _lock = agent.locks.lock(volume).await;
    let status = match agent.read_status(volume).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ambar-agent: warning: volume {volume:?}: cannot read status for cleanup: {e}");
            return Err(AgentError::Reconcile {
                volume: volume.to_string(),
                source: e,
            });
        }
    };
    let is_busy = status
        .as_ref()
        .is_some_and(|s| s.role == Role::Primary || s.devices.iter().any(|d| d.open));
    if is_busy {
        return Ok(None);
    }
    let mounted_uids = mounted_pod_uids_for_volume(std::path::Path::new("/var/lib/kubelet/pods"), volume);
    let has_pods = agent
        .volumes
        .has_node_pods_for_volume(&agent.node, volume, &mounted_uids)
        .await?;
    if should_cleanup_client(&agent.node, &object.spec, status.as_ref(), has_pods) {
        let outcome = agent.cleanup_stale_client(volume, object).await?;
        return Ok(Some(outcome));
    }
    Ok(None)
}

/// Removes `node` from `spec.nodes` of `volume` if it is a `Client`.
/// Retries on conflict up to `EDIT_ATTEMPTS` times.
pub async fn leave_client(volumes: &dyn Volumes, node: &str, volume: &str) -> Result<bool, ApiError> {
    for attempt in 0..EDIT_ATTEMPTS {
        let Some(object) = volumes.get(volume).await? else {
            return Ok(true);
        };
        let mut spec = object.spec.clone();
        if leave_as_client(&mut spec, node) != Leave::Removed {
            return Ok(true);
        }
        let uid = uid_of(&object).map_err(|e| match e {
            AgentError::Api(err) => err,
            other => ApiError::Other(other.to_string()),
        })?;
        let edited = volumes
            .replace_members(volume, &uid, &object.spec.nodes, &spec.nodes)
            .await?;
        if edited {
            return Ok(true);
        }
        if attempt + 1 < EDIT_ATTEMPTS {
            tokio::time::sleep(conflict_backoff(attempt)).await;
        }
    }
    Err(ApiError::Conflict(format!(
        "volume {volume:?} kept changing while removing stale client on node {node}"
    )))
}

#[cfg(test)]
#[path = "tests/cleanup.rs"]
mod tests;
