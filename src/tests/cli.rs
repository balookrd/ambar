use anyhow::Context as _;

use super::*;
use crate::model::SpecError;
use crate::reconcile::PlanError;

fn busy() -> ReconcileError {
    ReconcileError::Busy { volume: "spike".into() }
}

#[test]
fn a_busy_volume_exits_with_75() {
    assert_eq!(EXIT_BUSY, 75);
    assert_eq!(exit_code(&anyhow::Error::from(busy())), EXIT_BUSY);
}

#[test]
fn a_busy_volume_keeps_its_exit_code_under_added_context() {
    let err = Err::<(), _>(busy()).context("while applying").unwrap_err();
    assert_eq!(exit_code(&err), EXIT_BUSY);
}

#[test]
fn every_other_error_exits_with_1() {
    let errors = [
        anyhow::Error::from(ReconcileError::NotApplied {
            command: "up",
            volume: "spike".into(),
        }),
        anyhow::Error::from(ReconcileError::NoLoopDevice { volume: "spike".into() }),
        anyhow::Error::from(ReconcileError::from(PlanError::NotAMember {
            node: "n".into(),
            volume: "spike".into(),
        })),
        anyhow::Error::from(ReconcileError::from(SpecError::Name("all".into()))),
        anyhow::anyhow!("cannot read the file"),
    ];
    for err in errors {
        assert_eq!(exit_code(&err), 1, "{err}");
    }
}
