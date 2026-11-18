//! Tracking DRBD quorum loss and timing the grace period before pod eviction (R33).

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuorumAction {
    /// Quorum is healthy or the volume is not mounted.
    None,
    /// Quorum lost, but still within grace period. Carries elapsed duration.
    WithinGracePeriod(Duration),
    /// Grace period expired. Eviction of pods on this node must be triggered now.
    EvictPods,
    /// Eviction was already triggered for this quorum loss incident.
    AlreadyEvicted,
}

#[derive(Debug)]
pub struct QuorumTracker {
    grace_period: Duration,
    /// Maps volume name to `(quorum_lost_at, eviction_triggered)`.
    lost_at: HashMap<String, (Instant, bool)>,
}

impl QuorumTracker {
    pub fn new(grace_period: Duration) -> Self {
        Self {
            grace_period,
            lost_at: HashMap::new(),
        }
    }

    /// Updates the state of `volume` and returns the action to be taken.
    pub fn update(&mut self, volume: &str, is_mounted: bool, has_quorum: bool, now: Instant) -> QuorumAction {
        if !is_mounted || has_quorum {
            self.lost_at.remove(volume);
            return QuorumAction::None;
        }

        match self.lost_at.get_mut(volume) {
            Some((started_at, triggered)) => {
                if *triggered {
                    QuorumAction::AlreadyEvicted
                } else {
                    let elapsed = now.saturating_duration_since(*started_at);
                    if elapsed >= self.grace_period {
                        *triggered = true;
                        QuorumAction::EvictPods
                    } else {
                        QuorumAction::WithinGracePeriod(elapsed)
                    }
                }
            }
            None => {
                if self.grace_period == Duration::ZERO {
                    self.lost_at.insert(volume.to_string(), (now, true));
                    QuorumAction::EvictPods
                } else {
                    self.lost_at.insert(volume.to_string(), (now, false));
                    QuorumAction::WithinGracePeriod(Duration::ZERO)
                }
            }
        }
    }

    pub fn forget(&mut self, volume: &str) {
        self.lost_at.remove(volume);
    }
}

pub struct VolumeQuorumCheck<'a> {
    pub node: &'a str,
    pub volume: &'a str,
    pub is_mounted: bool,
    pub has_quorum: bool,
    pub mounted_uids: &'a [String],
    pub now: Instant,
}

/// Handles quorum checking for a volume: updates the tracker and, if the grace period
/// has expired, calls `delete_node_pods_for_volume` to release the volume.
pub async fn check_volume_quorum(
    tracker: &mut QuorumTracker,
    volumes: &dyn crate::agent::kube::Volumes,
    check: VolumeQuorumCheck<'_>,
) -> Result<Option<Vec<String>>, crate::agent::kube::ApiError> {
    match tracker.update(check.volume, check.is_mounted, check.has_quorum, check.now) {
        QuorumAction::EvictPods => {
            let deleted = volumes
                .delete_node_pods_for_volume(check.node, check.volume, check.mounted_uids)
                .await?;
            Ok(Some(deleted))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
#[path = "tests/quorum.rs"]
mod tests;
