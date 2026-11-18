//! Management of the `ambar.balookrd.github.io/no-quorum:NoSchedule` node taint.
//!
//! R32: A node is tainted when at least one mounted/active volume (Primary or open device)
//! has lost quorum. The taint is removed once all mounted volumes have quorum (or no volumes
//! are mounted).

use crate::agent::kube::{ApiError, Volumes};
use crate::drbd::status::{ResourceStatus, Role};

pub const NO_QUORUM_TAINT_KEY: &str = "ambar.balookrd.github.io/no-quorum";
pub const NO_QUORUM_TAINT_EFFECT: &str = "NoSchedule";

/// Decides whether the node should have the `no-quorum` taint based on current DRBD resources.
///
/// Returns `true` if any volume that is locally mounted or Primary lacks DRBD quorum.
pub fn should_taint_node(resources: &[ResourceStatus]) -> bool {
    resources.iter().any(|res| {
        let is_mounted = res.role == Role::Primary || res.device().is_some_and(|d| d.open);
        if !is_mounted {
            return false;
        }
        match res.device() {
            Some(dev) => !dev.quorum,
            None => true, // Primary without a device object cannot be confirmed to have quorum.
        }
    })
}

/// Ensures or removes the `ambar.balookrd.github.io/no-quorum:NoSchedule` taint on `node_name`.
/// Returns `Ok(true)` if the node's taint state changed, `Ok(false)` if already in desired state.
pub async fn reconcile_node_taint(
    volumes: &dyn Volumes,
    node_name: &str,
    should_taint: bool,
) -> Result<bool, ApiError> {
    if should_taint {
        volumes
            .ensure_node_taint(node_name, NO_QUORUM_TAINT_KEY, NO_QUORUM_TAINT_EFFECT)
            .await
    } else {
        volumes
            .remove_node_taint(node_name, NO_QUORUM_TAINT_KEY, NO_QUORUM_TAINT_EFFECT)
            .await
    }
}

#[cfg(test)]
#[path = "tests/taint.rs"]
mod tests;
