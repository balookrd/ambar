use std::path::PathBuf;

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

#[test]
fn agent_command_defaults() {
    let cli = Cli::try_parse_from(["ambar", "agent", "--node-name", "dv-1"]).unwrap();
    match cli.command {
        Command::Agent {
            node_name,
            csi_socket,
            namespace,
            modules_dir,
            quorum_loss_grace_period,
            adjust_attempts,
            adjust_interval,
            metrics_addr,
        } => {
            assert_eq!(node_name, "dv-1");
            assert_eq!(csi_socket, PathBuf::from("/var/lib/kubelet/plugins/ambar/csi.sock"));
            assert_eq!(namespace, "ambar-system");
            assert_eq!(modules_dir, PathBuf::from("/opt/ambar/modules"));
            assert_eq!(quorum_loss_grace_period, 30);
            assert_eq!(adjust_attempts, 3);
            assert_eq!(adjust_interval, 5);
            assert_eq!(metrics_addr, "0.0.0.0:9770");
        }
        _ => panic!("expected Command::Agent"),
    }
}

#[test]
fn agent_command_explicit_flags() {
    let cli = Cli::try_parse_from([
        "ambar",
        "agent",
        "--node-name",
        "dv-2",
        "--quorum-loss-grace-period",
        "45",
        "--adjust-attempts",
        "5",
        "--adjust-interval",
        "10",
        "--metrics-addr",
        "192.168.104.2:9770",
    ])
    .unwrap();
    match cli.command {
        Command::Agent {
            node_name,
            quorum_loss_grace_period,
            adjust_attempts,
            adjust_interval,
            metrics_addr,
            ..
        } => {
            assert_eq!(node_name, "dv-2");
            assert_eq!(quorum_loss_grace_period, 45);
            assert_eq!(adjust_attempts, 5);
            assert_eq!(adjust_interval, 10);
            assert_eq!(metrics_addr, "192.168.104.2:9770");
        }
        _ => panic!("expected Command::Agent"),
    }
}
