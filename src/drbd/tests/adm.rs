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
    assert_eq!(
        runner.calls(),
        ["drbdsetup status spike --json", "drbdsetup status other --json"]
    );
}

#[test]
fn all_statuses_queries_all_resources() {
    let runner = FakeRunner::new(|_, _| out(0, THREE_UPTODATE, ""));
    let drbd = Drbd { runner: &runner };
    let all = drbd.all_statuses().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "spike");
    assert_eq!(runner.calls(), ["drbdsetup status --json"]);
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
            command: "drbdsetup status spike --json".into(),
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

#[test]
fn status_missing_resource_with_exit_code_10_is_none() {
    // Recorded on the stand (P3): `drbdsetup status <vol> --json` for a missing
    // resource exits with code 10, `[\n]\n\n` on stdout, and `<vol>: No such resource` on stderr.
    let runner = FakeRunner::new(|_, _| out(10, "[\n]\n\n", "spike: No such resource\n"));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.status("spike").unwrap(), None);

    let other_err = FakeRunner::new(|_, _| out(10, "", "No valid meta data found\n"));
    let drbd_err = Drbd { runner: &other_err };
    assert!(matches!(
        drbd_err.status("spike"),
        Err(DrbdError::Command { code: 10, .. })
    ));
}

/// Stderr recorded in experiment P7 (1) for a file without metadata.
const P7_NO_METADATA_STDERR: &str = "No valid meta data found\nCommand 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255\n";

/// Full stdout of `drbdadm dump-md p10` on `dv-1` in the states of
/// experiment P10, section "Опыты перед CSI (P7–P10)" of the experiment
/// record. Verbatim; for the states with `up` the dump taken after
/// `drbdadm apply-al`.
const P10_1_JUST_CREATED: &str = include_str!("fixtures/p10-1-just-created.txt");
const P10_1_JUST_CREATED_RUN2: &str = include_str!("fixtures/p10-1-just-created-run2.txt");
const P10_2_UP_DOWN: &str = include_str!("fixtures/p10-2-up-down.txt");
const P10_3_CONNECTED_INCONSISTENT: &str = include_str!("fixtures/p10-3-connected-inconsistent.txt");
const P10_4_SKIP_UPTODATE: &str = include_str!("fixtures/p10-4-skip-uptodate.txt");
const P10_4_SKIP_UPTODATE_RUN2: &str = include_str!("fixtures/p10-4-skip-uptodate-run2.txt");
const P10_5_FILESYSTEM_AND_FILE: &str = include_str!("fixtures/p10-5-filesystem-and-file.txt");
const P10_6_FULL_RESYNC: &str = include_str!("fixtures/p10-6-full-resync.txt");
/// Not a recorded dump and not produced by `drbdadm`: `p10-1-just-created.txt`
/// with `max-peers 3` and the bitmaps `[3]`-`[6]` (with their `bits-set`
/// comments) removed, to check that `Fresh` does not depend on `max-peers 7`.
const SYNTHETIC_MAX_PEERS_3: &str = include_str!("fixtures/synthetic-1-max-peers-3.txt");

/// Stderr recorded in P10 (2)-(6) for `dump-md` right after `drbdadm down`.
const P10_UNCLEAN_STDERR: &str = "Found meta data is \"unclean\", please apply-al first\nCommand 'drbdmeta 1010 v09 /dev/ambar/p10 internal dump-md' terminated with exit code 255\n";

#[test]
fn a_dump_md_that_succeeds_with_the_just_created_state_is_fresh() {
    let runner = FakeRunner::new(|_, _| out(0, P10_1_JUST_CREATED, ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p10"), Ok(Metadata::Fresh));
    assert_eq!(runner.calls(), ["drbdadm dump-md p10"]);
}

#[test]
fn a_dump_md_that_succeeds_with_a_used_state_is_used() {
    let runner = FakeRunner::new(|_, _| out(0, P10_5_FILESYSTEM_AND_FILE, ""));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p10"), Ok(Metadata::Used));
}

#[test]
fn metadata_is_absent_on_the_code_and_message_recorded_for_an_empty_file() {
    let runner = FakeRunner::new(|_, _| out(255, "", P7_NO_METADATA_STDERR));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p7"), Ok(Metadata::Absent));
}

#[test]
fn the_marker_alone_is_not_enough_when_the_command_succeeded() {
    // Exit code 0 with the P7 message on stderr and no dump on stdout: not
    // "absent", and an unreadable dump is never "fresh".
    let runner = FakeRunner::new(|_, _| out(0, "", P7_NO_METADATA_STDERR));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p7"), Ok(Metadata::Used));
}

#[test]
fn a_dump_md_that_succeeds_without_a_readable_dump_is_used() {
    for stdout in ["", "\n", "garbage\n", "# DRBD meta data dump\n"] {
        let runner = FakeRunner::new(move |_, _| out(0, stdout, ""));
        let drbd = Drbd { runner: &runner };
        assert_eq!(drbd.metadata("p7"), Ok(Metadata::Used), "{stdout:?}");
    }
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
fn unclean_metadata_is_used() {
    // P10 (2)-(6): `dump-md` after `up` and `down`; the activity log was not
    // applied, so the metadata was attached at least once.
    let runner = FakeRunner::new(|_, _| out(255, "", P10_UNCLEAN_STDERR));
    let drbd = Drbd { runner: &runner };
    assert_eq!(drbd.metadata("p10"), Ok(Metadata::Used));
    assert_eq!(runner.calls(), ["drbdadm dump-md p10"]);
}

#[test]
fn the_unclean_marker_wins_over_the_no_metadata_marker() {
    let both = format!("{P7_NO_METADATA_STDERR}{P10_UNCLEAN_STDERR}");
    let reversed = format!("{P10_UNCLEAN_STDERR}{P7_NO_METADATA_STDERR}");
    for (name, stderr) in [("no-metadata first", both), ("unclean first", reversed)] {
        let runner = FakeRunner::new(move |_, _| out(255, "", &stderr));
        let drbd = Drbd { runner: &runner };
        assert_eq!(drbd.metadata("p10"), Ok(Metadata::Used), "{name}");
    }
}

#[test]
fn stderr_markers_are_not_consulted_after_a_successful_exit() {
    for stderr in [P10_UNCLEAN_STDERR, P7_NO_METADATA_STDERR] {
        // A just-created dump stays Fresh whatever stderr says ...
        let runner = FakeRunner::new(move |_, _| out(0, P10_1_JUST_CREATED, stderr));
        assert_eq!(Drbd { runner: &runner }.metadata("p10"), Ok(Metadata::Fresh));
        // ... and an unreadable one is Used, never Absent.
        let runner = FakeRunner::new(move |_, _| out(0, "", stderr));
        assert_eq!(Drbd { runner: &runner }.metadata("p10"), Ok(Metadata::Used));
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

// ---- classify_dump on the P10 dumps ----

#[test]
fn only_the_state_right_after_create_md_is_fresh() {
    assert_eq!(classify_dump(P10_1_JUST_CREATED), Metadata::Fresh, "1 just created");
    assert_eq!(
        classify_dump(P10_1_JUST_CREATED_RUN2),
        Metadata::Fresh,
        "1 just created, run 2"
    );
}

#[test]
fn a_dump_of_the_just_created_state_with_another_max_peers_is_fresh() {
    assert_eq!(classify_dump(SYNTHETIC_MAX_PEERS_3), Metadata::Fresh);
    // The bitmap count must follow `max-peers` in both directions.
    let more = SYNTHETIC_MAX_PEERS_3.replacen("max-peers 3;", "max-peers 4;", 1);
    assert_eq!(classify_dump(&more), Metadata::Used);
    let fewer = SYNTHETIC_MAX_PEERS_3.replacen("max-peers 3;", "max-peers 2;", 1);
    assert_eq!(classify_dump(&fewer), Metadata::Used);
}

#[test]
fn metadata_that_was_attached_or_connected_is_used() {
    for (name, dump) in [
        ("2 up and down on one node", P10_2_UP_DOWN),
        ("3 connected, Inconsistent, no skip", P10_3_CONNECTED_INCONSISTENT),
    ] {
        assert_eq!(classify_dump(dump), Metadata::Used, "{name}");
    }
}

#[test]
fn dumps_of_metadata_that_took_part_in_a_volume_are_used() {
    for (name, dump) in [
        ("4 after the skip of the initial sync", P10_4_SKIP_UPTODATE),
        ("4 after the skip of the initial sync, run 2", P10_4_SKIP_UPTODATE_RUN2),
        ("5 filesystem and a file", P10_5_FILESYSTEM_AND_FILE),
        ("6 full resync onto the node", P10_6_FULL_RESYNC),
    ] {
        assert_eq!(classify_dump(dump), Metadata::Used, "{name}");
    }
}

#[test]
fn an_unreadable_dump_is_used() {
    assert_eq!(classify_dump(""), Metadata::Used);
    assert_eq!(classify_dump("\n\n"), Metadata::Used);
    assert_eq!(classify_dump("No valid meta data found\n"), Metadata::Used);
    assert_eq!(classify_dump("not a dump at all"), Metadata::Used);
    // Exactly the part of a dump that the layout check does not need.
    assert_eq!(
        classify_dump("# DRBD meta data dump\n\nversion \"v09\";\n"),
        Metadata::Used
    );
}

#[test]
fn every_truncation_of_a_fresh_dump_is_used() {
    let lines: Vec<&str> = P10_1_JUST_CREATED.lines().collect();
    for keep in 0..lines.len() {
        let cut = lines[..keep].join("\n");
        assert_eq!(classify_dump(&cut), Metadata::Used, "cut after {keep} lines");
    }
    assert_eq!(classify_dump(&lines.join("\n")), Metadata::Fresh);
    // Cut in the middle of the last line.
    let text = P10_1_JUST_CREATED.trim_end();
    assert_eq!(classify_dump(&text[..text.len() - 1]), Metadata::Used);
}

/// `P10_1_JUST_CREATED` with the first line that equals `from` replaced by `to`.
fn with_line(from: &str, to: &str) -> String {
    let mut done = false;
    let lines: Vec<String> = P10_1_JUST_CREATED
        .lines()
        .map(|l| {
            if !done && l.trim() == from {
                done = true;
                to.to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    assert!(done, "no line {from:?} in the fixture");
    lines.join("\n") + "\n"
}

#[test]
fn one_changed_identifier_makes_a_fresh_dump_used() {
    for (name, from, to) in [
        (
            "current-uuid",
            "current-uuid 0x0000000000000004;",
            "current-uuid 0x0000000000000005;",
        ),
        (
            "current-uuid, a generated one",
            "current-uuid 0x0000000000000004;",
            "current-uuid 0x0240DDD21A4B2CE8;",
        ),
        (
            "current-uuid zero",
            "current-uuid 0x0000000000000004;",
            "current-uuid 0x0000000000000000;",
        ),
        ("flags of the node", "flags 0x00000080;", "flags 0x00000091;"),
        (
            "flags of the node, consistent only",
            "flags 0x00000080;",
            "flags 0x00000081;",
        ),
        ("la-size-sect", "la-size-sect 0;", "la-size-sect 2096632;"),
        (
            "bitmap-uuid of a slot",
            "bitmap-uuid 0x0000000000000000;",
            "bitmap-uuid 0x5400D259A0DF9D13;",
        ),
        (
            "bitmap-dagtag of a slot",
            "bitmap-dagtag 0x0000000000000000;",
            "bitmap-dagtag 0x0000000000000001;",
        ),
        ("flags of a slot", "flags 0x00000000;", "flags 0x00000020;"),
        (
            "flags of the node, as after up (P10 2)",
            "flags 0x00000080;",
            "flags 0x00000480;",
        ),
        (
            "flags of a slot, exists (P10 3)",
            "flags 0x00000000;",
            "flags 0x00010000;",
        ),
        (
            "flags of a slot, outdated and seen (P10 3)",
            "flags 0x00000000;",
            "flags 0x00000052;",
        ),
        (
            "flags of a slot, any non-zero bit",
            "flags 0x00000000;",
            "flags 0x00000001;",
        ),
        ("node-id", "node-id -1;", "node-id 0;"),
        ("members", "members 0x0000000000000000;", "members 0x0000000000000001;"),
        (
            "features",
            "features 0x0000000000000001;",
            "features 0x0000000000000003;",
        ),
        ("bitmap-index of a slot", "bitmap-index -1;", "bitmap-index 0;"),
        (
            "la-peer-max-bio-size",
            "la-peer-max-bio-size 0;",
            "la-peer-max-bio-size 1048576;",
        ),
        ("bm-byte-per-bit", "bm-byte-per-bit 4096;", "bm-byte-per-bit 8192;"),
        ("al-stripes", "al-stripes 1;", "al-stripes 2;"),
        ("al-stripe-size-4k", "al-stripe-size-4k 8;", "al-stripe-size-4k 16;"),
        (
            "hex with a sign",
            "current-uuid 0x0000000000000004;",
            "current-uuid 0x+4;",
        ),
        (
            "hex with a sign in a slot",
            "bitmap-uuid 0x0000000000000000;",
            "bitmap-uuid 0x+0;",
        ),
        ("hex without digits", "members 0x0000000000000000;", "members 0x;"),
        ("bits set in the bitmap", "# bits-set[0] 0;", "# bits-set[0] 1024;"),
        ("layout version", "version \"v09\";", "version \"v10\";"),
        ("max-peers does not match the bitmaps", "max-peers 7;", "max-peers 6;"),
    ] {
        assert_eq!(classify_dump(&with_line(from, to)), Metadata::Used, "{name}");
    }
}

#[test]
fn a_set_bit_in_a_bitmap_body_makes_a_fresh_dump_used() {
    let set = P10_1_JUST_CREATED.replacen(
        "bitmap[0] {\n}",
        "bitmap[0] {\n   # at 0kB\n    4096 times 0xFFFFFFFFFFFFFFFF;\n}",
        1,
    );
    assert_ne!(set, P10_1_JUST_CREATED);
    assert_eq!(classify_dump(&set), Metadata::Used);
}

#[test]
fn a_changed_identifier_in_the_last_peer_slot_or_the_history_makes_a_fresh_dump_used() {
    // Last slot: the 32nd `bitmap-uuid` line.
    let mut seen = 0;
    let slot: Vec<String> = P10_1_JUST_CREATED
        .lines()
        .map(|l| {
            if l.trim() == "bitmap-uuid 0x0000000000000000;" {
                seen += 1;
                if seen == 32 {
                    return "    bitmap-uuid 0x0000000000000001;".to_string();
                }
            }
            l.to_string()
        })
        .collect();
    assert_eq!(seen, 32);
    assert_eq!(classify_dump(&slot.join("\n")), Metadata::Used);
    // History: the first value of the first history line.
    let history = P10_1_JUST_CREATED.replacen(
        "        0x0000000000000000; 0x0000000000000000;",
        "        0x0240DDD21A4B2CE8; 0x0000000000000000;",
        1,
    );
    assert_ne!(history, P10_1_JUST_CREATED);
    assert_eq!(classify_dump(&history), Metadata::Used);
}

#[test]
fn a_dump_with_an_unknown_line_or_a_missing_field_is_used() {
    let extra = P10_1_JUST_CREATED.replacen("max-peers 7;", "max-peers 7;\nsomething-new 1;", 1);
    assert_eq!(classify_dump(&extra), Metadata::Used);
    let no_uuid: String = P10_1_JUST_CREATED
        .lines()
        .filter(|l| !l.starts_with("current-uuid"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(classify_dump(&no_uuid), Metadata::Used);
    // A complete dump followed by the start of one more block that never ends.
    for tail in ["bitmap[7] {\n", "peer[32] {\n    bitmap-index -1;\n"] {
        assert_eq!(
            classify_dump(&format!("{P10_1_JUST_CREATED}{tail}")),
            Metadata::Used,
            "{tail}"
        );
    }
    let two_uuids = P10_1_JUST_CREATED.replacen(
        "current-uuid 0x0000000000000004;",
        "current-uuid 0x0000000000000004;\ncurrent-uuid 0x0000000000000004;",
        1,
    );
    assert_eq!(classify_dump(&two_uuids), Metadata::Used);
}

#[test]
fn a_repeated_top_level_key_makes_a_fresh_dump_used() {
    for (key, line) in [
        ("version", "version \"v09\";"),
        ("max-peers", "max-peers 7;"),
        ("node-id", "node-id -1;"),
        ("flags", "flags 0x00000080;"),
        ("members", "members 0x0000000000000000;"),
        ("features", "features 0x0000000000000001;"),
        ("la-size-sect", "la-size-sect 0;"),
        ("bm-byte-per-bit", "bm-byte-per-bit 4096;"),
        ("device-uuid", "device-uuid 0x9A9C813A219E8F2D;"),
        ("la-peer-max-bio-size", "la-peer-max-bio-size 0;"),
        ("al-stripes", "al-stripes 1;"),
        ("al-stripe-size-4k", "al-stripe-size-4k 8;"),
    ] {
        let repeated = P10_1_JUST_CREATED.replacen(line, &format!("{line}\n{line}"), 1);
        assert_ne!(repeated, P10_1_JUST_CREATED, "no {key} line");
        assert_eq!(classify_dump(&repeated), Metadata::Used, "{key}");
    }
    // A wrong `version` first and the right one after it is not a layout we know.
    let two = P10_1_JUST_CREATED.replacen("version \"v09\";", "version \"v10\";\nversion \"v09\";", 1);
    assert_eq!(classify_dump(&two), Metadata::Used);
}

#[test]
fn comments_and_the_random_device_uuid_do_not_matter() {
    let changed = P10_1_JUST_CREATED
        .replace("# 2026-11-13", "# 2030-01-01")
        .replace("device-uuid 0x9A9C813A219E8F2D;", "device-uuid 0x1111111111111111;");
    assert_ne!(changed, P10_1_JUST_CREATED);
    assert_eq!(classify_dump(&changed), Metadata::Fresh);
}
