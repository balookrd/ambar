use std::cell::Cell;
use std::fs;
use std::rc::Rc;

use tempfile::TempDir;

use super::*;
use crate::backing::backing_file_bytes;
use crate::drbd::status::{self, DiskState};
use crate::tests::{FakeRunner, out};

const SPEC: &str = r#"{
  "name": "spike",
  "sizeBytes": 1073741824,
  "protocol": "C",
  "minor": 1000,
  "port": 7000,
  "maxPeers": 7,
  "nodes": [
    {"name": "lima-dv-1", "address": "192.168.104.1", "nodeId": 0, "diskful": true},
    {"name": "lima-dv-2", "address": "192.168.104.3", "nodeId": 1, "diskful": true},
    {"name": "lima-dv-3", "address": "192.168.104.4", "nodeId": 2, "diskful": false}
  ]
}"#;

const THREE_INCONSISTENT: &str = include_str!("../../stand/spike/samples/three-inconsistent.json");
const THREE_UPTODATE: &str = include_str!("../../stand/spike/samples/three-uptodate.json");

fn spec() -> VolumeSpec {
    VolumeSpec::from_json(SPEC).unwrap()
}

fn status_of(json: &str) -> Option<ResourceStatus> {
    status::parse(json).unwrap().into_iter().next()
}

fn steady() -> Observed {
    Observed {
        file: FileState::Ready,
        loop_device: Some("/dev/loop3".into()),
        link_target: Some("/dev/loop3".into()),
        res_matches: true,
        synced: true,
        status: status_of(THREE_UPTODATE),
    }
}

#[test]
fn a_new_copy_is_created_initialised_and_brought_up() {
    let obs = Observed {
        file: FileState::Absent,
        loop_device: None,
        link_target: None,
        res_matches: false,
        synced: false,
        status: None,
    };
    let bytes = backing_file_bytes(1_073_741_824, 7);
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [
            Action::CreateFile { bytes },
            Action::AttachLoop,
            Action::LinkDevice,
            Action::WriteRes,
            Action::CreateMd,
            Action::MarkReady,
            Action::Up,
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
        [Action::AttachLoop, Action::LinkDevice, Action::Up]
    );
}

#[test]
fn an_interrupted_initialisation_is_finished() {
    let obs = Observed {
        file: FileState::Pending,
        status: None,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::CreateMd, Action::MarkReady, Action::Up]
    );
}

#[test]
fn metadata_is_never_created_under_a_configured_resource() {
    let obs = Observed {
        file: FileState::Pending,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs),
        Err(PlanError::UninitialisedFileWithLiveResource { volume: "spike".into() })
    );
}

#[test]
fn a_node_without_a_disk_only_needs_the_resource() {
    let obs = Observed {
        file: FileState::Absent,
        loop_device: None,
        link_target: None,
        res_matches: false,
        synced: false,
        status: None,
    };
    assert_eq!(
        plan(&spec(), "lima-dv-3", &obs).unwrap(),
        [Action::WriteRes, Action::Up]
    );
}

#[test]
fn a_changed_description_is_applied_with_adjust() {
    let obs = Observed {
        res_matches: false,
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::WriteRes, Action::Adjust]
    );
    assert_eq!(
        plan(&spec(), "lima-dv-3", &obs).unwrap(),
        [Action::WriteRes, Action::Adjust]
    );
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
    assert_eq!(plan(&spec(), "lima-dv-1", &steady()).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &steady()).unwrap(), []);
}

#[test]
fn only_the_lowest_diskful_node_skips_the_initial_sync() {
    let obs = Observed {
        synced: false,
        status: status_of(THREE_INCONSISTENT),
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &obs).unwrap(),
        [Action::SkipInitialSync, Action::MarkSynced]
    );
    assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), []);
    assert_eq!(plan(&spec(), "lima-dv-3", &obs).unwrap(), []);
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

fn with_local_disk(disk: DiskState) -> Option<ResourceStatus> {
    let mut status = status_of(THREE_UPTODATE).unwrap();
    status.devices.iter_mut().find(|d| d.volume == 0).unwrap().disk_state = disk;
    Some(status)
}

#[test]
fn the_leader_skips_the_initial_sync_only_while_the_synced_marker_is_absent() {
    let fresh = Observed {
        synced: false,
        status: status_of(THREE_INCONSISTENT),
        ..steady()
    };
    assert_eq!(
        plan(&spec(), "lima-dv-1", &fresh).unwrap(),
        [Action::SkipInitialSync, Action::MarkSynced]
    );
    let marked = Observed { synced: true, ..fresh };
    assert_eq!(plan(&spec(), "lima-dv-1", &marked).unwrap(), []);
}

#[test]
fn a_copy_that_was_seen_up_to_date_gets_the_synced_marker() {
    let unmarked = Observed {
        synced: false,
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-2", &unmarked).unwrap(), [Action::MarkSynced]);
    assert_eq!(plan(&spec(), "lima-dv-1", &unmarked).unwrap(), [Action::MarkSynced]);
    assert_eq!(plan(&spec(), "lima-dv-2", &steady()).unwrap(), []);
}

#[test]
fn a_node_without_a_disk_never_gets_the_synced_marker() {
    let unmarked = Observed {
        synced: false,
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &unmarked).unwrap(), []);
    let fresh = Observed {
        synced: false,
        status: status_of(THREE_INCONSISTENT),
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-3", &fresh).unwrap(), []);
}

#[test]
fn a_copy_with_a_detached_disk_is_adjusted() {
    let obs = Observed {
        status: with_local_disk(DiskState::Diskless),
        ..steady()
    };
    assert_eq!(plan(&spec(), "lima-dv-1", &obs).unwrap(), [Action::Adjust]);
    assert_eq!(plan(&spec(), "lima-dv-2", &obs).unwrap(), [Action::Adjust]);
    assert_eq!(plan(&spec(), "lima-dv-3", &obs).unwrap(), []);
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
/// resource appear in `drbdsetup status`.
fn fake_node() -> FakeRunner {
    let attached = Rc::new(Cell::new(false));
    let up = Rc::new(Cell::new(false));
    FakeRunner::new(move |program, args| match (program, args) {
        ("losetup", ["-j", _]) if attached.get() => out(0, "/dev/loop7: [0048]:131 (image)\n", ""),
        ("losetup", ["--find", ..]) => {
            attached.set(true);
            out(0, "/dev/loop7\n", "")
        }
        ("losetup", ["-d", _]) => {
            attached.set(false);
            out(0, "", "")
        }
        ("drbdadm", ["up", _]) => {
            up.set(true);
            out(0, "", "")
        }
        ("drbdadm", ["down", _]) => {
            up.set(false);
            out(0, "", "")
        }
        ("drbdsetup", _) if up.get() => out(0, THREE_INCONSISTENT, ""),
        ("drbdsetup", _) => out(0, "[\n]\n\n", ""),
        _ => out(0, "", ""),
    })
}

fn temp_paths(tmp: &TempDir) -> Paths {
    Paths {
        volumes_dir: tmp.path().join("volumes"),
        res_dir: tmp.path().join("res"),
        dev_dir: tmp.path().join("dev"),
    }
}

#[test]
fn apply_creates_a_copy_then_skips_the_initial_sync_and_remove_undoes_it() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    let runner = fake_node();
    let reconciler = Reconciler {
        paths: &paths,
        runner: &runner,
    };
    let spec = spec();

    let done = reconciler.apply(&spec, "lima-dv-1").unwrap();
    assert_eq!(done.last(), Some(&Action::MarkSynced));
    assert_eq!(done.len(), 9);
    assert_eq!(done[done.len() - 2], Action::SkipInitialSync);

    let image = paths.image("spike");
    assert_eq!(
        fs::metadata(&image).unwrap().len(),
        backing_file_bytes(spec.size_bytes, spec.max_peers)
    );
    assert!(paths.ready_marker("spike").exists());
    assert!(paths.synced_marker("spike").exists());
    assert_eq!(
        backing::link_target(&paths.device_link("spike")).as_deref(),
        Some("/dev/loop7")
    );
    let res = fs::read_to_string(paths.res_file("spike")).unwrap();
    assert!(
        res.contains(&format!("disk {};", paths.device_link("spike").display())),
        "{res}"
    );

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
            "drbdadm create-md --force --max-peers=7 spike",
            "drbdadm up spike",
            "drbdadm new-current-uuid --clear-bitmap spike/0",
        ]
    );

    // A second apply right away has nothing new to create.
    let again = reconciler.apply(&spec, "lima-dv-1").unwrap();
    assert!(
        !again
            .iter()
            .any(|a| matches!(a, Action::CreateFile { .. } | Action::CreateMd | Action::Up)),
        "{again:?}"
    );

    let steps = reconciler.remove("spike").unwrap();
    assert_eq!(
        steps,
        [
            "down",
            "detach-loop",
            "remove-link",
            "remove-res",
            "remove-marker",
            "remove-synced-marker",
            "remove-file"
        ]
    );
    assert!(!image.exists());
    assert!(!paths.synced_marker("spike").exists());
    assert_eq!(reconciler.remove("spike").unwrap(), Vec::<&str>::new());
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
