use super::*;
use crate::tests::{FakeRunner, out};

const THREE_UPTODATE: &str = include_str!("../../../stand/spike/samples/three-uptodate.json");

#[test]
fn commands_are_built_as_recorded_on_the_stand() {
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    let drbd = Drbd { runner: &runner };
    drbd.create_md("spike", 7).unwrap();
    drbd.up("spike").unwrap();
    drbd.adjust("spike").unwrap();
    drbd.skip_initial_sync("spike").unwrap();
    drbd.down("spike").unwrap();
    assert_eq!(
        runner.calls(),
        [
            "drbdadm create-md --force --max-peers=7 spike",
            "drbdadm up spike",
            "drbdadm adjust spike",
            "drbdadm new-current-uuid --clear-bitmap spike/0",
            "drbdadm down spike",
        ]
    );
}

#[test]
fn a_failing_command_reports_its_code_and_stderr() {
    let runner = FakeRunner::new(|_, _| out(10, "", "No valid meta data found\n"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(
        drbd.up("spike"),
        Err(DrbdError::Command {
            command: "drbdadm up spike".into(),
            code: 10,
            stderr: "No valid meta data found".into(),
        })
    );
}

#[test]
fn adjust_hands_back_the_output_instead_of_judging_the_code() {
    let runner = FakeRunner::new(|_, _| out(1, "", "whatever"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.adjust("spike").unwrap().code, 1);
}

#[test]
fn status_finds_the_resource_by_name() {
    let runner = FakeRunner::new(|_, _| out(0, THREE_UPTODATE, ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.status("spike").unwrap().map(|s| s.node_id), Some(0));
    assert_eq!(drbd.status("other").unwrap(), None);
    assert_eq!(runner.calls()[0], "drbdsetup status --json");
}

#[test]
fn status_without_configured_resources_is_none() {
    // Recorded on the stand: an empty JSON array and a blank line.
    let runner = FakeRunner::new(|_, _| out(0, "[\n]\n\n", ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.status("spike").unwrap(), None);
}

#[test]
fn status_rejects_output_that_is_not_json() {
    let runner = FakeRunner::new(|_, _| out(0, "# No currently configured DRBD found.\n", ""));
    let drbd = Drbd { runner: &runner };
    let err = drbd.status("spike").unwrap_err();
    assert!(
        matches!(&err, DrbdError::Status(text) if text.contains("unexpected output")),
        "{err}"
    );
}

#[test]
fn status_fails_when_drbdsetup_fails() {
    let runner = FakeRunner::new(|_, _| out(20, "", "modprobe: FATAL: Module drbd not found\n"));
    let drbd = Drbd { runner: &runner };
    assert!(matches!(drbd.status("spike"), Err(DrbdError::Command { code: 20, .. })));
}

#[test]
fn status_trusts_the_exit_code_before_stdout() {
    let runner = FakeRunner::new(|_, _| out(1, "[\n]\n", "boom\n"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(
        drbd.status("spike"),
        Err(DrbdError::Command {
            command: "drbdsetup status --json".into(),
            code: 1,
            stderr: "boom".into(),
        })
    );
}

#[test]
fn status_reports_malformed_json_after_a_successful_exit() {
    let runner = FakeRunner::new(|_, _| out(0, "[ {\"name\": ", ""));
    let drbd = Drbd { runner: &runner };
    assert!(matches!(drbd.status("spike"), Err(DrbdError::Status(_))));
}
