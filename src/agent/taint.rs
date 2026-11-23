//! Management of the `ambar.balookrd.github.io/no-quorum:NoSchedule` node taint.
//!
//! R32: A node is tainted when at least one mounted/active volume (Primary or open device)
//! has lost quorum. The taint is removed once all mounted volumes have quorum (or no volumes
//! are mounted).

use std::time::{Duration, Instant};

use crate::agent::kube::{ApiError, Volumes};
use crate::drbd::status::{ResourceStatus, Role};

pub const NO_QUORUM_TAINT_KEY: &str = "ambar.balookrd.github.io/no-quorum";
pub const NO_QUORUM_TAINT_EFFECT: &str = "NoSchedule";

/// Periodic resynchronization interval for node taint state when `should_taint`
/// has not changed (protects against manual external edits without polling the
/// Kubernetes API every second).
pub const DEFAULT_TAINT_RESYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Tracks the last reconciled node taint state to avoid issuing a `GET /nodes/<node>`
/// API call on every failure-loop tick when nothing changed.
#[derive(Debug)]
pub struct TaintTracker {
    resync_interval: Duration,
    last_synced: Option<(bool, Instant)>,
}

impl TaintTracker {
    pub fn new(resync_interval: Duration) -> Self {
        Self {
            resync_interval,
            last_synced: None,
        }
    }

    pub fn needs_reconcile(&self, should_taint: bool, now: Instant) -> bool {
        match self.last_synced {
            None => true,
            Some((synced_state, synced_at)) => {
                synced_state != should_taint || now.saturating_duration_since(synced_at) >= self.resync_interval
            }
        }
    }

    pub fn record_synced(&mut self, tainted: bool, now: Instant) {
        self.last_synced = Some((tainted, now));
    }
}

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
