use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;
use crate::backing::backing_file_bytes;
use crate::drbd::status::{self, ConnectionState, DiskState};
use crate::exec::Output;
use crate::tests::{
    FakeRunner, P7_NO_METADATA_STDERR, THREE_INCONSISTENT, THREE_UPTODATE, configured_node, fake_node, fake_node_with,
    fake_node_with_dump_md, out, prepare_copy, spec, temp_paths,
};

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

/// A node where the image is gone and `losetup --list` prints `listing`.
fn node_with_listing(listing: String) -> FakeRunner {
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["--list", "--noheadings", "--output", "NAME,BACK-FILE"]) => out(0, &listing, ""),
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
    let runner = node_with_listing(format!("/dev/loop5 {} (deleted)\n", paths.image("spike").display()));
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
    let runner = node_with_listing(format!("/dev/loop5 {}\n", paths.image("other").display()));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    assert_eq!(reconciler.remove("spike").unwrap(), ["remove-marker", "remove-link"]);
    assert_eq!(count(&runner, "losetup -d"), 0);
}

#[test]
fn remove_detaches_every_loop_device_of_a_deleted_image_whatever_the_link_says() {
    // Two devices hold the deleted file and the device link points at neither.
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_link_without_image(&paths);
    let image = paths.image("spike");
    let runner = node_with_listing(format!(
        "/dev/loop1 {0} (deleted)\n/dev/loop2 /elsewhere/x.img\n/dev/loop3 {0} (deleted)\n",
        image.display()
    ));
    let steps = Reconciler {
        paths: &paths,
        runner: &runner,
    }
    .remove("spike")
    .unwrap();
    assert_eq!(steps, ["detach-loop", "detach-loop", "remove-marker", "remove-link"]);
    let detaches: Vec<_> = runner
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("losetup -d"))
        .collect();
    assert_eq!(detaches, ["losetup -d /dev/loop1", "losetup -d /dev/loop3"]);
}

#[test]
fn remove_finds_the_loop_device_when_the_volumes_directory_is_behind_a_link() {
    // `losetup` prints the canonical path; the configured one goes through a link.
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let linked = tmp.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let mut paths = temp_paths(&tmp);
    paths.volumes_dir = linked;
    prepare_link_without_image(&paths);
    let canonical = real.canonicalize().unwrap();
    let runner = node_with_listing(format!("/dev/loop5 {}/spike.img (deleted)\n", canonical.display()));
    let steps = Reconciler {
        paths: &paths,
        runner: &runner,
    }
    .remove("spike")
    .unwrap();
    assert_eq!(steps, ["detach-loop", "remove-marker", "remove-link"]);
    assert_eq!(count(&runner, "losetup -d /dev/loop5"), 1);
}

#[test]
fn remove_stops_when_the_loop_devices_of_a_missing_image_cannot_be_listed() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_link_without_image(&paths);
    let runner = FakeRunner::new(|program, _| match program {
        "losetup" => out(1, "", "losetup: cannot open /dev/loop-control"),
        "drbdsetup" => out(0, "[\n]\n", ""),
        _ => out(0, "", ""),
    });
    let err = Reconciler {
        paths: &paths,
        runner: &runner,
    }
    .remove("spike")
    .unwrap_err();
    assert!(err.to_string().contains("cannot open /dev/loop-control"), "{err}");
    assert!(paths.ready_marker("spike").exists());
    assert!(fs::symlink_metadata(paths.device_link("spike")).is_ok());
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

// ---- remove_checked ----

/// A node with a full copy of `spike` on disk.
fn copy_on_disk() -> (TempDir, Paths, FakeRunner) {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    prepare_copy(&paths, true, true);
    (tmp, paths, fake_node())
}

#[test]
fn a_guard_that_refuses_runs_nothing_and_deletes_nothing() {
    let (_tmp, paths, runner) = copy_on_disk();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let err = reconciler
        .remove_checked("spike", &|| Err("a peer is Primary".to_string()))
        .unwrap_err();
    assert!(
        matches!(&err, ReconcileError::Refused { volume, reason } if volume == "spike" && reason == "a peer is Primary"),
        "{err}"
    );
    assert!(err.to_string().contains("a peer is Primary"), "{err}");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    for path in [
        paths.image("spike"),
        paths.ready_marker("spike"),
        paths.synced_marker("spike"),
        paths.res_file("spike"),
        paths.applied_res("spike"),
        paths.device_link("spike"),
    ] {
        assert!(path.symlink_metadata().is_ok(), "{} was deleted", path.display());
    }
}

#[test]
fn a_guard_that_passes_lets_remove_proceed() {
    let (_tmp, paths, runner) = copy_on_disk();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let steps = reconciler.remove_checked("spike", &|| Ok(())).unwrap();
    assert!(steps.contains(&"remove-file"), "{steps:?}");
    assert!(!paths.image("spike").exists());
}

#[test]
fn a_removal_that_requires_an_absent_resource_refuses_one_that_appeared_after_the_guard() {
    // The guard saw the resource not configured; the status read that decides
    // `down` shows it. Nothing is run beyond the reads and nothing is deleted.
    let (_tmp, paths, _) = copy_on_disk();
    let reads = Arc::new(Mutex::new(0));
    let counter = reads.clone();
    let runner = FakeRunner::new(move |program, _| match program {
        "drbdsetup" => {
            let mut n = counter.lock().unwrap();
            *n += 1;
            out(0, if *n == 1 { "[\n]\n" } else { THREE_UPTODATE }, "")
        }
        _ => out(0, "", ""),
    });
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let guard = || {
        // Stands for the status read of the agent's guard.
        let status = Drbd { runner: &runner }.status("spike").map_err(|e| e.to_string())?;
        assert!(status.is_none());
        Ok(())
    };
    let err = reconciler.remove_unconfigured("spike", &guard).unwrap_err();
    assert!(
        matches!(&err, ReconcileError::Refused { volume, reason } if volume == "spike" && reason.contains("configured")),
        "{err}"
    );
    assert_eq!(*reads.lock().unwrap(), 2);
    assert_eq!(count(&runner, "drbdadm"), 0);
    assert_eq!(count(&runner, "losetup"), 0);
    for path in [
        paths.image("spike"),
        paths.ready_marker("spike"),
        paths.res_file("spike"),
    ] {
        assert!(path.exists(), "{} was deleted", path.display());
    }
    assert!(paths.device_link("spike").symlink_metadata().is_ok());
}

#[test]
fn a_removal_that_requires_an_absent_resource_proceeds_when_it_is_absent() {
    let (_tmp, paths, runner) = copy_on_disk();
    let steps = Reconciler {
        paths: &paths,
        runner: &runner,
    }
    .remove_unconfigured("spike", &|| Ok(()))
    .unwrap();
    assert!(steps.contains(&"remove-file"), "{steps:?}");
    assert!(!steps.contains(&"down"), "{steps:?}");
}

#[test]
fn the_guard_runs_with_the_volume_lock_held() {
    let (_tmp, paths, runner) = copy_on_disk();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let seen_busy = Mutex::new(None);
    reconciler
        .remove_checked("spike", &|| {
            // The lock is taken without waiting: a second taker sees it held.
            *seen_busy.lock().unwrap() = Some(matches!(
                reconciler.lock_volume("spike"),
                Err(ReconcileError::Busy { .. })
            ));
            Ok(())
        })
        .unwrap();
    assert_eq!(*seen_busy.lock().unwrap(), Some(true));
}

#[test]
fn the_guard_does_not_run_when_the_volume_is_busy_or_the_name_is_invalid() {
    let (_tmp, paths, runner) = copy_on_disk();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let called = Mutex::new(0);
    let guard = || {
        *called.lock().unwrap() += 1;
        Ok(())
    };
    let held = reconciler.lock_volume("spike").unwrap();
    let err = reconciler.remove_checked("spike", &guard).unwrap_err();
    assert!(matches!(err, ReconcileError::Busy { .. }), "{err}");
    drop(held);
    let err = reconciler.remove_checked("../x", &guard).unwrap_err();
    assert!(matches!(err, ReconcileError::Spec(_)), "{err}");
    assert_eq!(*called.lock().unwrap(), 0);
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

// ---- the metadata decides, no marker ----

fn md_calls(runner: &FakeRunner) -> Vec<String> {
    runner.calls().into_iter().filter(|c| c.contains("-md")).collect()
}

/// Dumps recorded in experiment P10, see `src/drbd/tests/adm.rs`.
const P10_JUST_CREATED: &str = include_str!("../drbd/tests/fixtures/p10-1-just-created.txt");
const P10_USED: [(&str, &str); 3] = [
    ("4", include_str!("../drbd/tests/fixtures/p10-4-skip-uptodate.txt")),
    (
        "5",
        include_str!("../drbd/tests/fixtures/p10-5-filesystem-and-file.txt"),
    ),
    ("6", include_str!("../drbd/tests/fixtures/p10-6-full-resync.txt")),
];

/// Applies `spike` once on a fresh volumes directory where `dump-md` answers
/// with `dump_md`; returns the result, the `-md` calls and the paths.
fn apply_once(dump_md: Output) -> (Result<Vec<Action>, ReconcileError>, Vec<String>, Paths, TempDir) {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = node_with_dump_md(dump_md);
    let result = Reconciler {
        paths: &paths,
        runner: &runner,
    }
    .apply(&spec(), "lima-dv-1");
    (result, md_calls(&runner), paths, tmp)
}

#[test]
fn metadata_that_never_held_data_is_written_again() {
    let (result, calls, paths, _tmp) = apply_once(out(0, P10_JUST_CREATED, ""));
    result.unwrap();
    assert_eq!(
        calls,
        ["drbdadm dump-md spike", "drbdadm create-md --force --max-peers=7 spike"]
    );
    assert!(paths.ready_marker("spike").exists());
}

#[test]
fn used_metadata_stops_create_md_and_the_ready_marker() {
    for (state, dump) in P10_USED {
        let (result, calls, paths, _tmp) = apply_once(out(0, dump, ""));
        let err = result.unwrap_err();
        assert!(
            matches!(err, ReconcileError::MetadataPresent { .. }),
            "state {state}: {err}"
        );
        assert_eq!(calls, ["drbdadm dump-md spike"], "state {state}");
        assert!(!paths.ready_marker("spike").exists(), "state {state}");
        assert!(paths.image("spike").exists(), "state {state}");
    }
}

#[test]
fn a_dump_that_cannot_be_read_is_treated_as_used_metadata() {
    let truncated: String = P10_JUST_CREATED.lines().take(100).collect::<Vec<_>>().join("\n");
    for dump in [String::new(), "\n".to_string(), "garbage".to_string(), truncated] {
        let (result, calls, paths, _tmp) = apply_once(out(0, &dump, ""));
        let err = result.unwrap_err();
        assert!(matches!(err, ReconcileError::MetadataPresent { .. }), "{dump:?}: {err}");
        assert_eq!(calls, ["drbdadm dump-md spike"], "{dump:?}");
        assert!(!paths.ready_marker("spike").exists());
    }
}

#[test]
fn an_unclean_activity_log_is_used_metadata() {
    // P10 (2)-(6): `dump-md` after `up` and `down`; the metadata was attached.
    let stderr = "Found meta data is \"unclean\", please apply-al first\n";
    let both = format!("No valid meta data found\n{stderr}");
    for (name, stderr) in [("unclean", stderr.to_string()), ("both markers", both)] {
        let (result, calls, paths, _tmp) = apply_once(out(255, "", &stderr));
        let err = result.unwrap_err();
        assert!(matches!(err, ReconcileError::MetadataPresent { .. }), "{name}: {err}");
        assert_eq!(calls, ["drbdadm dump-md spike"], "{name}");
        assert!(!paths.ready_marker("spike").exists(), "{name}");
        assert!(paths.image("spike").exists(), "{name}");
    }
}

#[test]
fn metadata_attached_once_is_not_written_again() {
    // The dumps of P10 (2) and (3) are what `dump-md` prints after
    // `apply-al`; an agent never sees them, but if one did arrive, it is Used.
    for (state, dump) in [
        ("2", include_str!("../drbd/tests/fixtures/p10-2-up-down.txt")),
        (
            "3",
            include_str!("../drbd/tests/fixtures/p10-3-connected-inconsistent.txt"),
        ),
    ] {
        let (result, calls, paths, _tmp) = apply_once(out(0, dump, ""));
        let err = result.unwrap_err();
        assert!(
            matches!(err, ReconcileError::MetadataPresent { .. }),
            "state {state}: {err}"
        );
        assert_eq!(calls, ["drbdadm dump-md spike"], "state {state}");
        assert!(!paths.ready_marker("spike").exists(), "state {state}");
    }
}

#[test]
fn an_apply_interrupted_after_create_md_is_finished_by_the_next_one() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    // No metadata before `create-md`; the just-created dump after it.
    let runner = fake_node_with_dump_md(|created| {
        if created {
            out(0, P10_JUST_CREATED, "")
        } else {
            out(255, "", P7_NO_METADATA_STDERR)
        }
    });
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
    assert_eq!(count(&runner, "drbdadm create-md"), 1);

    fs::remove_dir(&blocker).unwrap();
    let done = reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert!(
        done.contains(&Action::MarkReady) && done.contains(&Action::Up),
        "{done:?}"
    );
    // The metadata of the first attempt was recognised as never used.
    assert_eq!(count(&runner, "drbdadm dump-md"), 2);
    assert_eq!(count(&runner, "drbdadm create-md"), 2);
    assert!(paths.ready_marker("spike").exists());
}

#[test]
fn a_ready_file_is_not_checked_and_not_written_again() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    let runner = fake_node_with(out(0, P10_JUST_CREATED, ""));
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    let (dumps, creates) = (count(&runner, "drbdadm dump-md"), count(&runner, "drbdadm create-md"));
    reconciler.apply(&spec(), "lima-dv-1").unwrap();
    assert_eq!(count(&runner, "drbdadm dump-md"), dumps);
    assert_eq!(count(&runner, "drbdadm create-md"), creates);
}

// ---- a leftover <volume>.img.creating of an intermediate build ----

#[test]
fn the_leftover_creating_file_sits_next_to_the_image() {
    let paths = Paths::default();
    assert_eq!(
        paths.leftover_creating_file("spike"),
        paths.volumes_dir.join("spike.img.creating")
    );
}

#[test]
fn a_leftover_creating_file_changes_no_decision() {
    // Whatever it contains, the check runs and the metadata decides. `None`
    // stands for the real `<dev>:<ino>` of the image, the value an earlier
    // build would have honoured.
    for content in [Some(""), Some("garbage"), Some("1:2"), None] {
        let tmp = TempDir::new().unwrap();
        let paths = temp_paths(&tmp);
        fs::create_dir_all(&paths.volumes_dir).unwrap();
        fs::write(paths.image("spike"), "").unwrap();
        let content = content.map(str::to_string).unwrap_or_else(|| {
            use std::os::unix::fs::MetadataExt;
            let md = fs::metadata(paths.image("spike")).unwrap();
            format!("{}:{}", md.dev(), md.ino())
        });
        fs::write(paths.leftover_creating_file("spike"), &content).unwrap();
        let runner = node_with_dump_md(out(0, P10_USED[1].1, ""));
        let err = Reconciler {
            paths: &paths,
            runner: &runner,
        }
        .apply(&spec(), "lima-dv-1")
        .unwrap_err();
        assert!(
            matches!(err, ReconcileError::MetadataPresent { .. }),
            "{content:?}: {err}"
        );
        assert_eq!(md_calls(&runner), ["drbdadm dump-md spike"], "{content:?}");
    }
}

#[test]
fn remove_deletes_a_leftover_creating_file() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.leftover_creating_file("spike"), "").unwrap();
    fs::write(paths.image("spike"), "").unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let steps = reconciler.remove("spike").unwrap();
    assert!(steps.contains(&"remove-leftover-creating-file"), "{steps:?}");
    assert!(!paths.leftover_creating_file("spike").exists());
    assert!(!paths.image("spike").exists());
}

#[test]
fn clearing_stale_markers_deletes_a_leftover_creating_file() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    fs::write(paths.ready_marker("spike"), "").unwrap();
    fs::write(paths.leftover_creating_file("spike"), "").unwrap();
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    // The leftover alone is not a stale marker: no decision depends on it.
    assert!(reconciler.observe(&spec(), "lima-dv-1").unwrap().stale_markers);
    fs::remove_file(paths.ready_marker("spike")).unwrap();
    assert!(!reconciler.observe(&spec(), "lima-dv-1").unwrap().stale_markers);
    fs::write(paths.ready_marker("spike"), "").unwrap();
    reconciler
        .execute(&spec(), "lima-dv-1", &Action::ClearStaleMarkers, &mut None)
        .unwrap();
    assert!(!paths.leftover_creating_file("spike").exists());
    assert!(!paths.ready_marker("spike").exists());
}

// ---- messages and the volumes directory ----

#[test]
fn the_metadata_present_message_does_not_claim_that_nothing_changed() {
    let text = ReconcileError::MetadataPresent { volume: "spike".into() }.to_string();
    assert!(!text.contains("nothing was changed"), "{text}");
    assert!(text.contains("the metadata was not touched"), "{text}");
    assert!(!text.contains("apply-al"), "{text}");
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
