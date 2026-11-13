//! Mapping of errors to the process exit code of the `ambar` binary.

use crate::reconcile::ReconcileError;

/// Exit code of `ambar apply` and `ambar remove` when another process holds
/// the volume lock: EX_TEMPFAIL from `sysexits.h`.
pub const EXIT_BUSY: u8 = 75;

/// Exit code for any failure other than a busy volume.
pub const EXIT_FAILURE: u8 = 1;

/// 75 when `err` is (or wraps) `ReconcileError::Busy`, 1 for anything else.
pub fn exit_code(err: &anyhow::Error) -> u8 {
    match err.downcast_ref::<ReconcileError>() {
        Some(ReconcileError::Busy { .. }) => EXIT_BUSY,
        _ => EXIT_FAILURE,
    }
}

#[cfg(test)]
#[path = "tests/cli.rs"]
mod tests;
