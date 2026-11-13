use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::exec::DEFAULT_COMMAND_TIMEOUT;
use crate::tests::{FakeRunner, out};

const THREE_UPTODATE: &str = include_str!("../../../stand/spike/samples/three-uptodate.json");

#[test]
fn commands_are_built_as_recorded_on_the_stand() {
    let runner = FakeRunner::new(|_, _| out(0, "", ""));
    let drbd = Drbd { runner: &runner };
    drbd.create_md("spike", 7, 1 << 30).unwrap();
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

/// Stderr recorded in experiment P7 (1) for a file without metadata.
const P7_NO_METADATA_STDERR: &str = "No valid meta data found\nCommand 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255\n";

#[test]
fn metadata_is_present_when_dump_md_succeeds() {
    let runner = FakeRunner::new(|_, _| out(0, "# DRBD meta data dump\n", ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p7"), Ok(Metadata::Present));
    assert_eq!(runner.calls(), ["drbdadm dump-md p7"]);
}

#[test]
fn metadata_is_absent_on_the_code_and_message_recorded_for_an_empty_file() {
    let runner = FakeRunner::new(|_, _| out(255, "", P7_NO_METADATA_STDERR));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p7"), Ok(Metadata::Absent));
}

#[test]
fn the_marker_alone_is_not_enough_when_the_command_succeeded() {
    let runner = FakeRunner::new(|_, _| out(0, "", P7_NO_METADATA_STDERR));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p7"), Ok(Metadata::Present));
}

#[test]
fn any_other_failure_of_dump_md_is_a_failed_check() {
    // Recorded in P7 (3): the resource is up, the device is busy.
    let busy = "open(/dev/ambar/p7) failed: Device or resource busy\nExclusive open failed. Do it anyways?\n";
    for (code, stderr) in [
        (20, busy),
        (20, "Device is configured"),
        (255, ""),
        (1, "boom"),
        (-1, ""),
    ] {
        let runner = FakeRunner::new(move |_, _| out(code, "", stderr));
        let drbd = Drbd { runner: &runner };
        let err = drbd.metadata("p7").unwrap_err();
        assert!(
            matches!(&err, DrbdError::MetadataCheck { volume, detail }
                if volume == "p7" && detail.contains(&format!("code {code}"))),
            "{err:?}"
        );
    }
}

#[test]
fn a_dump_md_that_cannot_start_is_a_failed_check() {
    struct Broken;
    impl Runner for Broken {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<Output> {
            Err(std::io::Error::other("no such program"))
        }
    }
    let drbd = Drbd { runner: &Broken };
    assert!(matches!(drbd.metadata("p7"), Err(DrbdError::Spawn { .. })));
}

#[test]
fn the_create_md_limit_is_120_seconds_plus_one_second_per_gib() {
    let gib = 1u64 << 30;
    assert_eq!(create_md_timeout(0), DEFAULT_COMMAND_TIMEOUT);
    assert_eq!(create_md_timeout(1), Duration::from_secs(121));
    assert_eq!(create_md_timeout(gib), Duration::from_secs(121));
    assert_eq!(create_md_timeout(gib + 1), Duration::from_secs(122));
    assert_eq!(create_md_timeout(1 << 40), Duration::from_secs(1144));
    assert_eq!(create_md_timeout(1 << 50), Duration::from_secs(120 + (1 << 20)));
}

/// Records the limit each command was started with.
struct LimitRecorder {
    calls: Mutex<Vec<(String, Option<Duration>)>>,
}

impl Runner for LimitRecorder {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<Output> {
        self.calls.lock().unwrap().push((command_line(program, args), None));
        Ok(out(0, "[\n]\n", ""))
    }

    fn run_with_timeout(&self, program: &str, args: &[&str], timeout: Duration) -> std::io::Result<Output> {
        self.calls
            .lock()
            .unwrap()
            .push((command_line(program, args), Some(timeout)));
        Ok(out(0, "", ""))
    }
}

#[test]
fn create_md_runs_with_a_limit_that_grows_with_the_volume() {
    let runner = LimitRecorder {
        calls: Mutex::new(Vec::new()),
    };
    let drbd = Drbd { runner: &runner };
    drbd.create_md("spike", 7, 1 << 40).unwrap();
    drbd.up("spike").unwrap();
    assert_eq!(
        *runner.calls.lock().unwrap(),
        [
            (
                "drbdadm create-md --force --max-peers=7 spike".to_string(),
                Some(Duration::from_secs(1144))
            ),
            ("drbdadm up spike".to_string(), None),
        ]
    );
}

#[test]
fn a_timeout_and_a_failed_start_both_read_as_a_command_that_did_not_complete() {
    struct TimesOut;
    impl Runner for TimesOut {
        fn run(&self, _: &str, _: &[&str]) -> std::io::Result<Output> {
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "killed after 120s"))
        }
    }
    let err = Drbd { runner: &TimesOut }.up("spike").unwrap_err();
    assert_eq!(
        err.to_string(),
        "`drbdadm up spike` did not run to completion: killed after 120s"
    );
}
