//! Automatic recovery of `StandAlone` connections for secondary nodes (R34, R35).
//!
//! When a volume has connections in `StandAlone` and the local role is `Secondary`,
//! the agent automatically attempts `drbdadm adjust` up to `--adjust-attempts` times
//! with an interval of `--adjust-interval`. If the attempts are exhausted, the split-brain
//! flag `manual_check_needed` is set. The flag automatically resets when all connections
//! become `Connected`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::drbd::adm::Drbd;
use crate::drbd::status::{ConnectionState, ResourceStatus, Role};

#[derive(Debug, PartialEq, Eq)]
pub enum StandaloneAction {
    /// No StandAlone connections or local role is not Secondary.
    None,
    /// Waiting for adjust interval before next attempt.
    Wait(Duration),
    /// Should run `drbdadm adjust <volume>`.
    RunAdjust { attempt: usize },
    /// All adjust attempts exhausted; manual intervention needed (split-brain).
    ManualCheckNeeded,
}

#[derive(Debug, Default)]
struct VolumeState {
    attempts_made: usize,
    last_attempt_at: Option<Instant>,
    manual_check_needed: bool,
}

#[derive(Debug)]
pub struct StandaloneTracker {
    max_attempts: usize,
    interval: Duration,
    states: HashMap<String, VolumeState>,
}

impl StandaloneTracker {
    pub fn new(max_attempts: usize, interval: Duration) -> Self {
        Self {
            max_attempts,
            interval,
            states: HashMap::new(),
        }
    }

    /// Checks the resource's connection state and decides whether an adjust is needed.
    pub fn check(&mut self, res: &ResourceStatus, now: Instant) -> StandaloneAction {
        let has_standalone = res
            .connections
            .iter()
            .any(|c| c.connection_state == ConnectionState::StandAlone);

        if !has_standalone {
            // Auto-reset when all connections become Connected (R35)
            if !res.connections.is_empty()
                && res
                    .connections
                    .iter()
                    .all(|c| c.connection_state == ConnectionState::Connected)
            {
                self.states.remove(&res.name);
            }
            return StandaloneAction::None;
        }

        // Only Secondary role attempts auto-adjust (R34)
        if res.role != Role::Secondary {
            return StandaloneAction::None;
        }

        let state = self.states.entry(res.name.clone()).or_default();
        if state.manual_check_needed {
            return StandaloneAction::ManualCheckNeeded;
        }

        if state.attempts_made < self.max_attempts {
            if let Some(last) = state.last_attempt_at {
                let elapsed = now.saturating_duration_since(last);
                if elapsed < self.interval {
                    return StandaloneAction::Wait(self.interval - elapsed);
                }
            }
            state.attempts_made += 1;
            state.last_attempt_at = Some(now);
            StandaloneAction::RunAdjust {
                attempt: state.attempts_made,
            }
        } else {
            state.manual_check_needed = true;
            StandaloneAction::ManualCheckNeeded
        }
    }

    /// Returns `true` if this volume exhausted adjust attempts and needs manual check.
    pub fn is_manual_check_needed(&self, volume: &str) -> bool {
        self.states.get(volume).is_some_and(|s| s.manual_check_needed)
    }

    pub fn forget(&mut self, volume: &str) {
        self.states.remove(volume);
    }
}

/// Executes auto-adjust if indicated by the tracker. Returns `true` if an adjust was performed.
pub fn handle_standalone(
    tracker: &mut StandaloneTracker,
    drbd: &Drbd<'_>,
    res: &ResourceStatus,
    now: Instant,
) -> Result<bool, String> {
    match tracker.check(res, now) {
        StandaloneAction::RunAdjust { attempt: _ } => {
            drbd.adjust(&res.name)
                .map_err(|e| format!("cannot adjust {}: {e}", res.name))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
#[path = "tests/standalone.rs"]
mod tests;
