use std::fs;
use std::os::unix::fs::symlink;

use tempfile::TempDir;

use super::*;
use crate::tests::temp_paths;

fn dirs(tmp: &TempDir) -> Paths {
    let paths = temp_paths(tmp);
    for dir in [&paths.volumes_dir, &paths.res_dir, &paths.dev_dir] {
        fs::create_dir_all(dir).unwrap();
    }
    paths
}

fn touch(path: std::path::PathBuf) {
    fs::write(path, "").unwrap();
}

fn names(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// ---- local_volumes ----

#[test]
fn every_kind_of_trace_makes_the_volume_local() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("with-image"));
    touch(paths.res_file("only-res"));
    touch(paths.applied_res("only-applied"));
    symlink("/dev/loop7", paths.device_link("only-link")).unwrap();

    let found = local_volumes(&paths).unwrap();
    assert_eq!(found, names(&["only-applied", "only-link", "only-res", "with-image"]));
}

#[test]
fn a_volume_is_listed_once_however_many_traces_it_has() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    touch(paths.res_file("vol"));
    touch(paths.applied_res("vol"));
    symlink("/dev/loop7", paths.device_link("vol")).unwrap();
    assert_eq!(local_volumes(&paths).unwrap(), names(&["vol"]));
}

#[test]
fn markers_and_lock_files_without_an_image_do_not_make_a_volume_local() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.ready_marker("only-ready"));
    touch(paths.synced_marker("only-synced"));
    touch(paths.leftover_creating_file("only-creating"));
    touch(paths.lock_file("only-lock"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
}

#[test]
fn names_that_are_not_valid_volume_names_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.volumes_dir.join("Upper.img"));
    touch(paths.volumes_dir.join("with space.img"));
    touch(paths.volumes_dir.join("-lead.img"));
    touch(paths.volumes_dir.join("all.img"));
    touch(paths.volumes_dir.join(".img"));
    touch(paths.res_dir.join("bad_name.res"));
    symlink("/dev/loop7", paths.dev_dir.join("Bad")).unwrap();
    touch(paths.image("good"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&["good"]));
}

#[test]
fn files_that_are_not_volume_files_are_ignored() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.res_dir.join("global_common.conf"));
    touch(paths.res_dir.join("drbd.res.bak"));
    touch(paths.volumes_dir.join("notes.txt"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
}

#[test]
fn missing_resource_and_device_directories_mean_nothing_there() {
    let tmp = TempDir::new().unwrap();
    let paths = temp_paths(&tmp);
    fs::create_dir_all(&paths.volumes_dir).unwrap();
    touch(paths.image("vol"));
    assert_eq!(local_volumes(&paths).unwrap(), names(&["vol"]));
}

// ---- local ----

#[test]
fn local_reports_the_image_and_the_synced_marker() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    let state = local(&paths, "vol", None).unwrap();
    assert_eq!(
        state,
        Local {
            image: true,
            traces: false,
            synced: false
        }
    );
    touch(paths.synced_marker("vol"));
    let state = local(&paths, "vol", None).unwrap();
    assert!(state.image && state.synced);
}

#[test]
fn local_counts_resource_files_and_links_as_traces() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.res_file("a"));
    touch(paths.applied_res("b"));
    symlink("/dev/does-not-exist", paths.device_link("c")).unwrap();
    for volume in ["a", "b", "c"] {
        let state = local(&paths, volume, None).unwrap();
        assert_eq!(
            state,
            Local {
                image: false,
                traces: true,
                synced: false
            },
            "{volume}"
        );
    }
}

#[test]
fn local_does_not_count_markers_or_the_lock_file_as_traces() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.ready_marker("vol"));
    touch(paths.synced_marker("vol"));
    touch(paths.leftover_creating_file("vol"));
    touch(paths.lock_file("vol"));
    let state = local(&paths, "vol", None).unwrap();
    assert_eq!(
        state,
        Local {
            image: false,
            traces: false,
            synced: true
        }
    );
}

#[test]
fn local_of_an_unknown_volume_is_empty() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    assert_eq!(
        local(&paths, "vol", None).unwrap(),
        Local {
            image: false,
            traces: false,
            synced: false
        }
    );
}

#[test]
fn local_refuses_a_name_that_is_not_a_valid_volume_name() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    for bad in ["../x", "A", "", "all", "a/b"] {
        let err = local(&paths, bad, None).unwrap_err();
        assert!(matches!(err, ReconcileError::Spec(_)), "{bad:?}: {err}");
    }
}

#[test]
fn an_unreadable_path_is_an_error_not_absence() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    // A regular file where the volumes directory should be: `stat` of
    // `<file>/vol.img` fails with ENOTDIR, which is not "no such file".
    fs::remove_dir_all(&paths.volumes_dir).unwrap();
    touch(paths.volumes_dir.clone());
    let err = local(&paths, "vol", None).unwrap_err();
    assert!(matches!(err, ReconcileError::Io { .. }), "{err}");
}

// ---- P2: only what ambar wrote is a trace ----

#[test]
fn a_name_with_no_file_of_ambars_is_neither_a_local_volume_nor_a_trace() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    // The kernel has `foreign` configured; ambar never wrote a file for it.
    assert_eq!(local_volumes(&paths).unwrap(), names(&[]));
    let state = local(&paths, "foreign", None).unwrap();
    assert!(!state.traces && !state.image, "{state:?}");
}

// ---- file_state ----

#[test]
fn file_state_tells_absent_pending_and_ready_apart() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Absent);
    // Markers without the file do not make one.
    touch(paths.ready_marker("vol"));
    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Absent);
    fs::remove_file(paths.ready_marker("vol")).unwrap();
    touch(paths.image("vol"));
    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Pending);
    touch(paths.synced_marker("vol"));
    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Pending);
    touch(paths.ready_marker("vol"));
    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Ready);
}

#[test]
fn file_state_refuses_a_name_that_is_not_a_valid_volume_name() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    assert!(file_state(&paths, "../x", None).is_err());
}

// ---- synced_marker_present, loops_holding ----

#[test]
fn the_synced_marker_is_read_from_its_path() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    assert!(!synced_marker_present(&paths, "vol").unwrap());
    touch(paths.synced_marker("vol"));
    assert!(synced_marker_present(&paths, "vol").unwrap());
    assert!(synced_marker_present(&paths, "../x").is_err());
}

#[test]
fn a_synced_marker_that_cannot_be_inspected_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    symlink(paths.synced_marker("vol"), paths.synced_marker("vol")).unwrap();
    assert!(synced_marker_present(&paths, "vol").is_err());
}

fn listing(stdout: &'static str) -> crate::tests::FakeRunner {
    crate::tests::FakeRunner::new(move |_, _| crate::tests::out(0, stdout, ""))
}

#[test]
fn loops_holding_finds_a_deleted_file_by_its_path() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    let image = paths.image("vol");
    let text = format!("/dev/loop1 /other/a.img\n/dev/loop7 {} (deleted)\n", image.display());
    let runner = crate::tests::FakeRunner::new(move |_, _| crate::tests::out(0, &text, ""));
    assert_eq!(loops_holding(&runner, &image).unwrap(), ["/dev/loop7"]);
    assert_eq!(runner.calls(), ["losetup --list --noheadings --output NAME,BACK-FILE"]);
}

#[test]
fn loops_holding_finds_a_file_that_exists_and_all_its_devices() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    let image = paths.image("vol");
    let text = format!("/dev/loop2 {0}\n/dev/loop3 {0}\n", image.display());
    let runner = crate::tests::FakeRunner::new(move |_, _| crate::tests::out(0, &text, ""));
    assert_eq!(loops_holding(&runner, &image).unwrap(), ["/dev/loop2", "/dev/loop3"]);
}

#[test]
fn loops_holding_compares_the_resolved_path_too() {
    // `losetup` stores the canonical path; the volumes directory may be a link.
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    symlink(&real, &link).unwrap();
    let canonical = real.canonicalize().unwrap();
    let text = format!("/dev/loop4 {}/vol.img (deleted)\n", canonical.display());
    let runner = crate::tests::FakeRunner::new(move |_, _| crate::tests::out(0, &text, ""));
    assert_eq!(loops_holding(&runner, &link.join("vol.img")).unwrap(), ["/dev/loop4"]);
}

#[test]
fn loops_holding_ignores_other_files_and_a_longer_name() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    let image = paths.image("vol");
    let other = format!(
        "/dev/loop5 {}.bak\n/dev/loop6 {} (deleted) x\n",
        image.display(),
        image.display()
    );
    let runner = crate::tests::FakeRunner::new(move |_, _| crate::tests::out(0, &other, ""));
    assert!(loops_holding(&runner, &image).unwrap().is_empty());
    assert!(loops_holding(&listing(""), &image).unwrap().is_empty());
}

#[test]
fn loops_holding_reports_a_failed_listing_and_an_unresolvable_directory() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    let failing = crate::tests::FakeRunner::new(|_, _| crate::tests::out(1, "", "no permission"));
    let err = loops_holding(&failing, &paths.image("vol")).unwrap_err();
    assert!(err.contains("no permission"), "{err}");
    let missing = tmp.path().join("nowhere").join("vol.img");
    assert!(loops_holding(&listing(""), &missing).is_err());
}

// ---- LVM volume_group tests ----

#[test]
fn local_with_volume_group_ignores_image_file() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    touch(paths.synced_marker("vol"));

    // Without volume_group, the .img file is detected
    let state_no_vg = local(&paths, "vol", None).unwrap();
    assert!(state_no_vg.image);

    // With volume_group, it checks /dev/k3s-vg/vol which does not exist, so image is false
    let state_with_vg = local(&paths, "vol", Some("k3s-vg")).unwrap();
    assert!(!state_with_vg.image);
    assert!(state_with_vg.synced);
}

#[test]
fn file_state_with_volume_group_ignores_image_file() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    touch(paths.image("vol"));
    touch(paths.ready_marker("vol"));

    assert_eq!(file_state(&paths, "vol", None).unwrap(), FileState::Ready);
    // With volume_group, /dev/k3s-vg/vol is absent, so file_state is Absent despite .img
    assert_eq!(file_state(&paths, "vol", Some("k3s-vg")).unwrap(), FileState::Absent);
}

#[test]
fn local_and_file_state_detect_lvm_device_when_present() {
    let tmp = TempDir::new().unwrap();
    let paths = dirs(&tmp);
    let dev_dir = Path::new("/dev/ambar-test-vg");
    if fs::create_dir_all(dev_dir).is_ok() {
        let dev_file = dev_dir.join("vol");
        touch(dev_file.clone());
        touch(paths.ready_marker("vol"));

        let state = local(&paths, "vol", Some("ambar-test-vg")).unwrap();
        assert!(state.image);

        assert_eq!(
            file_state(&paths, "vol", Some("ambar-test-vg")).unwrap(),
            FileState::Ready
        );

        let _ = fs::remove_file(dev_file);
        let _ = fs::remove_dir(dev_dir);
    }
}
