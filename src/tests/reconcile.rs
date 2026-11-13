use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;
use crate::backing::backing_file_bytes;
use crate::drbd::status::{self, ConnectionState, DiskState};
use crate::exec::Output;
use crate::tests::{FakeRunner, out};

const SPEC: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "initialSync": "Skip",
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

/// Captured on lima-dv-1 (node id 0) of a volume with three copies.
const THREE_INCONSISTENT: &str = include_str!("../../stand/spike/samples/three-inconsistent.json");
const THREE_UPTODATE: &str = include_str!("../../stand/spike/samples/three-uptodate.json");

/// Stderr recorded in experiment P7 (1) for a backing file without metadata.
const P7_NO_METADATA_STDERR: &str = "No valid meta data found\nCommand 'drbdmeta 1007 v09 /dev/ambar/p7 internal dump-md' terminated with exit code 255\n";

fn spec() -> VolumeSpec {
    VolumeSpec::from_json(SPEC).unwrap()
}

fn status_of(json: &str) -> ResourceStatus {
    status::parse(json).unwrap().into_iter().next().unwrap()
}

/// A sample captured on lima-dv-1, rewritten as `node` of `SPEC` would see
/// the same volume: its own node id, one connection per other node, a
/// diskless peer reported as a client, and a client device on a node
/// without a disk.
fn seen_from(node: &str, json: &str) -> ResourceStatus {
    let spec = spec();
    let me = spec.node(node).unwrap();
    let mut status = status_of(json);
    let template = status.connections[0].clone();
    status.node_id = me.node_id;
    status.connections = spec
        .nodes
        .iter()
        .filter(|peer| peer.name != node)
        .map(|peer| {
            let mut conn = template.clone();
            conn.name = peer.name.clone();
            conn.peer_node_id = peer.node_id;
            if !peer.diskful {
                for device in &mut conn.peer_devices {
                    device.peer_client = true;
                    device.peer_disk_state = DiskState::Diskless;
                }
            }
            conn
        })
        .collect();
    if !me.diskful {
        let device = status.devices.iter_mut().find(|d| d.volume == 0).unwrap();
        device.client = true;
        device.disk_state = DiskState::Diskless;
    }
    status
}

/// A settled volume as `node` observes it.
fn steady_on(node: &str) -> Observed {
    Observed {
        file: FileState::Ready,
        loop_device: Some("/dev/loop3".into()),
        link_target: Some("/dev/loop3".into()),
        res_matches: true,
        res_applied: true,
        synced: true,
        stale_markers: false,
        status: Some(seen_from(node, THREE_UPTODATE)),
    }
}

fn steady() -> Observed {
    steady_on("lima-dv-1")
}

/// Everything configured and connected, no copy synced yet: the status
/// gate holds on the leader.
fn fresh_on(node: &str) -> Observed {
    Observed {
        synced: false,
        status: Some(seen_from(node, THREE_INCONSISTENT)),
        ..steady_on(node)
    }
}

fn nothing_on_the_node() -> Observed {
    Observed {
        file: FileState::Absent,
        loop_device: None,
        link_target: None,
        res_matches: false,
        res_applied: false,
        synced: false,
        stale_markers: false,
        status: None,
    }
}

// ---- plan ----

#[test]
fn a_new_copy_is_created_initialised_and_brought_up() {
    let bytes = backing_file_bytes(1_073_741_824, 7);
    assert_eq!(
        plan(&spec(), "lima-dv-1", &nothing_on_the_node()).unwrap(),
        [
            Action::CreateFile { bytes },
            Action::AttachLoop,
            Action::LinkDevice,
            Action::WriteRes,
            Action::CreateMd,
            Action::MarkReady,
            Action::Up,
            Action::MarkApplied,
        ]
    );
}

#[test]
fn after_a_reboot_the_loop_device_is_restored_without_touching_metadata() {
    let obs = Observed {
        loop_device: None,
        link_target: None,
        status: None,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::AttachLoop, Action::LinkDevice, Action::Up, Action::MarkApplied]
    );
}

#[test]
fn an_interrupted_initialisation_is_finished() {
    let obs = Observed {
        file: FileState::Pending,
        synced: false,
        status: None,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::CreateMd, Action::MarkReady, Action::Up, Action::MarkApplied]
    );
}

#[test]
fn metadata_is_never_created_under_a_configured_resource() {
    for file in [FileState::Pending, FileState::Absent] {
        let obs = Observed {
            file,
            synced: false,
            ..steady()
        };
        assert_eq!(
            plan(&spec(), "lima-dv-1", &obs),
            Err(PlanError::UninitialisedFileWithLiveResource { volume: "spike".into() })
        );
    }
}

#[test]
fn a_file_without_the_ready_marker_but_with_the_synced_marker_is_an_error() {
    for status in [None, Some(seen_from("lima-dv-1", THREE_UPTODATE))] {
        let obs = Observed {
            file: FileState::Pending,
            synced: true,
            status,
            ..steady()
        };
        assert_eq!(
            plan(&spec(), "lima-dv-1", &obs),
            Err(PlanError::ReadyMarkerLost { volume: "spike".into() })
        );
    }
}

#[test]
fn stale_markers_of_a_missing_image_are_cleared_before_the_file_is_created() {
    let bytes = backing_file_bytes(1_073_741_824, 7);
    for synced in [true, false] {
        let obs = Observed {
            res_matches: true,
            res_applied: true,
            synced,
            stale_markers: true,
            ..nothing_on_the_node()
        };
        assert_eq!(
            plan(&spec(), "lima-dv-1", &obs).unwrap(),
            [
                Action::ClearStaleMarkers,
                Action::CreateFile { bytes },
                Action::AttachLoop,
                Action::LinkDevice,
                Action::CreateMd,
                Action::MarkReady,
                Action::Up,
                Action::MarkApplied,
            ]
        );
    }
}

#[test]
fn a_node_without_a_disk_only_needs_the_resource() {
    assert_eq!(
        plan(&spec(), "lima-dv-3", &nothing_on_the_node()).unwrap(),
        [Action::WriteRes, Action::Up, Action::MarkApplied]
    );
}

#[test]
fn a_changed_description_is_applied_with_adjust() {
    for node in ["lima-dv-1", "lima-dv-3"] {
        let obs = Observed {
            res_matches: false,
            res_applied: false,
            ..steady_on(node)
        };
        assert_eq!(
            plan(&spec(), node, &obs).unwrap(),
            [Action::WriteRes, Action::Adjust, Action::MarkApplied]
        );
    }
}

#[test]
fn a_resource_file_that_was_never_applied_is_applied_with_adjust() {
    for node in ["lima-dv-1", "lima-dv-2", "lima-dv-3"] {
        let obs = Observed {
            res_applied: false,
            ..steady_on(node)
        };
        assert_eq!(
            plan(&spec(), node, &obs).unwrap(),
            [Action::Adjust, Action::MarkApplied]
        );
    }
}

#[test]
fn a_status_that_differs_from_the_description_is_adjusted_and_never_skipped() {
    let mut other_minor = seen_from("lima-dv-1", THREE_INCONSISTENT);
    other_minor.devices[0].minor = 1001;
    let mut other_id = seen_from("lima-dv-1", THREE_INCONSISTENT);
    other_id.node_id = 5;
    let mut missing_peer = seen_from("lima-dv-1", THREE_INCONSISTENT);
    missing_peer.connections.pop();
    for status in [other_minor, other_id, missing_peer] {
        assert!(status.ready_to_skip_initial_sync(), "{status:?}");
        let obs = Observed {
            status: Some(status),
            ..fresh_on("lima-dv-1")
        };
        assert_eq!(
            plan(&spec(), "lima-dv-1", &obs).unwrap(),
            [Action::Adjust, Action::MarkApplied]
        );
    }
}

#[test]
fn a_changed_resource_file_blocks_the_skip_in_that_pass() {
    let obs = Observed {
        res_matches: false,
        ..fresh_on("lima-dv-1")
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::WriteRes, Action::Adjust, Action::MarkApplied]
    );
}

#[test]
fn any_other_pending_step_blocks_the_skip_in_that_pass() {
    let obs = Observed {
        link_target: Some("/dev/loop9".into()),
        ..fresh_on("lima-dv-1")
    };
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::LinkDevice]);
}

#[test]
fn a_stale_device_link_is_repointed() {
    let obs = Observed {
        link_target: Some("/dev/loop9".into()),
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::LinkDevice]);
}

#[test]
fn a_settled_volume_needs_nothing() {
    for node in ["lima-dv-1", "lima-dv-2", "lima-dv-3"] {
        assert_eq!(plan(&spec(), node, &steady_on(node)).unwrap(), []);
    }
}

#[test]
fn only_the_lowest_diskful_node_skips_the_initial_sync() {
    assert_eq!(
        plan(&spec(), "lima-dv-1", &fresh_on("lima-dv-1")).unwrap(),
        [Action::SkipInitialSync]
    );
    assert_eq!(plan(&spec(), "lima-dv-2", &fresh_on("lima-dv-2")).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &fresh_on("lima-dv-3")).unwrap(), []);
}

#[test]
fn skip_is_never_planned_when_initial_sync_is_done() {
    let obs = fresh_on("lima-dv-1");
    assert!(obs.status.as_ref().unwrap().ready_to_skip_initial_sync());
    // The same input with `Skip` plans the skip.
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::SkipInitialSync]);
    let mut done = spec();
    done.initial_sync = InitialSync::Done;
    // With `Done` nothing is planned, and no synced marker either: the
    // disks are still `Inconsistent`.
    assert_eq!(plan(&done, "lima-dv-1", &obs).unwrap(), []);
}

#[test]
fn a_foreign_node_is_rejected() {
    assert_eq!(
        plan(&spec(), "elsewhere", &steady()),
        Err(PlanError::NotAMember {
            node: "elsewhere".into(),
            volume: "spike".into()
        })
    );
}

fn with_local_disk(node: &str, disk: DiskState) -> Option<ResourceStatus> {
    let mut status = seen_from(node, THREE_UPTODATE);
    status.devices.iter_mut().find(|d| d.volume == 0).unwrap().disk_state = disk;
    Some(status)
}

#[test]
fn the_leader_skips_the_initial_sync_only_while_the_synced_marker_is_absent() {
    let fresh = fresh_on("lima-dv-1");
    assert_eq!(plan(&spec(), "lima-dv-1", &fresh).unwrap(), [Action::SkipInitialSync]);
    let marked = Observed { synced: true, ..fresh };
    assert_eq!(plan(&spec(), "lima-dv-1", &marked).unwrap(), []);
}

#[test]
fn a_copy_that_was_seen_up_to_date_gets_the_synced_marker() {
    for node in ["lima-dv-1", "lima-dv-2"] {
        let unmarked = Observed {
            synced: false,
            ..steady_on(node)
        };
        assert_eq!(plan(&spec(), node, &unmarked).unwrap(), [Action::MarkSynced]);
    }
    assert_eq!(plan(&spec(), "lima-dv-2", &steady_on("lima-dv-2")).unwrap(), []);
}

#[test]
fn a_node_without_a_disk_never_gets_the_synced_marker() {
    let unmarked = Observed {
        synced: false,
        ..steady_on("lima-dv-3")
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &unmarked).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &fresh_on("lima-dv-3")).unwrap(), []);
    // Even a status that claims an up-to-date disk on a client device.
    let claims_disk = Observed {
        synced: false,
        status: with_local_disk("lima-dv-3", DiskState::UpToDate),
        ..steady_on("lima-dv-3")
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &claims_disk).unwrap(), []);
}

/// `node` as seen while its own volume-0 disk is `local` and the other copy
/// (`lima-dv-2` seen from `lima-dv-1`, `lima-dv-1` otherwise) reports `peer`.
fn with_disks(node: &str, local: DiskState, peer: DiskState) -> Option<ResourceStatus> {
    let mut status = with_local_disk(node, local).unwrap();
    let other = if node == "lima-dv-1" { "lima-dv-2" } else { "lima-dv-1" };
    let conn = status.connections.iter_mut().find(|c| c.name == other).unwrap();
    conn.peer_devices
        .iter_mut()
        .find(|p| p.volume == 0)
        .unwrap()
        .peer_disk_state = peer;
    Some(status)
}

#[test]
fn a_copy_that_resyncs_from_an_up_to_date_peer_gets_the_synced_marker() {
    let mut status = status_of(include_str!("../../stand/spike/samples/resync-target.json"));
    // The sample was captured on the second copy; rename its peer to fit `SPEC`.
    status.node_id = 1;
    status.connections[0].name = "lima-dv-1".into();
    status.connections[0].peer_node_id = 0;
    status.connections[1].name = "lima-dv-3".into();
    status.connections[1].peer_node_id = 2;
    status.name = "spike".into();
    status.devices[0].minor = 1000;
    let obs = Observed {
        synced: false,
        status: Some(status),
        ..steady_on("lima-dv-2")
    };
    assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), [Action::MarkSynced]);
}

#[test]
fn a_peer_that_holds_data_in_any_readable_state_marks_the_copy_synced() {
    for node in ["lima-dv-1", "lima-dv-2"] {
        for peer in [DiskState::UpToDate, DiskState::Consistent, DiskState::Outdated] {
            let obs = Observed {
                synced: false,
                status: with_disks(node, DiskState::Inconsistent, peer),
                ..steady_on(node)
            };
            assert_eq!(
                plan(&spec(), node, &obs).unwrap(),
                [Action::MarkSynced],
                "{node} {peer:?}"
            );
        }
    }
}

#[test]
fn without_a_peer_that_holds_data_the_copy_is_not_marked_synced() {
    // The gate fails because a peer is not connected, and no disk shows data.
    let mut status = with_disks("lima-dv-2", DiskState::Inconsistent, DiskState::Inconsistent).unwrap();
    status.connections[0].connection_state = ConnectionState::Connecting;
    assert!(!status.ready_to_skip_initial_sync());
    let obs = Observed {
        synced: false,
        status: Some(status),
        ..steady_on("lima-dv-2")
    };
    assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), []);

    for peer in [
        DiskState::Inconsistent,
        DiskState::DUnknown,
        DiskState::Diskless,
        DiskState::Failed,
    ] {
        let obs = Observed {
            synced: false,
            status: with_disks("lima-dv-2", DiskState::Inconsistent, peer),
            ..steady_on("lima-dv-2")
        };
        assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), [], "{peer:?}");
    }
}

#[test]
fn a_node_without_a_disk_is_not_marked_synced_by_a_peer_that_holds_data() {
    let mut status = seen_from("lima-dv-3", THREE_UPTODATE);
    status.connections[0].peer_devices[0].peer_disk_state = DiskState::UpToDate;
    let obs = Observed {
        synced: false,
        status: Some(status),
        ..steady_on("lima-dv-3")
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &obs).unwrap(), []);
}

#[test]
fn a_copy_with_a_detached_disk_is_adjusted() {
    for node in ["lima-dv-1", "lima-dv-2"] {
        let obs = Observed {
            status: with_local_disk(node, DiskState::Diskless),
            ..steady_on(node)
        };
        assert_eq!(
            plan(&spec(), node, &obs).unwrap(),
            [Action::Adjust, Action::MarkApplied]
        );
    }
    let obs = Observed {
        status: with_local_disk("lima-dv-3", DiskState::Diskless),
        ..steady_on("lima-dv-3")
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &obs).unwrap(), []);
}

// ---- status_matches_spec ----

#[test]
fn the_status_of_every_node_matches_the_description() {
    for node in ["lima-dv-1", "lima-dv-2", "lima-dv-3"] {
        assert!(
            status_matches_spec(&spec(), node, &seen_from(node, THREE_UPTODATE)),
            "{node}"
        );
    }
}

#[test]
fn a_different_node_id_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.node_id = 1;
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_different_minor_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.devices[0].minor = 1001;
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_missing_volume_0_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.devices[0].volume = 1;
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_missing_connection_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.connections.pop();
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn an_extra_connection_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    let mut extra = status.connections[0].clone();
    extra.name = "lima-dv-4".into();
    extra.peer_node_id = 3;
    status.connections.push(extra);
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_connection_with_another_peer_node_id_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.connections[0].peer_node_id = 7;
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_connection_with_another_peer_name_does_not_match() {
    let mut status = seen_from("lima-dv-1", THREE_UPTODATE);
    status.connections[0].name = "lima-dv-9".into();
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_detached_disk_on_a_node_with_a_copy_does_not_match() {
    let status = with_local_disk("lima-dv-1", DiskState::Diskless).unwrap();
    assert!(!status_matches_spec(&spec(), "lima-dv-1", &status));
}

#[test]
fn a_device_that_is_not_a_client_on_a_node_without_a_disk_does_not_match() {
    let mut status = seen_from("lima-dv-3", THREE_UPTODATE);
    status.devices[0].client = false;
    assert!(!status_matches_spec(&spec(), "lima-dv-3", &status));
}

#[test]
fn a_node_outside_the_description_does_not_match() {
    assert!(!status_matches_spec(
        &spec(),
        "elsewhere",
        &seen_from("lima-dv-1", THREE_UPTODATE)
    ));
}

/// The status of the volume as `node` sees it, in the four shapes a node
/// can be in: not configured, configured and matching with every disk
/// `Inconsistent`, configured and matching with every disk `UpToDate`, and
/// configured but with another minor than the description.
fn statuses_on(node: &str) -> [Option<ResourceStatus>; 4] {
    let mut mismatching = seen_from(node, THREE_UPTODATE);
    for device in &mut mismatching.devices {
        device.minor += 1;
    }
    [
        None,
        Some(seen_from(node, THREE_INCONSISTENT)),
        Some(seen_from(node, THREE_UPTODATE)),
        Some(mismatching),
    ]
}

/// Walks every combination of the inputs of `plan` and checks the rules that
/// protect data, whatever the other inputs are.
#[test]
fn the_data_safety_rules_hold_for_every_combination_of_inputs() {
    let mut inputs = 0;
    let mut create_md_seen = 0;
    let mut skip_seen = 0;
    let mut lost_seen = 0;
    for initial_sync in [InitialSync::Skip, InitialSync::Done] {
        let mut spec = spec();
        spec.initial_sync = initial_sync;
        for node in spec.nodes.iter().map(|n| n.name.as_str()) {
            let diskful = spec.node(node).unwrap().diskful;
            let leader = spec.initial_sync_leader().is_some_and(|l| l.name == node);
            for file in [FileState::Absent, FileState::Pending, FileState::Ready] {
                for status in statuses_on(node) {
                    for (synced, stale_markers, res_matches, res_applied) in
                        (0..16).map(|bits| (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0))
                    {
                        inputs += 1;
                        let obs = Observed {
                            file,
                            loop_device: Some("/dev/loop3".into()),
                            link_target: Some("/dev/loop3".into()),
                            res_matches,
                            res_applied,
                            synced,
                            stale_markers,
                            status: status.clone(),
                        };
                        let result = plan(&spec, node, &obs);
                        let what = format!("{node} {obs:?}");

                        if diskful && file == FileState::Pending && synced {
                            lost_seen += 1;
                            assert_eq!(
                                result,
                                Err(PlanError::ReadyMarkerLost { volume: "spike".into() }),
                                "{what}"
                            );
                        }
                        let Ok(actions) = result else { continue };

                        if actions.contains(&Action::CreateMd) {
                            create_md_seen += 1;
                            assert!(diskful, "{what}");
                            assert_ne!(file, FileState::Ready, "{what}");
                            assert!(obs.status.is_none(), "{what}");
                        }
                        if actions.contains(&Action::SkipInitialSync) {
                            skip_seen += 1;
                            assert_eq!(initial_sync, InitialSync::Skip, "{what}");
                            assert!(leader && diskful, "{what}");
                            assert!(!synced, "{what}");
                            assert!(obs.status.as_ref().unwrap().ready_to_skip_initial_sync(), "{what}");
                            assert_eq!(actions, [Action::SkipInitialSync], "{what}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!(inputs, 2 * 3 * 3 * 4 * 16);
    // The loop above must reach the branches the rules talk about.
    assert!(create_md_seen > 0 && skip_seen > 0 && lost_seen > 0);
}

// ---- executor ----

fn temp_paths(tmp: &TempDir) -> Paths {
    Paths {
        volumes_dir: tmp.path().join("volumes"),
        res_dir: tmp.path().join("res"),
        dev_dir: tmp.path().join("dev"),
    }
}

#[test]
fn apply_rejects_an_invalid_spec_before_touching_anything() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let mut spec = spec();
    spec.nodes[0].name = "lima;dv-1".into();

    let result = reconciler.apply(&spec, "lima;dv-1");
    assert!(matches!(result, Err(ReconcileError::Spec(_))), "{result:?}");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    assert!(!paths.volumes_dir.exists());
    assert!(!paths.res_dir.exists());
    assert!(!paths.dev_dir.exists());
}

/// A fake node: `losetup` remembers the attachment, `drbdadm up` makes the
/// resource appear in `drbdsetup status`, and the skip of the initial sync
/// turns the disks `UpToDate`.
fn fake_node() -> FakeRunner {
    fake_node_with(out(255, "", P7_NO_METADATA_STDERR))
}

/// `fake_node` whose `drbdadm dump-md` answers with `dump_md`.
fn fake_node_with(dump_md: Output) -> FakeRunner {
    fake_node_with_dump_md(move |_| dump_md.clone())
}

/// `fake_node` whose `drbdadm dump-md` answers by whether `create-md` has
/// run on the node before (the argument).
fn fake_node_with_dump_md(dump_md: impl Fn(bool) -> Output + Send + Sync + 'static) -> FakeRunner {
    let attached = Arc::new(AtomicBool::new(false));
    let up = Arc::new(AtomicBool::new(false));
    let skipped = Arc::new(AtomicBool::new(false));
    let created = Arc::new(AtomicBool::new(false));
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) if attached.load(Ordering::SeqCst) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("losetup", ["--find", ..]) => {
            attached.store(true, Ordering::SeqCst);
            out(0, "/dev/loop7\n", "")
        }
        ("losetup", ["-d", _]) => {
            attached.store(false, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["dump-md", _]) => dump_md(created.load(Ordering::SeqCst)),
        ("drbdadm", ["create-md", ..]) => {
            created.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["up", _]) => {
            up.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["down", _]) => {
            up.store(false, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdadm", ["new-current-uuid", ..]) => {
            skipped.store(true, Ordering::SeqCst);
            out(0, "", "")
        }
        ("drbdsetup", _) if up.load(Ordering::SeqCst) && skipped.load(Ordering::SeqCst) => out(0, THREE_UPTODATE, ""),
        ("drbdsetup", _) if up.load(Ordering::SeqCst) => out(0, THREE_INCONSISTENT, ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    })
}

#[test]
fn apply_creates_a_copy_then_skips_the_initial_sync_and_remove_undoes_it() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let spec = spec();

    let done = reconciler.apply(&spec, "lima-dv-1").unwrap();
    let bytes = backing_file_bytes(spec.size_bytes, spec.max_peers);
    assert_eq!(
        done,
        [
            Action::CreateFile { bytes },
            Action::AttachLoop,
            Action::LinkDevice,
            Action::WriteRes,
            Action::CreateMd,
            Action::MarkReady,
            Action::Up,
            Action::MarkApplied,
            Action::SkipInitialSync,
        ]
    );

    let image = paths.image("spike");
    assert_eq!(fs::metadata(&image).unwrap().len(), bytes);
    assert!(paths.ready_marker("spike").exists());
    // The skip is confirmed by a later apply, not by its return code.
    assert!(!paths.synced_marker("spike").exists());
    assert_eq!(
        backing::link_target(&paths.device_link("spike")).as_deref(),
        Some("/dev/loop7")
    );
    let res = fs::read_to_string(paths.res_file("spike")).unwrap();
    assert!(
        res.contains(&format!("disk {};", paths.device_link("spike").display())),
        "{res}"
    );
    assert_eq!(fs::read_to_string(paths.applied_res("spike")).unwrap(), res);

    let image_arg = image.to_string_lossy().into_owned();
    let commands: Vec<String> = runner
        .calls()
        .into_iter()
        .filter(|c| !c.starts_with("drbdsetup") && !c.starts_with("losetup -j"))
        .map(|c| c.replace(&image_arg, "<image>"))
        .collect();
    assert_eq!(
        commands,
        [
            "losetup --find --show --direct-io=on <image>",
            "drbdadm dump-md spike",
            "drbdadm create-md --force --max-peers=7 spike",
            "drbdadm up spike",
            "drbdadm new-current-uuid --clear-bitmap spike/0",
        ]
    );

    // The next apply sees the disks UpToDate and records it.
    assert_eq!(reconciler.apply(&spec, "lima-dv-1").unwrap(), [Action::MarkSynced]);
    assert!(paths.synced_marker("spike").exists());
    assert_eq!(reconciler.apply(&spec, "lima-dv-1").unwrap(), []);

    let steps = reconciler.remove("spike").unwrap();
    assert_eq!(
        steps,
        [
            "down",
            "detach-loop",
            "remove-file",
            "remove-marker",
            "remove-synced-marker",
            "remove-link",
            "remove-res",
            "remove-applied",
        ]
    );
    assert!(!image.exists());
    assert!(!paths.synced_marker("spike").exists());
    assert!(!paths.applied_res("spike").exists());
    assert!(!paths.lock_file("spike").exists());
    assert_eq!(reconciler.remove("spike").unwrap(), Vec::<&str>::new());
    assert!(!paths.lock_file("spike").exists());
}

#[test]
fn remove_detaches_every_loop_device_of_the_file() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.image("spike"), "").unwrap();
    let runner = FakeRunner::new(|program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop1: [1]:2 (image)\n/dev/loop0: [1]:2 (image)\n", ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let steps = reconciler.remove("spike").unwrap();
    assert_eq!(steps, ["detach-loop", "detach-loop", "remove-file"]);
    let detaches: Vec<String> = runner
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("losetup -d"))
        .collect();
    assert_eq!(detaches, ["losetup -d /dev/loop1", "losetup -d /dev/loop0"]);
}

#[test]
fn apply_refuses_to_act_on_a_file_attached_twice() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.image("spike"), "").unwrap();
    let runner = FakeRunner::new(|program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop1: [1]:2 (image)\n/dev/loop0: [1]:2 (image)\n", ""),
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(err, ReconcileError::Backing(BackingError::MultipleLoopDevices { .. })),
        "{err}"
    );
}

/// Files of a copy of `SPEC` that lima-dv-1 set up earlier; the device
/// link points at /dev/loop7.
fn prepare_copy(paths: &Paths, applied: bool, synced: bool) {
    for dir in [&paths.volumes_dir, &paths.res_dir, &paths.dev_dir] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(paths.image("spike"), "").unwrap();
    fs::write(paths.ready_marker("spike"), "").unwrap();
    if synced {
        fs::write(paths.synced_marker("spike"), "").unwrap();
    }
    let text = resource::render(&spec(), &paths.device_link("spike"));
    fs::write(paths.res_file("spike"), &text).unwrap();
    if applied {
        fs::write(paths.applied_res("spike"), &text).unwrap();
    }
    std::os::unix::fs::symlink("/dev/loop7", paths.device_link("spike")).unwrap();
}

/// A node where the copy is attached to /dev/loop7 and `drbdsetup status`
/// prints whatever `status` holds; every other command succeeds and
/// changes nothing.
fn configured_node(status: Arc<Mutex<String>>) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
        _ => out(0, "", ""),
    })
}

fn count(runner: &FakeRunner, prefix: &str) -> usize {
    runner.calls().iter().filter(|c| c.starts_with(prefix)).count()
}

#[test]
fn an_adjust_that_takes_effect_is_recorded() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, false, true);
    let runner = configured_node(Arc::new(Mutex::new(THREE_UPTODATE.to_string())));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    assert_eq!(
        reconciler.apply(&spec(), "lima-dv-1").unwrap(),
        [Action::Adjust, Action::MarkApplied]
    );
    assert_eq!(
        fs::read_to_string(paths.applied_res("spike")).unwrap(),
        fs::read_to_string(paths.res_file("spike")).unwrap()
    );
}

#[test]
fn an_adjust_without_effect_is_an_error_and_is_repeated_next_time() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, false, true);
    let other_minor = THREE_UPTODATE.replace("\"minor\": 1000", "\"minor\": 1001");
    assert_ne!(other_minor, THREE_UPTODATE);
    let runner = configured_node(Arc::new(Mutex::new(other_minor)));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::NotApplied { command: "adjust", volume } if volume == "spike"),
        "{err}"
    );
    assert!(!paths.applied_res("spike").exists());

    let obs = reconciler.observe(&spec(), "lima-dv-1").unwrap();
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::Adjust, Action::MarkApplied]
    );
    assert!(reconciler.apply(&spec(), "lima-dv-1").is_err());
    assert_eq!(count(&runner, "drbdadm adjust spike"), 2);
}

/// A node where `drbdadm adjust` fails with code 1 and `drbdsetup status`
/// shows a resource that already matches the description.
fn node_that_rejects_adjust() -> FakeRunner {
    FakeRunner::new(|program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["adjust", _]) => out(1, "", "Failure: (162) Invalid configuration request"),
        ("drbdsetup", _) => out(0, THREE_UPTODATE, ""),
        _ => out(0, "", ""),
    })
}

#[test]
fn a_rejected_adjust_is_not_recorded_even_when_the_status_matches() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, false, true);
    let runner = node_that_rejects_adjust();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::NotApplied { command: "adjust", volume } if volume == "spike"),
        "{err}"
    );
    assert!(!paths.applied_res("spike").exists());

    let obs = reconciler.observe(&spec(), "lima-dv-1").unwrap();
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::Adjust, Action::MarkApplied]
    );
    assert!(reconciler.apply(&spec(), "lima-dv-1").is_err());
    assert_eq!(count(&runner, "drbdadm adjust spike"), 2);
}

#[test]
fn an_up_without_effect_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = configured_node(Arc::new(Mutex::new("[\n]\n".to_string())));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-3").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::NotApplied { command: "up", volume } if volume == "spike"),
        "{err}"
    );
    assert_eq!(count(&runner, "drbdadm up spike"), 1);
    assert!(paths.res_file("spike").exists());
    assert!(!paths.applied_res("spike").exists());
}

#[test]
fn the_synced_marker_is_written_only_after_the_status_shows_the_skip() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, false);
    let status = Arc::new(Mutex::new(THREE_INCONSISTENT.to_string()));
    let runner = configured_node(status.clone());
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let skip = "drbdadm new-current-uuid --clear-bitmap spike/0";

    // The command returned 0 but the status still shows Inconsistent.
    assert_eq!(
        reconciler.apply(&spec(), "lima-dv-1").unwrap(),
        [Action::SkipInitialSync]
    );
    assert_eq!(count(&runner, skip), 1);
    assert!(!paths.synced_marker("spike").exists());

    *status.lock().unwrap() = THREE_UPTODATE.to_string();
    assert_eq!(reconciler.apply(&spec(), "lima-dv-1").unwrap(), [Action::MarkSynced]);
    assert!(paths.synced_marker("spike").exists());

    // With the marker present a gate-ready status never triggers a skip.
    *status.lock().unwrap() = THREE_INCONSISTENT.to_string();
    assert_eq!(reconciler.apply(&spec(), "lima-dv-1").unwrap(), []);
    assert_eq!(count(&runner, skip), 1);
}

#[test]
fn a_leader_that_resyncs_from_a_peer_never_skips_the_initial_sync_later() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, false);
    // The leader was re-created and resyncs from the second copy.
    let resyncing = THREE_INCONSISTENT.replacen(
        "\"peer-disk-state\": \"Inconsistent\"",
        "\"peer-disk-state\": \"UpToDate\"",
        1,
    );
    assert_ne!(resyncing, THREE_INCONSISTENT);
    let status = Arc::new(Mutex::new(resyncing));
    let runner = configured_node(status.clone());
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let skip = "drbdadm new-current-uuid --clear-bitmap spike/0";

    assert_eq!(reconciler.apply(&spec(), "lima-dv-1").unwrap(), [Action::MarkSynced]);
    assert!(paths.synced_marker("spike").exists());

    // The second copy is lost too and re-created: both disks are Inconsistent
    // and the gate holds, but the marker says the volume has held data.
    *status.lock().unwrap() = THREE_INCONSISTENT.to_string();
    assert_eq!(reconciler.apply(&spec(), "lima-dv-1").unwrap(), []);
    assert_eq!(count(&runner, skip), 0);
}

#[test]
fn stale_markers_are_cleared_before_a_new_copy_is_created() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.ready_marker("spike"), "").unwrap();
    fs::write(paths.synced_marker("spike"), "").unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let done = reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert_eq!(done[0], Action::ClearStaleMarkers);
    assert!(done.contains(&Action::CreateMd), "{done:?}");
    assert_eq!(count(&runner, "drbdadm create-md"), 1);
    assert!(paths.ready_marker("spike").exists());
    assert!(!paths.synced_marker("spike").exists());
}

#[test]
fn linking_without_a_known_loop_device_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler
        .execute(&spec(), "lima-dv-1", &Action::LinkDevice, &mut None)
        .unwrap_err();
    assert!(
        matches!(&err, ReconcileError::NoLoopDevice { volume } if volume == "spike"),
        "{err}"
    );
    assert!(!paths.device_link("spike").exists());
}

#[test]
fn markers_get_distinct_temporary_names() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let ready = temp_path(&paths.ready_marker("spike"));
    let synced = temp_path(&paths.synced_marker("spike"));
    assert_ne!(ready, synced);
    assert_eq!(ready, paths.volumes_dir.join(".spike.img.ready.tmp"));
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::create_dir_all(&paths.res_dir).unwrap();

    write_atomically(&paths.synced_marker("spike"), "").unwrap();
    write_atomically(&paths.applied_res("spike"), "text").unwrap();
    assert!(paths.synced_marker("spike").exists());
    assert_eq!(fs::read_to_string(paths.applied_res("spike")).unwrap(), "text");
    assert!(!synced.exists());
}

// ---- remove ----

#[test]
fn remove_deletes_nothing_when_down_fails() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, true);
    let runner = FakeRunner::new(|program, args| match (program, args) {
        ("drbdadm", ["down", _]) => out(1, "", "resource busy"),
        ("drbdsetup", _) => out(0, THREE_UPTODATE, ""),
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.remove("spike").unwrap_err();
    assert!(matches!(err, ReconcileError::Drbd(_)), "{err}");
    assert_copy_intact(&paths);
    assert_eq!(count(&runner, "losetup -d"), 0);
}

#[test]
fn remove_deletes_nothing_when_the_resource_stays_configured_after_down() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, true);
    let runner = configured_node(Arc::new(Mutex::new(THREE_UPTODATE.to_string())));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.remove("spike").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::NotApplied { command: "down", volume } if volume == "spike"),
        "{err}"
    );
    assert_copy_intact(&paths);
    assert_eq!(count(&runner, "losetup -d"), 0);
}

fn assert_copy_intact(paths: &Paths) {
    for path in [
        paths.image("spike"),
        paths.ready_marker("spike"),
        paths.synced_marker("spike"),
        paths.res_file("spike"),
        paths.applied_res("spike"),
    ] {
        assert!(path.exists(), "{}", path.display());
    }
    assert!(backing::link_target(&paths.device_link("spike")).is_some());
}

#[test]
fn remove_rejects_an_invalid_name_before_touching_anything() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    for name in ["../x", "", "a/b", "all"] {
        let err = reconciler.remove(name).unwrap_err();
        assert!(
            matches!(&err, ReconcileError::Spec(SpecError::Name(n)) if n == name),
            "{err}"
        );
    }
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    assert!(!paths.volumes_dir.exists());
}

/// A node where the image is gone and `losetup` reports `back_file` behind
/// /dev/loop5.
fn node_with_back_file(back_file: String) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["--list", "--noheadings", "--output", "BACK-FILE", "/dev/loop5"]) => {
            out(0, &format!("{back_file}\n"), "")
        }
        ("drbdsetup", _) => out(0, "[\n]\n", ""),
        _ => out(0, "", ""),
    })
}

fn prepare_link_without_image(paths: &Paths) {
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::create_dir_all(&paths.dev_dir).unwrap();
    fs::write(paths.ready_marker("spike"), "").unwrap();
    std::os::unix::fs::symlink("/dev/loop5", paths.device_link("spike")).unwrap();
}

#[test]
fn remove_detaches_the_loop_device_of_a_deleted_image() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_link_without_image(&paths);
    let deleted = format!("{} (deleted)", paths.image("spike").display());
    let runner = node_with_back_file(deleted);
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    assert_eq!(
        reconciler.remove("spike").unwrap(),
        ["detach-loop", "remove-marker", "remove-link"]
    );
    assert_eq!(count(&runner, "losetup -d /dev/loop5"), 1);
}

#[test]
fn remove_leaves_a_loop_device_of_another_file_alone() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_link_without_image(&paths);
    let other = paths.image("other").display().to_string();
    let runner = node_with_back_file(other);
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    assert_eq!(reconciler.remove("spike").unwrap(), ["remove-marker", "remove-link"]);
    assert_eq!(count(&runner, "losetup -d"), 0);
}

// ---- durability of the image ----

#[test]
fn sync_file_flushes_an_existing_file_and_reports_a_missing_one() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("x.img");
    fs::write(&path, "data").unwrap();
    sync_file(&path).unwrap();

    let err = sync_file(&tmp.path().join("missing.img")).unwrap_err();
    assert!(matches!(err, ReconcileError::Io { action: "sync", .. }), "{err}");
}

// ---- unreadable markers ----

/// Makes `path` a symbolic link to itself: `stat` fails with ELOOP, also for
/// a process that ignores file permissions.
fn make_unreadable(path: &Path) {
    std::os::unix::fs::symlink(path, path).unwrap();
}

#[test]
fn a_marker_or_image_that_cannot_be_inspected_stops_observe_and_apply() {
    type PathOf = fn(&Paths) -> PathBuf;
    let targets: [(&str, PathOf); 3] = [
        ("image", |p| p.image("spike")),
        ("ready marker", |p| p.ready_marker("spike")),
        ("synced marker", |p| p.synced_marker("spike")),
    ];
    for (what, path_of) in targets {
        let tmp = TempDir::new().unwrap();
        let paths = temp_paths(&tmp);
        prepare_copy(&paths, true, true);
        let path = path_of(&paths);
        fs::remove_file(&path).ok();
        make_unreadable(&path);
        let runner = configured_node(Arc::new(Mutex::new(THREE_UPTODATE.to_string())));
        let reconciler = Reconciler {
            paths: &paths,
            runner: &runner,
        };

        let err = reconciler.observe(&spec(), "lima-dv-1").unwrap_err();
        assert!(
            matches!(err, ReconcileError::Io { action: "inspect", .. }),
            "{what}: {err}"
        );
        let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
        assert!(
            matches!(err, ReconcileError::Io { action: "inspect", .. }),
            "{what}: {err}"
        );
        assert_eq!(count(&runner, "drbdadm"), 0, "{what}: {:?}", runner.calls());
    }
}

#[test]
fn remove_stops_when_the_image_cannot_be_inspected() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, true);
    fs::remove_file(paths.image("spike")).unwrap();
    make_unreadable(&paths.image("spike"));
    let runner = FakeRunner::new(|_, _| out(0, "[\n]\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.remove("spike").unwrap_err();
    assert!(matches!(err, ReconcileError::Io { action: "inspect", .. }), "{err}");
    assert!(paths.ready_marker("spike").exists());
    assert!(paths.synced_marker("spike").exists());
}

// ---- lock ----

#[test]
fn a_held_volume_lock_makes_apply_and_remove_busy() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let held = reconciler.lock_volume("spike").unwrap();

    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::Busy { volume } if volume == "spike"),
        "{err}"
    );
    let err = reconciler.remove("spike").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::Busy { volume } if volume == "spike"),
        "{err}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());

    // Another volume is not blocked.
    assert_eq!(reconciler.remove("other").unwrap(), Vec::<&str>::new());

    drop(held);
    assert!(reconciler.apply(&spec(), "lima-dv-1").is_ok());
}

#[test]
fn the_lock_file_counts_as_linked_while_its_path_names_the_open_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike.lock");
    let file = File::create(&path).unwrap();
    assert!(still_linked(&file, &path).unwrap());
}

#[test]
fn a_removed_lock_file_is_not_linked() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike.lock");
    let file = File::create(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(!still_linked(&file, &path).unwrap());
}

#[test]
fn a_replaced_lock_file_is_not_linked() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("spike.lock");
    let file = File::create(&path).unwrap();
    fs::remove_file(&path).unwrap();
    // The new file is created while the old one is still open, so the two
    // never share an inode.
    File::create(&path).unwrap();
    assert!(!still_linked(&file, &path).unwrap());
}

// ---- metadata check before create-md ----

fn node_with_dump_md(dump_md: Output) -> FakeRunner {
    fake_node_with(dump_md)
}

#[test]
fn create_md_runs_only_after_dump_md_reports_no_metadata() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = node_with_dump_md(out(255, "", P7_NO_METADATA_STDERR));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    let md: Vec<String> = runner.calls().into_iter().filter(|c| c.contains("-md")).collect();
    assert_eq!(
        md,
        ["drbdadm dump-md spike", "drbdadm create-md --force --max-peers=7 spike"]
    );
    assert!(paths.ready_marker("spike").exists());
}

#[test]
fn existing_metadata_stops_create_md_and_leaves_no_ready_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = node_with_dump_md(out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(&err, ReconcileError::MetadataPresent { volume } if volume == "spike"),
        "{err}"
    );
    assert!(
        !runner.calls().iter().any(|c| c.contains("create-md")),
        "{:?}",
        runner.calls()
    );
    assert!(!paths.ready_marker("spike").exists());
    assert!(paths.image("spike").exists());
    assert_eq!(
        err.to_string(),
        "volume \"spike\": the backing file already holds DRBD metadata but has no ready marker; the metadata was not \
         touched. Check the file by hand before restoring the marker"
    );
}

#[test]
fn a_failed_metadata_check_stops_create_md() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = node_with_dump_md(out(20, "", "Device is configured"));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(err, ReconcileError::Drbd(DrbdError::MetadataCheck { .. })),
        "{err}"
    );
    assert!(
        !runner.calls().iter().any(|c| c.contains("create-md")),
        "{:?}",
        runner.calls()
    );
    assert!(!paths.ready_marker("spike").exists());
}

// ---- the volumes directory ----

#[test]
fn apply_does_not_create_the_volumes_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(matches!(&err, ReconcileError::Io { action: "open", .. }), "{err}");
    assert!(!paths.volumes_dir.exists());
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

// ---- the creating marker ----

/// The `<dev>:<ino>` text the creating marker holds for the file at `path`.
fn identity_of(path: &Path) -> String {
    let meta = fs::metadata(path).unwrap();
    format!("{}:{}", meta.dev(), meta.ino())
}

fn md_calls(runner: &FakeRunner) -> Vec<String> {
    runner.calls().into_iter().filter(|c| c.contains("-md")).collect()
}

/// Metadata exists once `create-md` ran; before that the file is empty.
fn md_after_create() -> FakeRunner {
    fake_node_with_dump_md(|created| {
        if created {
            out(0, "# DRBD meta data dump\n", "")
        } else {
            out(255, "", P7_NO_METADATA_STDERR)
        }
    })
}

#[test]
fn existing_metadata_writes_no_creating_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = node_with_dump_md(out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(matches!(err, ReconcileError::MetadataPresent { .. }), "{err}");
    assert!(!paths.creating_marker("spike").exists());
    assert_eq!(md_calls(&runner), ["drbdadm dump-md spike"]);
}

#[test]
fn the_creating_marker_is_written_between_the_check_and_create_md() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let marker = paths.creating_marker("spike");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_runner = Arc::clone(&seen);
    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("drbdadm", ["dump-md", _]) => {
            seen_by_runner.lock().unwrap().push(("dump-md", marker.exists()));
            out(255, "", P7_NO_METADATA_STDERR)
        }
        ("drbdadm", ["create-md", ..]) => {
            seen_by_runner.lock().unwrap().push(("create-md", marker.exists()));
            out(0, "", "")
        }
        ("losetup", ["--find", ..]) => out(0, "/dev/loop7\n", ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    // `up` has no effect on this fake, so the pass ends with NotApplied after
    // the markers were handled.
    let _ = reconciler.apply(&spec(), "lima-dv-1");
    assert_eq!(*seen.lock().unwrap(), [("dump-md", false), ("create-md", true)]);
}

#[test]
fn a_creating_marker_skips_the_metadata_check_and_runs_create_md() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    // This agent started `create-md` earlier on this very file: metadata may
    // exist by now.
    fs::write(paths.image("spike"), "").unwrap();
    fs::write(paths.creating_marker("spike"), identity_of(&paths.image("spike"))).unwrap();
    let runner = node_with_dump_md(out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert_eq!(md_calls(&runner), ["drbdadm create-md --force --max-peers=7 spike"]);
    assert!(paths.ready_marker("spike").exists());
}

#[test]
fn mark_ready_leaves_the_ready_marker_and_no_creating_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.creating_marker("spike"), "").unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    // The action alone, not a whole `apply`: the next pass of `apply` also
    // removes a creating marker that sits next to a ready marker.
    reconciler
        .execute(&spec(), "lima-dv-1", &Action::MarkReady, &mut None)
        .unwrap();
    assert!(paths.ready_marker("spike").exists());
    assert!(!paths.creating_marker("spike").exists());
}

#[test]
fn an_apply_interrupted_after_create_md_is_finished_by_the_next_one() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = md_after_create();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    // Writing the ready marker fails: a directory sits in the way of the
    // temporary file, as if the process had died right after `create-md`.
    let blocker = paths.volumes_dir.join(".spike.img.ready.tmp");
    fs::create_dir(&blocker).unwrap();
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(matches!(err, ReconcileError::Io { action: "create", .. }), "{err}");
    assert!(!paths.ready_marker("spike").exists());
    assert!(paths.creating_marker("spike").exists());
    assert_eq!(count(&runner, "drbdadm create-md"), 1);

    fs::remove_dir(&blocker).unwrap();
    let done = reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert!(
        done.contains(&Action::MarkReady) && done.contains(&Action::Up),
        "{done:?}"
    );
    // The metadata of the first attempt was not mistaken for foreign data.
    assert_eq!(count(&runner, "drbdadm dump-md"), 1);
    assert_eq!(count(&runner, "drbdadm create-md"), 2);
    assert!(paths.ready_marker("spike").exists());
    assert!(!paths.creating_marker("spike").exists());
}

#[test]
fn a_creating_marker_never_makes_a_ready_file_get_new_metadata() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    // Left behind by a crash between writing the ready marker and removing
    // the creating marker.
    fs::write(paths.creating_marker("spike"), "").unwrap();
    let before = count(&runner, "drbdadm create-md");
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert_eq!(count(&runner, "drbdadm create-md"), before);
    // The leftover is removed, so a later loss of the ready marker cannot
    // skip the metadata check.
    assert!(!paths.creating_marker("spike").exists());
}

#[test]
fn remove_deletes_the_creating_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.creating_marker("spike"), "").unwrap();
    fs::write(paths.image("spike"), "").unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let steps = reconciler.remove("spike").unwrap();
    assert!(steps.contains(&"remove-creating-marker"), "{steps:?}");
    assert!(!paths.creating_marker("spike").exists());
    assert!(!paths.image("spike").exists());
}

#[test]
fn a_creating_marker_without_an_image_is_cleared_with_the_other_stale_markers() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.creating_marker("spike"), "").unwrap();
    let runner = node_with_dump_md(out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let obs = reconciler.observe(&spec(), "lima-dv-1").unwrap();
    assert!(obs.stale_markers);
    // The marker of the old image must not let the new file skip the check:
    // the file is new, the check runs, finds metadata and stops the pass.
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(matches!(err, ReconcileError::MetadataPresent { .. }), "{err}");
    assert_eq!(md_calls(&runner), ["drbdadm dump-md spike"]);
    assert!(!paths.creating_marker("spike").exists());
}

#[test]
fn the_creating_marker_name_sits_next_to_the_image() {
    let paths = Paths::default();
    assert_eq!(
        paths.creating_marker("spike"),
        paths.volumes_dir.join("spike.img.creating")
    );
}

// ---- messages and the volumes directory ----

#[test]
fn the_metadata_present_message_does_not_claim_that_nothing_changed() {
    let text = ReconcileError::MetadataPresent { volume: "spike".into() }.to_string();
    assert!(!text.contains("nothing was changed"), "{text}");
    assert!(text.contains("the metadata was not touched"), "{text}");
}

#[test]
fn a_missing_volumes_directory_gives_an_actionable_error() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    for err in [
        reconciler.apply(&spec(), "lima-dv-1").unwrap_err(),
        reconciler.remove("spike").unwrap_err(),
    ] {
        assert!(matches!(&err, ReconcileError::Io { action: "open", .. }), "{err}");
        let text = err.to_string();
        assert!(text.contains(&paths.volumes_dir.display().to_string()), "{text}");
        assert!(text.contains("must exist on the node"), "{text}");
        assert!(text.contains("0700"), "{text}");
        assert!(text.contains("never creates"), "{text}");
    }
    assert!(!paths.volumes_dir.exists());
    assert!(!paths.res_dir.exists());
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn remove_does_not_create_the_volumes_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    reconciler.remove("spike").unwrap_err();
    assert!(!paths.volumes_dir.exists());
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn an_atomic_write_does_not_create_a_missing_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let err = write_atomically(&paths.ready_marker("spike"), "").unwrap_err();
    assert!(matches!(err, ReconcileError::Io { action: "create", .. }), "{err}");
    assert!(!paths.volumes_dir.exists());
}

// ---- the creating marker and a configured resource (G1) ----

/// A node where the copy is attached to /dev/loop7, `drbdsetup status`
/// prints `status`, and `drbdadm dump-md` answers with `dump_md`.
fn node_with_status_and_dump_md(status: Arc<Mutex<String>>, dump_md: Output) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("drbdadm", ["dump-md", _]) => dump_md.clone(),
        ("drbdsetup", _) => out(0, &status.lock().unwrap(), ""),
        _ => out(0, "", ""),
    })
}

#[test]
fn a_creating_marker_does_not_survive_a_configured_resource() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, false);
    fs::remove_file(paths.ready_marker("spike")).unwrap();
    // A pass wrote the marker and ran `create-md`, then died before the
    // ready marker; the operator brought the resource up by hand.
    fs::write(paths.creating_marker("spike"), identity_of(&paths.image("spike"))).unwrap();
    let status = Arc::new(Mutex::new(THREE_INCONSISTENT.to_string()));
    let runner = node_with_status_and_dump_md(Arc::clone(&status), out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(
        matches!(
            err,
            ReconcileError::Plan(PlanError::UninitialisedFileWithLiveResource { .. })
        ),
        "{err}"
    );
    assert!(!paths.creating_marker("spike").exists());

    // The operator follows the message and takes the resource down: the
    // check runs again and stops on the metadata.
    *status.lock().unwrap() = "[\n]\n\n".to_string();
    let err = reconciler.apply(&spec(), "lima-dv-1").unwrap_err();
    assert!(matches!(err, ReconcileError::MetadataPresent { .. }), "{err}");
    assert_eq!(md_calls(&runner), ["drbdadm dump-md spike"]);
}

// ---- the creating marker is bound to the backing file (G2) ----

/// Runs one `apply` on a pending file with the given marker text and
/// `dump-md` answering "metadata present"; returns the result and the calls.
fn apply_with_marker(text: &str, replace_image: bool) -> (Result<Vec<Action>, ReconcileError>, Vec<String>, bool) {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.image("spike"), "").unwrap();
    let text = if text == "<identity>" {
        identity_of(&paths.image("spike"))
    } else {
        text.to_string()
    };
    if replace_image {
        // The new file exists while the old one does, so the inode differs.
        let other = paths.volumes_dir.join("other");
        fs::write(&other, "").unwrap();
        fs::rename(&other, paths.image("spike")).unwrap();
    }
    fs::write(paths.creating_marker("spike"), text).unwrap();
    let runner = node_with_dump_md(out(0, "# DRBD meta data dump\n", ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let result = reconciler.apply(&spec(), "lima-dv-1");
    (result, md_calls(&runner), paths.creating_marker("spike").exists())
}

#[test]
fn a_marker_of_the_same_file_skips_the_check() {
    let (result, calls, _) = apply_with_marker("<identity>", false);
    result.unwrap();
    assert_eq!(calls, ["drbdadm create-md --force --max-peers=7 spike"]);
}

#[test]
fn a_marker_of_another_inode_runs_the_check_and_is_removed() {
    let (result, calls, exists) = apply_with_marker("<identity>", true);
    assert!(
        matches!(result, Err(ReconcileError::MetadataPresent { .. })),
        "{result:?}"
    );
    assert_eq!(calls, ["drbdadm dump-md spike"]);
    assert!(!exists);
}

#[test]
fn an_empty_marker_runs_the_check() {
    let (result, calls, exists) = apply_with_marker("", false);
    assert!(
        matches!(result, Err(ReconcileError::MetadataPresent { .. })),
        "{result:?}"
    );
    assert_eq!(calls, ["drbdadm dump-md spike"]);
    assert!(!exists);
}

#[test]
fn a_garbage_marker_runs_the_check() {
    for text in ["garbage", "1:2:3", "a:b", "12", ":"] {
        let (result, calls, exists) = apply_with_marker(text, false);
        assert!(
            matches!(result, Err(ReconcileError::MetadataPresent { .. })),
            "{text}: {result:?}"
        );
        assert_eq!(calls, ["drbdadm dump-md spike"], "{text}");
        assert!(!exists, "{text}");
    }
}

#[test]
fn the_marker_a_pass_writes_holds_the_identity_of_the_file() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let marker = paths.creating_marker("spike");
    let image = paths.image("spike");
    let seen = Arc::new(Mutex::new(String::new()));
    let seen_by_runner = Arc::clone(&seen);
    let runner = FakeRunner::new(move |program, args| match (program, args) {
        ("drbdadm", ["dump-md", _]) => out(255, "", P7_NO_METADATA_STDERR),
        ("drbdadm", ["create-md", ..]) => {
            *seen_by_runner.lock().unwrap() = fs::read_to_string(&marker).unwrap_or_default();
            out(0, "", "")
        }
        ("losetup", ["--find", ..]) => out(0, "/dev/loop7\n", ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let _ = reconciler.apply(&spec(), "lima-dv-1");
    assert_eq!(*seen.lock().unwrap(), identity_of(&image));
}
